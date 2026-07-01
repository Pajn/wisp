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
        if new_branch.is_empty() {
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

        let output = Command::new(&self.binary)
            .current_dir(repo_root)
            .args(&args)
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
                        command: self.command_for_args(&args),
                        message: source.to_string(),
                    }
                }
            })?;

        if !output.status.success() {
            return Err(KindraError::CommandFailed {
                command: self.command_for_args(&args),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                status: output.status.code(),
            });
        }

        parse_worktree_path(&String::from_utf8_lossy(&output.stdout))
    }
}

impl CommandKindraProvider {
    fn command_for_args(&self, args: &[String]) -> Vec<String> {
        std::iter::once(self.binary.display().to_string())
            .chain(args.iter().cloned())
            .collect()
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

    use super::{parse_worktree_path, temp_worktrees_configured_in};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wisp-kindra-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        dir
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
