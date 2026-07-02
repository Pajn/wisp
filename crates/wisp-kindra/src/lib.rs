//! Integration with [Kindra](https://github.com/Pajn/kindra), a CLI for managing
//! stacked branches and managed git worktrees (the `kin` binary).
//!
//! Wisp uses this crate to detect whether the repository under the cursor has
//! Kindra *temporary* worktrees configured and, when it does, to create a fresh
//! temporary worktree on demand and report back the path it landed on.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde::Deserialize;
use thiserror::Error;

/// Reads Kindra state and drives the `kin` CLI for a repository.
pub trait KindraProvider {
    /// Returns `true` when the repository rooted at `repo_root` declares Kindra
    /// temporary worktrees (a `[worktrees]` section with the `temp` role enabled).
    fn temp_worktrees_configured(&self, repo_root: &Path) -> bool;

    /// Creates a new temporary worktree for a brand new branch `new_branch`
    /// based on `start_point`, returning the path of the created worktree.
    fn create_temp_worktree(
        &self,
        repo_root: &Path,
        new_branch: &str,
        start_point: &str,
    ) -> Result<PathBuf, KindraError>;

    /// Returns the branch names of all Kindra-managed *temporary* worktrees for
    /// the repository rooted at `repo_root`.
    fn temp_worktree_branches(&self, repo_root: &Path) -> Vec<String>;

    /// Removes the temporary worktree for `branch`. Never passes `--force`, so a
    /// worktree with uncommitted changes is left in place and surfaces an error.
    fn remove_temp_worktree(&self, repo_root: &Path, branch: &str) -> Result<(), KindraError>;
}

/// Drives Kindra through the `kin` command-line binary.
#[derive(Debug, Clone)]
pub struct CommandKindraProvider {
    binary: PathBuf,
    git_binary: PathBuf,
}

impl Default for CommandKindraProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandKindraProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("kin"),
            git_binary: PathBuf::from("git"),
        }
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    #[must_use]
    pub fn with_git_binary(mut self, git_binary: impl Into<PathBuf>) -> Self {
        self.git_binary = git_binary.into();
        self
    }

    /// Resolves the absolute git common directory for `repo_root`, where shared
    /// repository state (including `kindra.toml`) lives for linked worktrees.
    fn git_common_dir(&self, repo_root: &Path) -> Option<PathBuf> {
        let output = Command::new(&self.git_binary)
            .current_dir(repo_root)
            .args(["rev-parse", "--git-common-dir"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let common_dir = PathBuf::from(trimmed);
        Some(if common_dir.is_absolute() {
            common_dir
        } else {
            repo_root.join(common_dir)
        })
    }
}

impl KindraProvider for CommandKindraProvider {
    fn temp_worktrees_configured(&self, repo_root: &Path) -> bool {
        let Some(common_dir) = self.git_common_dir(repo_root) else {
            return false;
        };
        temp_worktrees_configured_in(&common_dir.join("kindra.toml"))
    }

    fn create_temp_worktree(
        &self,
        repo_root: &Path,
        new_branch: &str,
        start_point: &str,
    ) -> Result<PathBuf, KindraError> {
        let new_branch = new_branch.trim();
        // Reject empty and dash-prefixed names: a leading `-` would be parsed as
        // a flag by the `kin`/clap argument path rather than as the branch value.
        if new_branch.is_empty() || new_branch.starts_with('-') {
            return Err(KindraError::InvalidBranch {
                branch: new_branch.to_string(),
            });
        }

        let args = vec![
            "wt".to_string(),
            "temp".to_string(),
            "-b".to_string(),
            new_branch.to_string(),
            start_point.to_string(),
        ];

        let output = self.run_kin(repo_root, &args)?;
        parse_worktree_path(&String::from_utf8_lossy(&output.stdout))
    }

    fn temp_worktree_branches(&self, repo_root: &Path) -> Vec<String> {
        let Some(common_dir) = self.git_common_dir(repo_root) else {
            return Vec::new();
        };
        temp_worktree_branches_in(&common_dir.join("kindra_worktrees.json"))
    }

    fn remove_temp_worktree(&self, repo_root: &Path, branch: &str) -> Result<(), KindraError> {
        let branch = branch.trim();
        if branch.is_empty() {
            return Err(KindraError::InvalidBranch {
                branch: branch.to_string(),
            });
        }

        // `branch:<name>` targets the temp worktree for that branch (never main
        // or review). `--yes` skips kin's own prompt since we confirm in the UI.
        let args = vec![
            "wt".to_string(),
            "remove".to_string(),
            format!("branch:{branch}"),
            "--yes".to_string(),
        ];

        self.run_kin(repo_root, &args)?;
        Ok(())
    }
}

impl CommandKindraProvider {
    fn command_for_args(&self, args: &[String]) -> Vec<String> {
        std::iter::once(self.binary.display().to_string())
            .chain(args.iter().cloned())
            .collect()
    }

    /// Runs `kin` with `args` in `repo_root`, returning the completed output only
    /// when the process both spawned and exited successfully. Spawn failures map
    /// to [`KindraError::Unavailable`] (missing binary) or
    /// [`KindraError::SpawnFailed`]; a non-zero exit maps to
    /// [`KindraError::CommandFailed`] with the process's real stderr.
    fn run_kin(
        &self,
        repo_root: &Path,
        args: &[String],
    ) -> Result<std::process::Output, KindraError> {
        let output = Command::new(&self.binary)
            .current_dir(repo_root)
            .args(args)
            .output()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    KindraError::Unavailable {
                        message: source.to_string(),
                    }
                } else {
                    // The process never ran, so there is no command stderr here;
                    // CommandFailed is reserved for real execution failures below.
                    KindraError::SpawnFailed {
                        command: self.command_for_args(args),
                        message: source.to_string(),
                    }
                }
            })?;

        if !output.status.success() {
            return Err(KindraError::CommandFailed {
                command: self.command_for_args(args),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                status: output.status.code(),
            });
        }

        Ok(output)
    }
}

#[derive(Debug, Error)]
pub enum KindraError {
    #[error("kin is unavailable: {message}")]
    Unavailable { message: String },
    #[error("failed to spawn kin: {command:?}: {message}")]
    SpawnFailed {
        command: Vec<String>,
        message: String,
    },
    #[error("kin command failed: {command:?} (status {status:?}): {stderr}")]
    CommandFailed {
        command: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
    #[error("invalid branch name: {branch:?}")]
    InvalidBranch { branch: String },
    #[error("kin did not report a worktree path")]
    MissingPath,
}

/// Subset of `kindra.toml` Wisp needs to decide whether temp worktrees are on.
///
/// Unknown keys are ignored, so the rest of Kindra's schema can evolve freely.
#[derive(Debug, Default, Deserialize)]
struct KindraConfigFile {
    worktrees: Option<WorktreesConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct WorktreesConfig {
    temp: Option<TempConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct TempConfig {
    enabled: Option<bool>,
}

/// Returns whether the `kindra.toml` at `config_path` enables temporary worktrees.
///
/// Temp worktrees require a `[worktrees]` section to exist; within it the `temp`
/// role defaults to enabled, so it counts as configured unless explicitly
/// disabled with `temp.enabled = false`.
#[must_use]
pub fn temp_worktrees_configured_in(config_path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(config_path) else {
        return false;
    };
    let Ok(config) = toml::from_str::<KindraConfigFile>(&raw) else {
        return false;
    };
    match config.worktrees {
        Some(worktrees) => worktrees.temp.and_then(|temp| temp.enabled).unwrap_or(true),
        None => false,
    }
}

/// Subset of Kindra's `kindra_worktrees.json` metadata Wisp needs to identify
/// temp worktrees. Unknown keys (path, timestamps) are ignored.
#[derive(Debug, Default, Deserialize)]
struct WorktreeMetadataFile {
    #[serde(default)]
    worktrees: Vec<ManagedWorktreeRecord>,
}

#[derive(Debug, Deserialize)]
struct ManagedWorktreeRecord {
    role: String,
    branch: String,
}

/// Returns the branch names of temp worktrees recorded in the metadata file at
/// `metadata_path`. Missing or unparseable metadata yields an empty list.
#[must_use]
pub fn temp_worktree_branches_in(metadata_path: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(metadata_path) else {
        return Vec::new();
    };
    let Ok(metadata) = serde_json::from_str::<WorktreeMetadataFile>(&raw) else {
        return Vec::new();
    };
    metadata
        .worktrees
        .into_iter()
        .filter(|record| record.role == "temp")
        .map(|record| record.branch)
        .collect()
}

/// Extracts the worktree path Kindra prints on stdout after creating a worktree.
///
/// `kin wt temp` prints the resulting worktree path on its own line; we use the
/// last non-empty line so any leading diagnostics are ignored.
fn parse_worktree_path(stdout: &str) -> Result<PathBuf, KindraError> {
    stdout
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(PathBuf::from)
        .ok_or(KindraError::MissingPath)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{parse_worktree_path, temp_worktree_branches_in, temp_worktrees_configured_in};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wisp-kindra-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn lists_only_temp_worktree_branches_from_metadata() {
        let dir = temp_dir("metadata");
        let path = dir.join("kindra_worktrees.json");
        fs::write(
            &path,
            r#"{
              "version": 1,
              "worktrees": [
                {"role": "main", "branch": "main", "path": "p1", "created_at": 1, "last_used_at": 1},
                {"role": "temp", "branch": "feature/spike", "path": "p2", "created_at": 1, "last_used_at": 1},
                {"role": "temp", "branch": "hotfix", "path": "p3", "created_at": 1, "last_used_at": 1}
              ]
            }"#,
        )
        .expect("write metadata");

        let mut branches = temp_worktree_branches_in(&path);
        branches.sort();
        assert_eq!(
            branches,
            vec!["feature/spike".to_string(), "hotfix".to_string()]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_metadata_yields_no_temp_branches() {
        let dir = temp_dir("metadata-missing");
        assert!(temp_worktree_branches_in(&dir.join("kindra_worktrees.json")).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn create_temp_worktree_rejects_empty_and_dash_prefixed_branches() {
        use super::{CommandKindraProvider, KindraError, KindraProvider};

        let provider = CommandKindraProvider::new();
        // Validation happens before any `kin` spawn, so these need no binary.
        for name in ["", "  ", "-foo", "--force"] {
            let result = provider.create_temp_worktree(std::path::Path::new("/tmp"), name, "main");
            assert!(
                matches!(result, Err(KindraError::InvalidBranch { .. })),
                "expected InvalidBranch for {name:?}, got {result:?}"
            );
        }
    }

    #[test]
    fn detects_temp_worktrees_when_section_present() {
        let dir = temp_dir("enabled");
        let path = dir.join("kindra.toml");
        fs::write(
            &path,
            "[worktrees]\nroot = \".git/kindra-worktrees\"\ntrunk = \"main\"\n",
        )
        .expect("write config");

        assert!(temp_worktrees_configured_in(&path));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn honors_explicit_temp_disable() {
        let dir = temp_dir("disabled");
        let path = dir.join("kindra.toml");
        fs::write(&path, "[worktrees]\n\n[worktrees.temp]\nenabled = false\n")
            .expect("write config");

        assert!(!temp_worktrees_configured_in(&path));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_repos_without_a_worktrees_section() {
        let dir = temp_dir("no-section");
        let path = dir.join("kindra.toml");
        fs::write(&path, "upstream_branch = \"main\"\n").expect("write config");

        assert!(!temp_worktrees_configured_in(&path));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_config_is_not_configured() {
        let dir = temp_dir("missing");
        assert!(!temp_worktrees_configured_in(&dir.join("kindra.toml")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_trailing_worktree_path_from_output() {
        let path =
            parse_worktree_path("Creating worktree...\n/repo/.git/kindra-worktrees/temp/feature\n")
                .expect("path");
        assert_eq!(
            path,
            std::path::PathBuf::from("/repo/.git/kindra-worktrees/temp/feature")
        );
    }

    #[test]
    fn empty_output_has_no_path() {
        assert!(parse_worktree_path("\n  \n").is_err());
    }
}
