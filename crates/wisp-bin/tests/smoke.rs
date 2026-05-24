use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wisp")
}

#[test]
fn no_args_prints_top_level_help() {
    let output = Command::new(bin()).output().expect("run wisp");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("Commands:"));
    assert!(stdout.contains("popup"));
    assert!(stdout.contains("statusline"));
    assert!(!stdout.contains("ui"));
}

#[test]
fn explicit_help_prints_top_level_help() {
    let output = Command::new(bin())
        .arg("--help")
        .output()
        .expect("run help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("Commands:"));
    assert!(stdout.contains("doctor"));
    assert!(stdout.contains("statusline"));
}

#[test]
fn print_config_command_dumps_effective_config() {
    let output = Command::new(bin())
        .arg("print-config")
        .output()
        .expect("run print-config");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ResolvedConfig"));
    assert!(stdout.contains("preview_width"));
}

#[test]
fn statusline_help_lists_nested_subcommands() {
    let output = Command::new(bin())
        .args(["statusline", "--help"])
        .output()
        .expect("run statusline help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("install"));
    assert!(stdout.contains("render"));
    assert!(stdout.contains("uninstall"));
}

#[test]
fn status_line_help_lists_nested_subcommands() {
    let output = Command::new(bin())
        .args(["status-line", "--help"])
        .output()
        .expect("run status-line help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage:"));
    assert!(stdout.contains("install"));
    assert!(stdout.contains("render"));
    assert!(stdout.contains("uninstall"));
}

#[test]
fn unknown_command_is_rejected() {
    let output = Command::new(bin())
        .arg("does-not-exist")
        .output()
        .expect("run unknown command");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does-not-exist"));
}

#[test]
fn doctor_command_reports_runtime_environment() {
    let output = Command::new(bin())
        .arg("doctor")
        .output()
        .expect("run doctor");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("wisp doctor"));
    assert!(stdout.contains("event strategy"));
}

#[test]
fn doctor_reports_embers_when_selected_without_a_socket() {
    let output = Command::new(bin())
        .arg("doctor")
        .env("WISP_BACKEND", "embers")
        // Isolate from any real user config (XDG short-circuits the HOME
        // fallback) so a local [embers].socket_path can't change the result.
        .env("XDG_CONFIG_HOME", env!("CARGO_TARGET_TMPDIR"))
        .env_remove("WISP_EMBERS_SOCKET")
        .env_remove("EMBERS_SOCKET")
        .output()
        .expect("run doctor");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("backend: embers"));
    #[cfg(feature = "embers")]
    {
        assert!(stdout.contains("socket not configured"));
        assert!(stdout.contains("event strategy: SubscriptionStream"));
    }
    #[cfg(not(feature = "embers"))]
    {
        assert!(stdout.contains("support not compiled in"));
        assert!(stdout.contains("event strategy: Disabled"));
    }
}

#[test]
fn statusline_render_command_prints_status_output() {
    let output = Command::new(bin())
        .args(["statusline", "render"])
        .output()
        .expect("run statusline render");

    if !output.status.success() {
        eprintln!("statusline render failed!");
        eprintln!("stdout: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("stderr: {}", String::from_utf8_lossy(&output.stderr));
    }
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("󰖔"));
}

#[test]
fn embers_statusline_reports_not_supported_before_connecting() {
    let output = Command::new(bin())
        .args(["statusline", "render"])
        .env("WISP_BACKEND", "embers")
        // Isolate from any real user config (XDG short-circuits the HOME
        // fallback) so a local [embers].socket_path can't change the result.
        .env("XDG_CONFIG_HOME", env!("CARGO_TARGET_TMPDIR"))
        .env_remove("WISP_EMBERS_SOCKET")
        .env_remove("EMBERS_SOCKET")
        .output()
        .expect("run statusline render");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not supported on the embers backend yet"));
    assert!(!stderr.contains("socket path"));
}
