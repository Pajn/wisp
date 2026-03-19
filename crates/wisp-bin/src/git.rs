use std::{fs, path::Path, path::PathBuf, process::Command};

use wisp_core::{DomainState, GitBranchSync, WorktreeInfo};

/// Runs `git worktree list --porcelain` and returns all worktrees.
pub fn git_worktree_list(cwd: &Path) -> Vec<WorktreeInfo> {
    let output = match Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(cwd)
        .output()
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;
    let mut current_locked = false;

    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // Save the previous worktree if we were tracking one
            if let Some(p) = current_path.take() {
                worktrees.push(WorktreeInfo {
                    path: p,
                    branch: current_branch.take(),
                    is_locked: current_locked,
                });
                current_locked = false;
            }
            current_path = Some(PathBuf::from(path));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = Some(branch.to_string());
        } else if line.strip_prefix("locked").is_some() {
            current_locked = true;
        } else if line.strip_prefix("prunable").is_some() || line == "bare" {
            // Skip bare repos
            current_path = None;
            current_branch = None;
            current_locked = false;
        }
    }

    // Don't forget the last worktree
    if let Some(p) = current_path {
        worktrees.push(WorktreeInfo {
            path: p,
            branch: current_branch,
            is_locked: current_locked,
        });
    }

    worktrees
}

/// Finds the git repository root from a given path.
pub fn git_repo_root(path: &Path) -> Option<PathBuf> {
    path.ancestors().find_map(repo_root_at)
}

fn repo_root_at(path: &Path) -> Option<PathBuf> {
    let dot_git = path.join(".git");

    if dot_git.is_dir() {
        return resolve_git_dir(path)?.parent().map(Path::to_path_buf);
    }

    if !dot_git.is_file() {
        return None;
    }

    let pointer = fs::read_to_string(&dot_git).ok()?;
    let target = pointer.lines().next()?.trim().strip_prefix("gitdir: ")?;
    let git_dir = Path::new(target);
    let resolved_git_dir = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        dot_git.parent()?.join(git_dir)
    };

    if resolved_git_dir.exists() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

/// Returns the branch name for the given directory, or None if not on a branch.
pub fn branch_name_for_directory(path: &Path) -> Option<String> {
    path.ancestors().find_map(branch_name_for_git_root)
}

fn branch_name_for_git_root(path: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(path)?;
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();

    if let Some(reference) = head.strip_prefix("ref: ") {
        return Some(
            reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference)
                .to_string(),
        );
    }

    Some(head.chars().take(7).collect())
}

/// Resolves the .git directory for a path, handling both regular dirs and gitdir: pointers.
pub fn resolve_git_dir(path: &Path) -> Option<PathBuf> {
    let dot_git = path.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }

    if !dot_git.is_file() {
        return None;
    }

    let pointer = fs::read_to_string(&dot_git).ok()?;
    let target = pointer.trim().strip_prefix("gitdir: ")?;
    let git_dir = Path::new(target);
    if git_dir.is_absolute() {
        Some(git_dir.to_path_buf())
    } else {
        Some(path.join(git_dir))
    }
}

/// Returns the sync status and dirty flag for a given directory.
///
/// This only detects whether the local branch still needs to be pushed. Branches that are only
/// behind their upstream are still reported as [`GitBranchSync::Pushed`].
pub fn branch_status_for_directory(path: &Path) -> Option<(GitBranchSync, bool)> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["status", "--porcelain=2", "--branch"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut upstream = None;
    let mut ahead = 0usize;
    let mut dirty = false;

    for line in stdout.lines() {
        if let Some(remote) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(remote.to_string());
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            let mut parts = ab.split_whitespace();
            let ahead_raw = parts.next().and_then(|part| part.strip_prefix('+'));
            ahead = ahead_raw
                .and_then(|part| part.parse::<usize>().ok())
                .unwrap_or(0);
        } else if !line.starts_with("# ") && !line.is_empty() {
            dirty = true;
        }
    }

    let sync = if upstream.is_none() || ahead > 0 {
        GitBranchSync::NotPushed
    } else {
        GitBranchSync::Pushed
    };

    Some((sync, dirty))
}

/// Gets the git repository root based on the current tmux state.
/// Finds the current session's focused window path and resolves it to a git repo root.
pub fn worktree_repo_root(state: &DomainState, client_id: Option<&str>) -> Option<PathBuf> {
    // Get the current session
    let current_session_id = state.current_session_id(client_id)?;

    // Get the session record
    let session = state.sessions.get(current_session_id)?;

    // Find the active window
    let active_window = session
        .windows
        .values()
        .find(|window| window.active)
        .or_else(|| session.windows.values().next())?;

    // Get the current path from the window
    let current_path = active_window.current_path.as_ref()?;

    // Find the git repo root
    git_repo_root(current_path)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use wisp_core::GitBranchSync;

    use super::{branch_name_for_directory, branch_status_for_directory, git_repo_root};

    fn unique_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("wisp-git-test-{nonce}"))
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("git command");

        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dirs");
        }
        fs::write(path, contents).expect("write file");
    }

    fn init_synced_repo(root: &Path) -> (PathBuf, PathBuf) {
        let remote = root.join("remote.git");
        let local = root.join("local");

        run_git(
            root,
            &["init", "--bare", remote.to_str().expect("remote path")],
        );
        run_git(
            root,
            &[
                "clone",
                remote.to_str().expect("remote path"),
                local.to_str().expect("local path"),
            ],
        );
        run_git(&local, &["config", "user.name", "Wisp Tests"]);
        run_git(&local, &["config", "user.email", "wisp-tests@example.com"]);

        write_file(&local.join("README.md"), "seed\n");
        run_git(&local, &["add", "README.md"]);
        run_git(&local, &["commit", "-m", "seed"]);
        run_git(&local, &["push", "-u", "origin", "HEAD"]);

        (remote, local)
    }

    #[test]
    fn resolves_repo_root_for_nested_git_directory_paths() {
        let root = unique_root();
        let repo = root.join("repo");
        let nested = repo.join("src/module");
        fs::create_dir_all(repo.join(".git")).expect("git dir");
        fs::create_dir_all(&nested).expect("nested dir");

        assert_eq!(git_repo_root(&nested), Some(repo.clone()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_repo_root_for_worktree_git_files() {
        let root = unique_root();
        let repo = root.join("repo");
        let worktree = root.join("worktree");
        let nested = worktree.join("src/module");
        let worktree_git_dir = repo.join(".git/worktrees/feature");
        fs::create_dir_all(&worktree_git_dir).expect("worktree git dir");
        fs::create_dir_all(&nested).expect("nested worktree dir");
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git_dir.display()),
        )
        .expect("git pointer");

        assert_eq!(git_repo_root(&nested), Some(worktree.clone()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn branch_name_for_directory_strips_heads_prefix() {
        let root = unique_root();
        let repo = root.join("repo");
        fs::create_dir_all(repo.join(".git")).expect("git dir");
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feature/demo\n").expect("head file");

        assert_eq!(
            branch_name_for_directory(&repo),
            Some("feature/demo".to_string())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn branch_status_marks_ahead_branches_as_not_pushed() {
        let root = unique_root();
        fs::create_dir_all(&root).expect("root dir");
        let (_remote, local) = init_synced_repo(&root);

        write_file(&local.join("local.txt"), "ahead\n");
        run_git(&local, &["add", "local.txt"]);
        run_git(&local, &["commit", "-m", "local change"]);

        assert_eq!(
            branch_status_for_directory(&local),
            Some((GitBranchSync::NotPushed, false))
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn branch_status_treats_behind_only_branches_as_pushed() {
        let root = unique_root();
        fs::create_dir_all(&root).expect("root dir");
        let (remote, local) = init_synced_repo(&root);
        let peer = root.join("peer");

        run_git(
            &root,
            &[
                "clone",
                remote.to_str().expect("remote path"),
                peer.to_str().expect("peer path"),
            ],
        );
        run_git(&peer, &["config", "user.name", "Wisp Tests"]);
        run_git(&peer, &["config", "user.email", "wisp-tests@example.com"]);
        write_file(&peer.join("peer.txt"), "behind\n");
        run_git(&peer, &["add", "peer.txt"]);
        run_git(&peer, &["commit", "-m", "peer change"]);
        run_git(&peer, &["push"]);
        run_git(&local, &["fetch", "origin"]);

        assert_eq!(
            branch_status_for_directory(&local),
            Some((GitBranchSync::Pushed, false))
        );

        let _ = fs::remove_dir_all(root);
    }
}
