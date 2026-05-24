use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use embers_core::{BufferId, SessionId, new_request_id};
use embers_protocol::{
    BufferLocation, BufferLocationResponse, BufferRequest, ClientMessage, ServerResponse,
    SessionRequest,
};
use embers_test_support::TestServer;
use wisp_embers::{EmbersClient, EmbersJoinPlacement};

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

async fn session_id_by_name(
    connection: &mut embers_test_support::TestConnection,
    name: &str,
) -> SessionId {
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

async fn buffer_location(
    connection: &mut embers_test_support::TestConnection,
    buffer_id: BufferId,
) -> BufferLocation {
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
    let client = EmbersClient::connect(server.socket_path()).expect("create embers client");
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
        .block_on(embers_test_support::TestConnection::connect(
            server.socket_path(),
        ))
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
    let client = EmbersClient::connect(server.socket_path()).expect("create embers client");
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
        .block_on(embers_test_support::TestConnection::connect(
            server.socket_path(),
        ))
        .expect("connect protocol client");
    let alpha_id = runtime.block_on(session_id_by_name(&mut connection, "alpha"));
    let alpha_snapshot = runtime
        .block_on(connection.session_snapshot(alpha_id))
        .expect("alpha snapshot");
    let alpha_root = alpha_snapshot
        .nodes
        .iter()
        .find(|node| node.id == alpha_snapshot.session.root_node_id)
        .expect("alpha root node");
    let alpha_split = alpha_root.split.as_ref().expect("root should become split");
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
    let beta_root = beta_snapshot
        .nodes
        .iter()
        .find(|node| node.id == beta_snapshot.session.root_node_id)
        .expect("beta root node");
    let beta_split = beta_root
        .split
        .as_ref()
        .expect("beta root should become split");
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
