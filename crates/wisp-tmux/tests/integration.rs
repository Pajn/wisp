use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use wisp_tmux::{CommandTmuxClient, PopupCommand, SidebarPaneSpec, SidebarSide, TmuxClient};

struct TmuxHarness {
    socket_name: String,
    root: PathBuf,
}

static HARNESS_COUNTER: AtomicU64 = AtomicU64::new(0);

impl TmuxHarness {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be valid")
            .as_nanos();
        let unique = HARNESS_COUNTER.fetch_add(1, Ordering::Relaxed);
        let socket_name = format!("wisp-test-{nonce}-{unique}");
        let root = std::env::temp_dir().join(&socket_name);
        fs::create_dir_all(&root).expect("temporary tmux root");

        Self { socket_name, root }
    }

    fn client(&self) -> CommandTmuxClient {
        CommandTmuxClient::new()
            .with_socket_name(self.socket_name.clone())
            .with_config_file("/dev/null")
    }

    fn seed_session(&self, session_name: &str) {
        let status = self
            .run([
                "new-session",
                "-d",
                "-s",
                session_name,
                "-c",
                &self.root.display().to_string(),
            ])
            .status
            .code()
            .expect("seed tmux session exit status");

        assert_eq!(status, 0, "seed session should succeed");
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> std::process::Output {
        Command::new("tmux")
            .arg("-L")
            .arg(&self.socket_name)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .output()
            .expect("tmux command")
    }

    fn read_value<const N: usize>(&self, args: [&str; N]) -> String {
        String::from_utf8_lossy(&self.run(args).stdout)
            .trim()
            .to_string()
    }
}

impl Drop for TmuxHarness {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket_name)
            .arg("-f")
            .arg("/dev/null")
            .arg("kill-server")
            .status();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn lists_sessions_from_an_isolated_server() {
    let harness = TmuxHarness::new();
    harness.seed_session("alpha");
    harness.seed_session("beta");

    let sessions = harness
        .client()
        .list_sessions()
        .expect("list tmux sessions");

    let names = sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    assert!(sessions.iter().all(|session| session.id.starts_with('$')));
}

#[test]
fn ensures_sessions_are_created_from_directories() {
    let harness = TmuxHarness::new();
    let workspace = harness.root.join("workspace");
    fs::create_dir_all(&workspace).expect("workspace directory");

    harness
        .client()
        .ensure_session("workspace", &workspace)
        .expect("create session");

    let sessions = harness
        .client()
        .list_sessions()
        .expect("list tmux sessions");
    assert!(sessions.iter().any(|session| session.name == "workspace"));
}

#[test]
fn snapshots_include_capability_information() {
    let harness = TmuxHarness::new();
    harness.seed_session("alpha");

    let snapshot = harness
        .client()
        .snapshot(true)
        .expect("tmux snapshot should load");

    assert!(snapshot.capabilities.supports_popup);
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.name == "alpha")
    );
}

#[test]
fn captures_active_pane_from_selected_session() {
    let harness = TmuxHarness::new();
    let status = harness
        .run([
            "new-session",
            "-d",
            "-s",
            "alpha",
            "-c",
            &harness.root.display().to_string(),
            "printf 'alpha pane\\nline two\\n'; sleep 5",
        ])
        .status
        .code()
        .expect("seed tmux session exit status");

    assert_eq!(status, 0, "seed session should succeed");
    thread::sleep(Duration::from_millis(200));

    let captured = harness
        .client()
        .capture_pane("alpha")
        .expect("capture active pane");

    assert!(captured.contains("alpha pane"));
    assert!(captured.contains("line two"));
}

#[test]
fn opens_sidebar_panes_and_updates_status_lines() {
    let harness = TmuxHarness::new();
    harness.seed_session("alpha");

    harness
        .client()
        .open_sidebar_pane(&SidebarPaneSpec {
            target: Some("alpha".to_string()),
            side: SidebarSide::Left,
            width: 30,
            title: Some("Wisp Sidebar".to_string()),
            command: PopupCommand {
                program: PathBuf::from("/bin/sh"),
                args: vec!["-lc".to_string(), "sleep 1".to_string()],
            },
        })
        .expect("open sidebar pane");

    let panes = harness.read_value([
        "list-panes",
        "-t",
        "alpha",
        "-F",
        "#{pane_id}\t#{pane_title}\t#{pane_width}",
    ]);
    let pane_rows = panes.lines().collect::<Vec<_>>();
    assert!(pane_rows.len() >= 2);

    let sidebar_pane = pane_rows
        .iter()
        .find_map(|row| {
            let mut fields = row.split('\t');
            let pane_id = fields.next()?;
            let pane_title = fields.next()?;
            let pane_width = fields.next()?;
            (pane_title == "Wisp Sidebar" && pane_width == "30").then(|| pane_id.to_string())
        })
        .expect("sidebar pane id should exist");
    harness
        .client()
        .close_sidebar_pane(Some(&sidebar_pane))
        .expect("close sidebar pane");

    let client = harness.client();
    client
        .set_status_line_count(2)
        .expect("set status line count");
    assert_eq!(client.status_line_count().expect("status count"), 2);
    client
        .update_status_line(2, "Wisp  main")
        .expect("update status line");

    let rendered = harness.read_value(["show-options", "-gv", "status-format[1]"]);
    assert_eq!(rendered, "Wisp  main");
    client
        .set_hook("client-session-changed[200]", "refresh-client -S")
        .expect("set status refresh hook");
    let hooks = harness.read_value(["show-hooks", "-g", "client-session-changed"]);
    assert!(hooks.contains("refresh-client -S"));
    client
        .clear_hook("client-session-changed[200]")
        .expect("clear status refresh hook");
    client.clear_status_line(2).expect("clear status line");
    let cleared = harness.read_value(["show-options", "-gv", "status-format[1]"]);
    assert_ne!(cleared, "Wisp  main");
}

#[test]
fn kills_sessions() {
    let harness = TmuxHarness::new();
    harness.seed_session("alpha");
    harness.seed_session("beta");

    harness
        .client()
        .kill_session("alpha")
        .expect("kill session");

    let sessions = harness
        .client()
        .list_sessions()
        .expect("list tmux sessions");

    assert!(sessions.iter().all(|session| session.name != "alpha"));
    assert!(sessions.iter().any(|session| session.name == "beta"));
}

#[test]
fn renames_sessions() {
    let harness = TmuxHarness::new();
    harness.seed_session("alpha");

    harness
        .client()
        .rename_session("alpha", "renamed")
        .expect("rename session");

    let sessions = harness
        .client()
        .list_sessions()
        .expect("list tmux sessions");

    assert!(sessions.iter().all(|session| session.name != "alpha"));
    assert!(sessions.iter().any(|session| session.name == "renamed"));
}
