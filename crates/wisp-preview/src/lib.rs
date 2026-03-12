use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::PathBuf,
};

use thiserror::Error;
use wisp_core::{DomainState, PreviewContent, PreviewKey, PreviewRequest};

pub trait PreviewProvider {
    fn can_preview(&self, request: &PreviewRequest) -> bool;
    fn generate(&self, request: &PreviewRequest) -> Result<PreviewContent, PreviewError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemPreviewProvider {
    pub max_file_bytes: usize,
    pub max_lines: usize,
    pub max_entries: usize,
}

impl Default for FilesystemPreviewProvider {
    fn default() -> Self {
        Self {
            max_file_bytes: 256 * 1024,
            max_lines: 40,
            max_entries: 20,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionPreviewProvider {
    pub state: DomainState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewCache {
    capacity: usize,
    order: VecDeque<PreviewKey>,
    entries: HashMap<PreviewKey, PreviewContent>,
}

impl PreviewCache {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    #[must_use]
    pub fn get(&self, key: &PreviewKey) -> Option<&PreviewContent> {
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: PreviewKey, value: PreviewContent) {
        if let std::collections::hash_map::Entry::Occupied(mut entry) =
            self.entries.entry(key.clone())
        {
            entry.insert(value);
            return;
        }

        if self.order.len() >= self.capacity
            && let Some(oldest) = self.order.pop_front()
        {
            self.entries.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
    }
}

impl PreviewProvider for FilesystemPreviewProvider {
    fn can_preview(&self, request: &PreviewRequest) -> bool {
        matches!(
            request,
            PreviewRequest::Directory { .. }
                | PreviewRequest::File { .. }
                | PreviewRequest::Metadata { .. }
        )
    }

    fn generate(&self, request: &PreviewRequest) -> Result<PreviewContent, PreviewError> {
        match request {
            PreviewRequest::Directory { path, .. } => self.preview_directory(path),
            PreviewRequest::File { path, .. } => self.preview_file(path),
            PreviewRequest::Metadata { title, .. } => Ok(PreviewContent::from_text(title, "", 1)),
            PreviewRequest::SessionSummary { .. } => Err(PreviewError::Unsupported),
        }
    }
}

impl FilesystemPreviewProvider {
    fn preview_directory(&self, path: &PathBuf) -> Result<PreviewContent, PreviewError> {
        let mut entries = fs::read_dir(path)
            .map_err(|source| PreviewError::Io {
                path: path.clone(),
                source,
            })?
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect::<Vec<_>>();
        entries.sort();
        entries.truncate(self.max_entries);

        Ok(PreviewContent {
            title: format!("Directory {}", path.display()),
            body: if entries.is_empty() {
                vec!["(empty)".to_string()]
            } else {
                entries
            },
            truncated: false,
        })
    }

    fn preview_file(&self, path: &PathBuf) -> Result<PreviewContent, PreviewError> {
        let metadata = fs::metadata(path).map_err(|source| PreviewError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.len() as usize > self.max_file_bytes {
            return Ok(PreviewContent::from_text(
                format!("File {}", path.display()),
                "[file too large to preview]",
                1,
            ));
        }

        let text = fs::read_to_string(path).map_err(|source| PreviewError::Io {
            path: path.clone(),
            source,
        })?;

        Ok(PreviewContent::from_text(
            format!("File {}", path.display()),
            text,
            self.max_lines,
        ))
    }
}

impl PreviewProvider for SessionPreviewProvider {
    fn can_preview(&self, request: &PreviewRequest) -> bool {
        matches!(request, PreviewRequest::SessionSummary { .. })
    }

    fn generate(&self, request: &PreviewRequest) -> Result<PreviewContent, PreviewError> {
        let PreviewRequest::SessionSummary { session_name, .. } = request else {
            return Err(PreviewError::Unsupported);
        };

        let Some(session) = self.state.sessions.get(session_name) else {
            return Err(PreviewError::MissingSession(session_name.clone()));
        };

        let mut lines = vec![
            format!("attached: {}", session.attached),
            format!("windows: {}", session.windows.len()),
            format!("attention: {:?}", session.aggregate_alerts.highest_priority),
        ];
        for window in session.windows.values().take(8) {
            lines.push(format!(
                "{} {}",
                if window.active { "*" } else { "-" },
                window.name
            ));
        }

        Ok(PreviewContent {
            title: format!("Session {}", session.name),
            body: lines,
            truncated: session.windows.len() > 8,
        })
    }
}

#[derive(Debug, Error)]
pub enum PreviewError {
    #[error("preview source is unsupported")]
    Unsupported,
    #[error("failed to read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("session `{0}` was not found")]
    MissingSession(String),
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use wisp_core::{
        AlertAggregate, AttentionBadge, DomainState, PreviewContent, PreviewKey, PreviewRequest,
        SessionRecord, SessionSortKey, WindowRecord,
    };

    use crate::{FilesystemPreviewProvider, PreviewCache, PreviewProvider, SessionPreviewProvider};

    #[test]
    fn preview_cache_evicts_oldest_entries() {
        let mut cache = PreviewCache::new(1);
        cache.insert(
            PreviewKey::Metadata("one".to_string()),
            PreviewContent::from_text("one", "a", 1),
        );
        cache.insert(
            PreviewKey::Metadata("two".to_string()),
            PreviewContent::from_text("two", "b", 1),
        );

        assert!(
            cache
                .get(&PreviewKey::Metadata("one".to_string()))
                .is_none()
        );
        assert!(
            cache
                .get(&PreviewKey::Metadata("two".to_string()))
                .is_some()
        );
    }

    #[test]
    fn filesystem_preview_truncates_large_files() {
        let root = std::env::temp_dir().join("wisp-preview-file");
        fs::write(&root, "line1\nline2\nline3\nline4").expect("preview fixture");

        let preview = FilesystemPreviewProvider {
            max_lines: 2,
            ..FilesystemPreviewProvider::default()
        }
        .generate(&PreviewRequest::File {
            key: PreviewKey::File(root.clone()),
            path: root.clone(),
        })
        .expect("file preview");

        assert_eq!(preview.body, vec!["line1".to_string(), "line2".to_string()]);
        assert!(preview.truncated);

        let _ = fs::remove_file(root);
    }

    #[test]
    fn session_preview_uses_domain_state() {
        let provider = SessionPreviewProvider {
            state: DomainState {
                sessions: BTreeMap::from([(
                    "alpha".to_string(),
                    SessionRecord {
                        id: "alpha".to_string(),
                        name: "alpha".to_string(),
                        attached: true,
                        windows: BTreeMap::from([(
                            "alpha:1".to_string(),
                            WindowRecord {
                                id: "alpha:1".to_string(),
                                index: 1,
                                name: "shell".to_string(),
                                active: true,
                                panes: BTreeMap::new(),
                                alerts: Default::default(),
                                has_unseen: false,
                                current_path: None,
                                active_command: None,
                            },
                        )]),
                        aggregate_alerts: AlertAggregate {
                            any_activity: false,
                            any_bell: false,
                            any_silence: false,
                            any_unseen: false,
                            attention_count: 0,
                            highest_priority: AttentionBadge::None,
                        },
                        has_unseen: false,
                        sort_key: SessionSortKey::default(),
                    },
                )]),
                ..DomainState::default()
            },
        };

        let preview = provider
            .generate(&PreviewRequest::SessionSummary {
                key: PreviewKey::Session("alpha".to_string()),
                session_name: "alpha".to_string(),
            })
            .expect("session preview");

        assert!(preview.body.iter().any(|line| line.contains("windows: 1")));
    }
}
