use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
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

#[test]
fn loads_entries_from_a_fake_zoxide_binary() {
    let root = unique_root();
    let bin_dir = root.join("bin");
    let workspace = root.join("workspace");
    fs::create_dir_all(&bin_dir).expect("bin directory");
    fs::create_dir_all(&workspace).expect("workspace directory");

    let script = bin_dir.join("zoxide");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '12.5 {workspace}\\n4.0 {workspace}/../workspace\\n99.0 {root}/missing\\n'\n",
            workspace = workspace.display(),
            root = root.display(),
        ),
    )
    .expect("fake zoxide script");
    let mut permissions = fs::metadata(&script)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).expect("executable fake zoxide");

    let entries = CommandZoxideProvider::new()
        .with_binary(&script)
        .load_entries(50)
        .expect("zoxide entries");

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].score, Some(12.5));
    assert_eq!(entries[0].path, workspace);

    let _ = fs::remove_dir_all(root);
}
