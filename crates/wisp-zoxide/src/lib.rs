use std::{
    collections::BTreeMap,
    env,
    path::{Component, Path, PathBuf},
    process::Command,
};

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderMode {
    Query,
    FrecencyList,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryEntry {
    pub path: PathBuf,
    pub score: Option<f64>,
    pub exists: bool,
}

pub trait ZoxideProvider {
    fn load_entries(&self, max_entries: usize) -> Result<Vec<DirectoryEntry>, ZoxideError>;
    fn query_directory(&self, query: &str) -> Result<Option<DirectoryEntry>, ZoxideError>;
}

#[derive(Debug, Clone)]
pub struct CommandZoxideProvider {
    binary: PathBuf,
    mode: ProviderMode,
    include_missing: bool,
}

impl Default for CommandZoxideProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandZoxideProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            binary: PathBuf::from("zoxide"),
            mode: ProviderMode::Query,
            include_missing: false,
        }
    }

    #[must_use]
    pub fn with_binary(mut self, binary: impl Into<PathBuf>) -> Self {
        self.binary = binary.into();
        self
    }

    #[must_use]
    pub fn with_mode(mut self, mode: ProviderMode) -> Self {
        self.mode = mode;
        self
    }

    #[must_use]
    pub fn with_missing_entries(mut self, include_missing: bool) -> Self {
        self.include_missing = include_missing;
        self
    }

    fn args(&self) -> Vec<&'static str> {
        match self.mode {
            ProviderMode::Query | ProviderMode::FrecencyList => vec!["query", "-l", "-s"],
        }
    }

    fn query_args(&self, query: &str) -> Vec<String> {
        let mut args = self
            .args()
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        args.extend(query.split_whitespace().map(str::to_string));
        args
    }

    fn command_for_args(&self, args: &[String]) -> Vec<String> {
        std::iter::once(self.binary.display().to_string())
            .chain(args.iter().cloned())
            .collect()
    }
}

impl ZoxideProvider for CommandZoxideProvider {
    fn load_entries(&self, max_entries: usize) -> Result<Vec<DirectoryEntry>, ZoxideError> {
        let output = Command::new(&self.binary)
            .args(self.args())
            .output()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    ZoxideError::Unavailable {
                        message: source.to_string(),
                    }
                } else {
                    ZoxideError::CommandFailed {
                        command: vec![
                            self.binary.display().to_string(),
                            "query".to_string(),
                            "-l".to_string(),
                            "-s".to_string(),
                        ],
                        stderr: source.to_string(),
                        status: None,
                    }
                }
            })?;

        if !output.status.success() {
            return Err(ZoxideError::CommandFailed {
                command: vec![
                    self.binary.display().to_string(),
                    "query".to_string(),
                    "-l".to_string(),
                    "-s".to_string(),
                ],
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                status: output.status.code(),
            });
        }

        let parsed = parse_entries(&String::from_utf8_lossy(&output.stdout))?;
        Ok(normalize_entries(parsed, self.include_missing)
            .into_iter()
            .take(max_entries)
            .collect())
    }

    fn query_directory(&self, query: &str) -> Result<Option<DirectoryEntry>, ZoxideError> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(None);
        }

        let args = self.query_args(query);
        let output = Command::new(&self.binary)
            .args(&args)
            .output()
            .map_err(|source| {
                if source.kind() == std::io::ErrorKind::NotFound {
                    ZoxideError::Unavailable {
                        message: source.to_string(),
                    }
                } else {
                    ZoxideError::CommandFailed {
                        command: self.command_for_args(&args),
                        stderr: source.to_string(),
                        status: None,
                    }
                }
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.is_empty() {
                return Ok(None);
            }

            return Err(ZoxideError::CommandFailed {
                command: self.command_for_args(&args),
                stderr,
                status: output.status.code(),
            });
        }

        let parsed = parse_entries(&String::from_utf8_lossy(&output.stdout))?;
        for entry in parsed {
            let path = normalize_path(&entry.path);
            let exists = path.exists();
            if !self.include_missing && !exists {
                continue;
            }

            return Ok(Some(DirectoryEntry {
                path,
                score: entry.score,
                exists,
            }));
        }

        Ok(None)
    }
}

#[derive(Debug, Error)]
pub enum ZoxideError {
    #[error("zoxide is unavailable: {message}")]
    Unavailable { message: String },
    #[error("zoxide command failed: {command:?} (status {status:?}): {stderr}")]
    CommandFailed {
        command: Vec<String>,
        status: Option<i32>,
        stderr: String,
    },
    #[error("failed to parse zoxide output: {message}")]
    Parse { message: String },
}

#[must_use]
pub fn normalize_entries(
    entries: impl IntoIterator<Item = DirectoryEntry>,
    include_missing: bool,
) -> Vec<DirectoryEntry> {
    let mut deduplicated = BTreeMap::<PathBuf, DirectoryEntry>::new();

    for entry in entries {
        let normalized_path = normalize_path(&entry.path);
        let exists = normalized_path.exists();
        if !include_missing && !exists {
            continue;
        }

        let candidate = DirectoryEntry {
            path: normalized_path.clone(),
            score: entry.score,
            exists,
        };

        match deduplicated.get(&normalized_path) {
            Some(existing)
                if existing.exists
                    && (!candidate.exists
                        || existing.score.unwrap_or_default()
                            >= candidate.score.unwrap_or_default()) => {}
            Some(existing)
                if existing.score.unwrap_or_default() >= candidate.score.unwrap_or_default() => {}
            _ => {
                deduplicated.insert(normalized_path, candidate);
            }
        }
    }

    deduplicated.into_values().collect()
}

pub fn parse_entries(output: &str) -> Result<Vec<DirectoryEntry>, ZoxideError> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let trimmed = line.trim();
            let mut parts = trimmed.splitn(2, char::is_whitespace);
            let first = parts.next().unwrap_or_default();
            let second = parts.next().map(str::trim_start).unwrap_or_default();

            if second.is_empty() {
                Ok(DirectoryEntry {
                    path: PathBuf::from(first),
                    score: None,
                    exists: Path::new(first).exists(),
                })
            } else if let Ok(score) = first.parse::<f64>() {
                Ok(DirectoryEntry {
                    path: PathBuf::from(second),
                    score: Some(score),
                    exists: Path::new(second).exists(),
                })
            } else {
                Ok(DirectoryEntry {
                    path: PathBuf::from(trimmed),
                    score: None,
                    exists: Path::new(trimmed).exists(),
                })
            }
        })
        .collect()
}

#[must_use]
pub fn default_home_dir() -> Option<PathBuf> {
    env::var("HOME").ok().map(PathBuf::from)
}

#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(segment) => normalized.push(segment),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
        }
    }

    if normalized.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        CommandZoxideProvider, DirectoryEntry, normalize_entries, normalize_path, parse_entries,
    };

    #[test]
    fn parses_scored_zoxide_output() {
        let output = "12.5 /tmp/workspace\n3.2 /tmp/other";

        let entries = parse_entries(output).expect("parse entries");

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].score, Some(12.5));
        assert_eq!(entries[0].path, std::path::PathBuf::from("/tmp/workspace"));
    }

    #[test]
    fn normalizes_paths_lexically() {
        let path = normalize_path(std::path::Path::new("/tmp/demo/./nested/../project"));

        assert_eq!(path, std::path::PathBuf::from("/tmp/demo/project"));
    }

    #[test]
    fn deduplicates_paths_and_drops_missing_entries() {
        let root = std::env::temp_dir().join("wisp-zoxide-unit");
        let existing = root.join("existing");
        fs::create_dir_all(&existing).expect("existing temp directory");

        let entries = normalize_entries(
            vec![
                DirectoryEntry {
                    path: existing.clone(),
                    score: Some(5.0),
                    exists: true,
                },
                DirectoryEntry {
                    path: existing.join("..").join("existing"),
                    score: Some(10.0),
                    exists: true,
                },
                DirectoryEntry {
                    path: root.join("missing"),
                    score: Some(50.0),
                    exists: false,
                },
            ],
            false,
        );

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].score, Some(10.0));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn builds_query_arguments_from_whitespace_separated_terms() {
        let provider = CommandZoxideProvider::new();

        assert_eq!(
            provider.query_args("  dev shell  "),
            vec!["query", "-l", "-s", "dev", "shell"]
        );
    }
}
