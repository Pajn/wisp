use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use embers_core::{BufferId, SessionId, new_request_id};
use embers_protocol::{
    BufferLocation, BufferLocationResponse, BufferRequest, ClientMessage, ClientRecord,
    ClientRequest, NodeRecord, ServerResponse, SessionRequest, SessionSnapshot,
};
use embers_test_support::{TestConnection, TestServer};
use wisp_embers::{EmbersClient, EmbersJoinPlacement, EmbersLaunchEnv};

fn unique_test_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    std::env::temp_dir().join(format!("wisp-embers-{name}-{nonce}"))
}

fn shell_sleep_command(seconds: u64) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-lc".to_string(),
        format!("sleep {seconds}"),
    ]
}

async fn session_id_by_name(connection: &mut TestConnection, name: &str) -> SessionId {
    match connection
        .request(&ClientMessage::Session(SessionRequest::List {
            request_id: new_request_id(),
        }))
        .await
        .expect("list sessions succeeds")
    {
        ServerResponse::Sessions(response) => {
            response
                .sessions
                .into_iter()
                .find(|session| session.name == name)
                .expect("session exists")
                .id
        }
        other => panic!("expected sessions response, got {other:?}"),
    }
}

async fn buffer_location(connection: &mut TestConnection, buffer_id: BufferId) -> BufferLocation {
    match connection
        .request(&ClientMessage::Buffer(BufferRequest::GetLocation {
            request_id: new_request_id(),
            buffer_id,
        }))
        .await
        .expect("buffer location succeeds")
    {
        ServerResponse::BufferLocation(BufferLocationResponse { location, .. }) => location,
        other => panic!("expected buffer location response, got {other:?}"),
    }
}

async fn own_client(connection: &mut TestConnection) -> ClientRecord {
    match connection
        .request(&ClientMessage::Client(ClientRequest::Get {
            request_id: new_request_id(),
            client_id: None,
        }))
        .await
        .expect("get client succeeds")
    {
        ServerResponse::Client(response) => response.client,
        other => panic!("expected client response, got {other:?}"),
    }
}

async fn client_record(connection: &mut TestConnection, client_id: u64) -> ClientRecord {
    list_clients(connection)
        .await
        .into_iter()
        .find(|client| client.id == client_id)
        .expect("client exists")
}

async fn list_clients(connection: &mut TestConnection) -> Vec<ClientRecord> {
    match connection
        .request(&ClientMessage::Client(ClientRequest::List {
            request_id: new_request_id(),
        }))
        .await
        .expect("list clients succeeds")
    {
        ServerResponse::Clients(response) => response.clients,
        other => panic!("expected clients response, got {other:?}"),
    }
}

/// Creates a session holding one shell buffer, as `embers` does for a new
/// session, and returns the session and that buffer.
async fn create_session_with_buffer(
    connection: &mut TestConnection,
    name: &str,
) -> (SessionId, BufferId) {
    create_session_running(connection, name, shell_sleep_command(30)).await
}

/// Like [`create_session_with_buffer`], with the buffer running `command`.
async fn create_session_running(
    connection: &mut TestConnection,
    name: &str,
    command: Vec<String>,
) -> (SessionId, BufferId) {
    let session_id = match connection
        .request(&ClientMessage::Session(SessionRequest::Create {
            request_id: new_request_id(),
            name: name.to_string(),
        }))
        .await
        .expect("create session succeeds")
    {
        ServerResponse::SessionSnapshot(response) => response.snapshot.session.id,
        other => panic!("expected session snapshot, got {other:?}"),
    };
    let buffer_id = match connection
        .request(&ClientMessage::Buffer(BufferRequest::Create {
            request_id: new_request_id(),
            title: Some(name.to_string()),
            command,
            cwd: None,
            env: BTreeMap::new(),
            size: None,
            session_id: Some(session_id),
            host: None,
        }))
        .await
        .expect("create buffer succeeds")
    {
        ServerResponse::Buffer(response) => response.buffer.id,
        other => panic!("expected buffer response, got {other:?}"),
    };
    match connection
        .request(&ClientMessage::Session(SessionRequest::AddRootTab {
            request_id: new_request_id(),
            session_id,
            title: name.to_string(),
            buffer_id: Some(buffer_id),
            child_node_id: None,
        }))
        .await
        .expect("add root tab succeeds")
    {
        ServerResponse::SessionSnapshot(_) => {}
        other => panic!("expected session snapshot, got {other:?}"),
    }
    (session_id, buffer_id)
}

/// Connects a stand-in for a user's terminal client and attaches it to
/// `session_id`, returning the connection (the client lives as long as it
/// does) and its client id.
async fn attach_user_client(server: &TestServer, session_id: SessionId) -> (TestConnection, u64) {
    let mut connection = TestConnection::connect(server.socket_path())
        .await
        .expect("connect user client");
    match connection
        .request(&ClientMessage::Client(ClientRequest::Switch {
            request_id: new_request_id(),
            client_id: None,
            session_id,
            node_id: None,
        }))
        .await
        .expect("switch user client")
    {
        ServerResponse::Client(response) => {
            let client_id = response.client.id;
            (connection, client_id)
        }
        other => panic!("expected client response, got {other:?}"),
    }
}

fn launched_by_client(client_id: u64) -> EmbersLaunchEnv {
    EmbersLaunchEnv {
        client: Some(client_id.to_string()),
        buffer: None,
    }
}

fn parse_buffer_id(buffer_id: &str) -> BufferId {
    BufferId(buffer_id.parse().expect("buffer id should parse"))
}

fn make_session_dir(path: &Path) {
    fs::create_dir_all(path).expect("session directory");
}

#[test]
fn creates_floating_window_for_current_session_buffer() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let workspace = unique_test_dir("floating");
    make_session_dir(&workspace);
    let mut user = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect user client");
    let user_id = runtime.block_on(own_client(&mut user)).id;
    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), launched_by_client(user_id))
            .expect("create embers client");
    client
        .create_or_switch_session("alpha", &workspace)
        .expect("create current session");
    assert_eq!(
        client.current_session_name().expect("read current session"),
        Some("alpha".to_string())
    );

    let viewport = client
        .current_session_viewport_size()
        .expect("read viewport")
        .expect("viewport should exist");
    assert!(viewport.0 >= 40);
    assert!(viewport.1 >= 12);

    let buffer_id = client
        .create_buffer(
            &shell_sleep_command(30),
            "wisp float test",
            Some(&workspace),
            &BTreeMap::new(),
        )
        .expect("create floating buffer");
    client
        .create_floating_for_buffer_in_current_session(
            &buffer_id,
            Some("Wisp Float"),
            40,
            12,
            true,
            true,
        )
        .expect("create floating window");
    assert_eq!(
        client.focused_buffer_id().expect("read focused buffer"),
        Some(buffer_id.clone())
    );

    let mut connection = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect protocol client");
    let alpha_id = runtime.block_on(session_id_by_name(&mut connection, "alpha"));
    let snapshot = runtime
        .block_on(connection.session_snapshot(alpha_id))
        .expect("read alpha snapshot");
    assert_eq!(snapshot.floating.len(), 1);
    let floating = &snapshot.floating[0];
    assert_eq!(floating.title.as_deref(), Some("Wisp Float"));
    assert_eq!(floating.geometry.width, 40);
    assert_eq!(floating.geometry.height, 12);
    assert!(floating.focused);
    assert!(floating.close_on_empty);

    let location = runtime.block_on(buffer_location(
        &mut connection,
        parse_buffer_id(&buffer_id),
    ));
    assert_eq!(location.session_id(), Some(alpha_id));
    assert_eq!(location.floating_id(), Some(floating.id));
    assert!(location.node_id().is_some());

    let _ = fs::remove_dir_all(&workspace);
    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

#[test]
fn joins_moves_and_detaches_sidebar_buffer_at_session_root() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let alpha_dir = unique_test_dir("alpha");
    let beta_dir = unique_test_dir("beta");
    make_session_dir(&alpha_dir);
    make_session_dir(&beta_dir);
    let mut user = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect user client");
    let user_id = runtime.block_on(own_client(&mut user)).id;
    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), launched_by_client(user_id))
            .expect("create embers client");
    client
        .create_or_switch_session("alpha", &alpha_dir)
        .expect("create alpha session");
    let buffer_id = client
        .create_buffer(
            &shell_sleep_command(30),
            "wisp sidebar test",
            Some(&alpha_dir),
            &BTreeMap::new(),
        )
        .expect("create sidebar buffer");

    let joined_session = client
        .join_buffer_to_current_session_root(&buffer_id, EmbersJoinPlacement::Left, Some(36), true)
        .expect("join buffer to alpha root");
    assert_eq!(joined_session, "alpha");
    assert_eq!(
        client
            .focused_buffer_id()
            .expect("read focused alpha buffer"),
        Some(buffer_id.clone())
    );

    let mut connection = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect protocol client");
    let alpha_id = runtime.block_on(session_id_by_name(&mut connection, "alpha"));
    let alpha_snapshot = runtime
        .block_on(connection.session_snapshot(alpha_id))
        .expect("alpha snapshot");
    let alpha_root = window_root(&alpha_snapshot).expect("alpha window root");
    let alpha_split = alpha_root
        .split
        .as_ref()
        .expect("window root should become split");
    assert_eq!(alpha_split.sizes.first().copied(), Some(36));
    assert_eq!(alpha_split.child_ids.len(), 2);

    let alpha_location = runtime.block_on(buffer_location(
        &mut connection,
        parse_buffer_id(&buffer_id),
    ));
    assert_eq!(alpha_location.session_id(), Some(alpha_id));
    assert!(alpha_location.node_id().is_some());
    assert!(alpha_location.floating_id().is_none());

    client
        .create_or_switch_session("beta", &beta_dir)
        .expect("create beta session");
    assert_eq!(
        client.current_session_name().expect("read beta session"),
        Some("beta".to_string())
    );

    let moved_session = client
        .join_buffer_to_current_session_root(&buffer_id, EmbersJoinPlacement::Left, Some(36), true)
        .expect("move buffer to beta root");
    assert_eq!(moved_session, "beta");
    assert_eq!(
        client
            .focused_buffer_id()
            .expect("read focused beta buffer"),
        Some(buffer_id.clone())
    );

    let beta_id = runtime.block_on(session_id_by_name(&mut connection, "beta"));
    let moved_location = runtime.block_on(buffer_location(
        &mut connection,
        parse_buffer_id(&buffer_id),
    ));
    assert_eq!(moved_location.session_id(), Some(beta_id));
    assert!(moved_location.node_id().is_some());
    assert!(moved_location.floating_id().is_none());

    let beta_snapshot = runtime
        .block_on(connection.session_snapshot(beta_id))
        .expect("beta snapshot");
    let beta_root = window_root(&beta_snapshot).expect("beta window root");
    let beta_split = beta_root
        .split
        .as_ref()
        .expect("beta window root should become split");
    assert_eq!(beta_split.sizes.first().copied(), Some(36));
    assert_eq!(beta_split.child_ids.len(), 2);

    client
        .detach_buffer(&buffer_id)
        .expect("detach sidebar buffer");
    let detached_location = runtime.block_on(buffer_location(
        &mut connection,
        parse_buffer_id(&buffer_id),
    ));
    assert_eq!(detached_location.session_id(), None);
    assert_eq!(detached_location.node_id(), None);
    assert_eq!(detached_location.floating_id(), None);

    let _ = fs::remove_dir_all(&alpha_dir);
    let _ = fs::remove_dir_all(&beta_dir);
    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

#[test]
fn embers_client_env_targets_the_launching_clients_session() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let mut setup = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect setup client");
    let (alpha_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "alpha"));
    let (beta_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "beta"));
    let (_user, user_id) = runtime.block_on(attach_user_client(&server, alpha_id));
    // A second attached client means the single-client fallback cannot be
    // what picks alpha.
    let (_other, _) = runtime.block_on(attach_user_client(&server, beta_id));

    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), launched_by_client(user_id))
            .expect("create embers client");

    assert_eq!(client.target_client_id().expect("resolve target"), user_id);
    assert_eq!(
        client.current_session_name().expect("read current session"),
        Some("alpha".to_string())
    );
    assert!(
        client
            .current_session_viewport_size()
            .expect("read viewport")
            .is_some()
    );
    let snapshot = client.snapshot().expect("snapshot");
    assert_eq!(snapshot.context.client_id, Some(user_id.to_string()));
    assert_eq!(
        snapshot.context.current_session_name,
        Some("alpha".to_string())
    );

    let buffer_id = client
        .create_buffer(
            &shell_sleep_command(30),
            "wisp popup test",
            None,
            &BTreeMap::new(),
        )
        .expect("create popup buffer");
    client
        .create_floating_for_buffer_in_current_session(&buffer_id, None, 40, 12, true, true)
        .expect("create floating window");
    let location = runtime.block_on(buffer_location(&mut setup, parse_buffer_id(&buffer_id)));
    assert_eq!(location.session_id(), Some(alpha_id));

    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

#[test]
fn embers_buffer_env_targets_the_client_showing_that_buffer() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let mut setup = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect setup client");
    let (alpha_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "alpha"));
    let (beta_id, beta_buffer) = runtime.block_on(create_session_with_buffer(&mut setup, "beta"));
    let (_alpha_user, _) = runtime.block_on(attach_user_client(&server, alpha_id));
    let (_first_beta_user, first_beta_id) = runtime.block_on(attach_user_client(&server, beta_id));
    let (_second_beta_user, second_beta_id) =
        runtime.block_on(attach_user_client(&server, beta_id));
    assert!(first_beta_id < second_beta_id);

    // A stale EMBERS_CLIENT (that client has gone) falls through to the buffer.
    let client = EmbersClient::connect_with_launch_env(
        server.socket_path(),
        EmbersLaunchEnv {
            client: Some("999999".to_string()),
            buffer: Some(beta_buffer.to_string()),
        },
    )
    .expect("create embers client");

    // Both beta clients show the buffer; the lowest client id wins.
    assert_eq!(
        client.target_client_id().expect("resolve target"),
        first_beta_id
    );
    assert_eq!(
        client.current_session_name().expect("read current session"),
        Some("beta".to_string())
    );

    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

#[test]
fn single_attached_client_is_the_fallback_target() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let mut setup = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect setup client");
    let (alpha_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "alpha"));
    let (_user, user_id) = runtime.block_on(attach_user_client(&server, alpha_id));

    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), EmbersLaunchEnv::default())
            .expect("create embers client");

    assert_eq!(client.target_client_id().expect("resolve target"), user_id);
    assert_eq!(
        client.current_session_name().expect("read current session"),
        Some("alpha".to_string())
    );

    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

#[test]
fn target_is_resolved_again_after_its_client_disconnects() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let mut setup = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect setup client");
    let (alpha_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "alpha"));
    let (first, first_id) = runtime.block_on(attach_user_client(&server, alpha_id));

    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), EmbersLaunchEnv::default())
            .expect("create embers client");
    assert_eq!(client.target_client_id().expect("resolve target"), first_id);

    // Detaching and attaching again gives the user a new client id.
    drop(first);
    let deadline = Instant::now() + Duration::from_secs(5);
    while runtime
        .block_on(list_clients(&mut setup))
        .iter()
        .any(|record| record.id == first_id)
    {
        assert!(Instant::now() < deadline, "first client never disconnected");
        thread::sleep(Duration::from_millis(25));
    }
    let (_second, second_id) = runtime.block_on(attach_user_client(&server, alpha_id));

    assert_eq!(
        client.current_session_name().expect("read current session"),
        Some("alpha".to_string())
    );
    assert_eq!(
        client.target_client_id().expect("resolve target"),
        second_id
    );
    assert_eq!(
        client
            .snapshot()
            .expect("snapshot")
            .context
            .current_session_name,
        Some("alpha".to_string()),
        "snapshot keeps the current session"
    );

    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

#[test]
fn unresolvable_target_names_embers_client() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let mut setup = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect setup client");
    let (alpha_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "alpha"));
    let (_first, _) = runtime.block_on(attach_user_client(&server, alpha_id));
    let (_second, _) = runtime.block_on(attach_user_client(&server, alpha_id));

    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), EmbersLaunchEnv::default())
            .expect("create embers client");

    let error = client
        .current_session_viewport_size()
        .expect_err("two attached clients and no hint is ambiguous");
    assert!(
        error.to_string().contains("EMBERS_CLIENT"),
        "unexpected error: {error}"
    );
    // The picker can still list sessions without a target.
    let snapshot = client.snapshot().expect("snapshot without a target");
    assert_eq!(snapshot.context.current_session_name, None);
    assert_eq!(snapshot.sessions.len(), 1);

    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

#[test]
fn switching_sessions_switches_the_target_client() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let workspace = unique_test_dir("switch");
    make_session_dir(&workspace);
    let mut setup = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect setup client");
    let (alpha_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "alpha"));
    let (beta_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "beta"));
    let (_user, user_id) = runtime.block_on(attach_user_client(&server, alpha_id));

    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), launched_by_client(user_id))
            .expect("create embers client");

    client.switch_session("beta").expect("switch to beta");
    assert_eq!(
        runtime
            .block_on(client_record(&mut setup, user_id))
            .current_session_id,
        Some(beta_id)
    );
    client.poll_updates().expect("poll updates");
    let snapshot = client.snapshot().expect("snapshot");
    assert_eq!(
        snapshot.context.current_session_name,
        Some("beta".to_string())
    );
    assert_eq!(
        snapshot.context.previous_session_name,
        Some("alpha".to_string())
    );

    client
        .create_or_switch_session("gamma", &workspace)
        .expect("create gamma");
    let gamma_id = runtime.block_on(session_id_by_name(&mut setup, "gamma"));
    assert_eq!(
        runtime
            .block_on(client_record(&mut setup, user_id))
            .current_session_id,
        Some(gamma_id)
    );

    // Only the user's client moved; wisp's own connection stays detached.
    let attached = runtime
        .block_on(list_clients(&mut setup))
        .into_iter()
        .filter(|record| record.current_session_id.is_some())
        .map(|record| record.id)
        .collect::<Vec<_>>();
    assert_eq!(attached, vec![user_id]);

    let _ = fs::remove_dir_all(&workspace);
    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

#[test]
fn session_preview_carries_the_panes_colors() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let server = runtime
        .block_on(TestServer::start())
        .expect("start embers server");
    let mut setup = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect setup client");
    // A kitty image placeholder cell: U+10EEEE with row and column
    // diacritics, the image id in the foreground and the placement id in the
    // underline color.
    let script = concat!(
        r"printf '\033[31mred\033[0m \033[1;38;2;10;20;30mrgb\033[0m ",
        r"\033[38;5;42;58;5;7m\364\216\273\256\314\205\314\215\033[0m\n'; ",
        "sleep 30",
    );
    let command = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    let (session_id, _) = runtime.block_on(create_session_running(&mut setup, "styled", command));
    let (_user, _) = runtime.block_on(attach_user_client(&server, session_id));

    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), EmbersLaunchEnv::default())
            .expect("create embers client");

    let deadline = Instant::now() + Duration::from_secs(10);
    let preview = loop {
        let preview = client
            .capture_session_preview("styled", 40)
            .expect("capture preview");
        if preview.contains("rgb") || Instant::now() >= deadline {
            break preview;
        }
        thread::sleep(Duration::from_millis(50));
    };

    assert_eq!(
        preview,
        "\u{1b}[0;31mred\u{1b}[0m \u{1b}[0;1;38;2;10;20;30mrgb\u{1b}[0m \
         \u{1b}[0;38;5;42;58;5;7m\u{10EEEE}\u{0305}\u{030D}\u{1b}[0m"
    );

    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}

/// The root node of the window `snapshot`'s session shows: the active tab of
/// its window list.
fn window_root(snapshot: &SessionSnapshot) -> Option<&NodeRecord> {
    let root = snapshot
        .nodes
        .iter()
        .find(|node| node.id == snapshot.session.root_node_id)?;
    let window_id = match root.tabs.as_ref() {
        Some(tabs) => tabs.tabs.get(tabs.active as usize)?.child_id,
        None => root.id,
    };
    snapshot.nodes.iter().find(|node| node.id == window_id)
}

#[test]
fn an_unreachable_host_leaves_no_session_behind() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    // A remote command that fails the way SSH does for a host it cannot reach.
    let server = runtime
        .block_on(TestServer::start_with(|config| {
            config.remote_command = vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo \"$0: no route to host\" >&2; exit 255".to_string(),
                "{host}".to_string(),
            ];
        }))
        .expect("start embers server");
    let mut setup = runtime
        .block_on(TestConnection::connect(server.socket_path()))
        .expect("connect setup client");
    let (alpha_id, _) = runtime.block_on(create_session_with_buffer(&mut setup, "alpha"));
    let (_user, user_id) = runtime.block_on(attach_user_client(&server, alpha_id));

    let client =
        EmbersClient::connect_with_launch_env(server.socket_path(), launched_by_client(user_id))
            .expect("create embers client");

    let error = client
        .create_or_switch_remote_session("devbox", "devbox")
        .expect_err("an unreachable host fails");
    assert!(
        error.to_string().contains("devbox: no route to host"),
        "unexpected error: {error}"
    );
    assert_eq!(
        client.list_session_names().expect("list sessions"),
        vec!["alpha".to_string()]
    );
    assert_eq!(
        runtime
            .block_on(client_record(&mut setup, user_id))
            .current_session_id,
        Some(alpha_id)
    );

    runtime
        .block_on(server.shutdown())
        .expect("shutdown embers server");
}
