use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use wisp_kindra::{CommandKindraProvider, KindraProvider};

static UNIQUE_ROOT_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_root() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "wisp-kindra-it-{}-{}",
        std::process::id(),
        UNIQUE_ROOT_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp root");
    dir
}

fn run_git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("git command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo(root: &Path) {
    run_git(root, &["init", "-q"]);
    run_git(root, &["config", "user.name", "Wisp Tests"]);
    run_git(root, &["config", "user.email", "wisp-tests@example.com"]);
    std::fs::write(root.join("README.md"), "seed\n").expect("seed file");
    run_git(root, &["add", "README.md"]);
    run_git(root, &["commit", "-q", "-m", "seed"]);
}

#[test]
fn detects_temp_worktrees_via_git_common_dir() {
    let root = unique_root();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);
    std::fs::write(
        repo.join(".git/kindra.toml"),
        "[worktrees]\ntrunk = \"main\"\n",
    )
    .expect("write kindra.toml");

    let provider = CommandKindraProvider::new();
    assert!(provider.temp_worktrees_configured(&repo));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn linked_worktree_resolves_config_from_common_dir() {
    let root = unique_root();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);
    std::fs::write(
        repo.join(".git/kindra.toml"),
        "[worktrees]\ntrunk = \"main\"\n",
    )
    .expect("write kindra.toml");

    // A linked worktree only has a `.git` *file* pointing at the common dir, so
    // detection must follow `--git-common-dir` to find the shared kindra.toml.
    let linked = root.join("linked");
    run_git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().expect("linked path"),
        ],
    );

    let provider = CommandKindraProvider::new();
    assert!(provider.temp_worktrees_configured(&linked));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn repo_without_config_is_not_configured() {
    let root = unique_root();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    init_repo(&repo);

    let provider = CommandKindraProvider::new();
    assert!(!provider.temp_worktrees_configured(&repo));

    let _ = std::fs::remove_dir_all(&root);
}
