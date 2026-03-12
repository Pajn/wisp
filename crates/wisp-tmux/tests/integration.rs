use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use wisp_tmux::{CommandTmuxClient, TmuxClient};

struct TmuxHarness {
    socket_name: String,
    root: PathBuf,
}

impl TmuxHarness {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be valid")
            .as_nanos();
        let socket_name = format!("wisp-test-{nonce}");
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
        let status = Command::new("tmux")
            .arg("-L")
            .arg(&self.socket_name)
            .arg("-f")
            .arg("/dev/null")
            .arg("new-session")
            .arg("-d")
            .arg("-s")
            .arg(session_name)
            .arg("-c")
            .arg(&self.root)
            .status()
            .expect("seed tmux session");

        assert!(status.success(), "seed session should succeed");
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
