use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_wisp")
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
fn statusline_render_command_prints_status_output() {
    let output = Command::new(bin())
        .args(["statusline", "render"])
        .output()
        .expect("run statusline render");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Wisp"));
}
