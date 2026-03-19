use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use wisp_zoxide::{CommandZoxideProvider, ZoxideProvider};

fn unique_root() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be valid")
        .as_nanos();
    std::env::temp_dir().join(format!("wisp-zoxide-test-{nonce}"))
}

fn write_fake_zoxide_script(script: &Path, contents: String) {
    let staging = script.with_extension("tmp");
    fs::write(&staging, contents).expect("fake zoxide script");

    let mut permissions = fs::metadata(&staging)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&staging, permissions).expect("executable fake zoxide");
    fs::rename(&staging, script).expect("publish fake zoxide");
}

#[test]
fn loads_entries_from_a_fake_zoxide_binary() {
    let root = unique_root();
    let bin_dir = root.join("bin");
    let workspace = root.join("workspace");
    fs::create_dir_all(&bin_dir).expect("bin directory");
    fs::create_dir_all(&workspace).expect("workspace directory");

    let script = bin_dir.join("zoxide");
    write_fake_zoxide_script(
        &script,
        format!(
            "#!/bin/sh\nprintf '12.5 {workspace}\\n4.0 {workspace}/../workspace\\n99.0 {root}/missing\\n'\n",
            workspace = workspace.display(),
            root = root.display(),
        ),
    );

    let entries = CommandZoxideProvider::new()
        .with_binary(&script)
        .load_entries(50)
        .expect("zoxide entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].score, Some(12.5));
    assert_eq!(entries[0].path, workspace);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn queries_the_best_matching_directory() {
    let root = unique_root();
    let bin_dir = root.join("bin");
    let workspace = root.join("workspace");
    let nested = workspace.join("shell");
    fs::create_dir_all(&bin_dir).expect("bin directory");
    fs::create_dir_all(&nested).expect("nested workspace directory");

    let script = bin_dir.join("zoxide");
    write_fake_zoxide_script(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"query\" ] && [ \"$4\" = \"dev\" ] && [ \"$5\" = \"shell\" ]; then\n  printf '90.0 {nested}\\n12.0 {workspace}\\n'\nelse\n  exit 1\nfi\n",
            nested = nested.display(),
            workspace = workspace.display(),
        ),
    );

    let entry = CommandZoxideProvider::new()
        .with_binary(&script)
        .query_directory("dev shell")
        .expect("zoxide query should succeed")
        .expect("zoxide query should find a match");

    assert_eq!(entry.score, Some(90.0));
    assert_eq!(entry.path, nested);

    let _ = fs::remove_dir_all(root);
}
