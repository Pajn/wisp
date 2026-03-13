use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::PathBuf,
};

use thiserror::Error;
use wisp_core::{DomainState, PreviewContent, PreviewKey, PreviewRequest};
use wisp_tmux::TmuxClient;

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
pub struct SessionDetailsPreviewProvider {
    pub state: DomainState,
}

#[derive(Debug)]
pub struct ActivePanePreviewProvider<T> {
    pub tmux: T,
    pub max_lines: usize,
}

impl<T> ActivePanePreviewProvider<T> {
    #[must_use]
    pub fn new(tmux: T) -> Self {
        Self {
            tmux,
            max_lines: 40,
        }
    }
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

impl PreviewProvider for SessionDetailsPreviewProvider {
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

impl<T> PreviewProvider for ActivePanePreviewProvider<T>
where
    T: TmuxClient,
{
    fn can_preview(&self, request: &PreviewRequest) -> bool {
        matches!(request, PreviewRequest::SessionSummary { .. })
    }

    fn generate(&self, request: &PreviewRequest) -> Result<PreviewContent, PreviewError> {
        let PreviewRequest::SessionSummary { session_name, .. } = request else {
            return Err(PreviewError::Unsupported);
        };

        let captured =
            self.tmux
                .capture_pane(session_name)
                .map_err(|source| PreviewError::Tmux {
                    session_name: session_name.clone(),
                    message: source.to_string(),
                })?;

        Ok(PreviewContent::from_text_tail(
            format!("Pane {session_name}"),
            captured,
            self.max_lines,
        ))
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
    #[error("failed to capture active pane for session `{session_name}`: {message}")]
    Tmux {
        session_name: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use wisp_core::{
        AlertAggregate, AttentionBadge, DomainState, PreviewContent, PreviewKey, PreviewRequest,
        SessionRecord, SessionSortKey, WindowRecord,
    };

    use crate::{
        ActivePanePreviewProvider, FilesystemPreviewProvider, PreviewCache, PreviewProvider,
        SessionDetailsPreviewProvider,
    };

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
        let provider = SessionDetailsPreviewProvider {
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

    #[derive(Debug, Default)]
    struct StubTmuxClient {
        capture: String,
    }

    impl wisp_tmux::TmuxClient for StubTmuxClient {
        fn capabilities(&self) -> Result<wisp_tmux::TmuxCapabilities, wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn current_context(&self) -> Result<wisp_tmux::TmuxContext, wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn list_sessions(&self) -> Result<Vec<wisp_tmux::TmuxSession>, wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn list_windows(&self) -> Result<Vec<wisp_tmux::TmuxWindow>, wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn capture_pane(&self, _target: &str) -> Result<String, wisp_tmux::TmuxError> {
            Ok(self.capture.clone())
        }

        fn snapshot(
            &self,
            _query_windows: bool,
        ) -> Result<wisp_tmux::TmuxSnapshot, wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn ensure_session(
            &self,
            _session_name: &str,
            _directory: &std::path::Path,
        ) -> Result<(), wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn switch_or_attach_session(
            &self,
            _session_name: &str,
        ) -> Result<(), wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn kill_session(&self, _session_name: &str) -> Result<(), wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn create_or_switch_session(
            &self,
            _session_name: &str,
            _directory: &std::path::Path,
        ) -> Result<(), wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn open_popup(
            &self,
            _command: &wisp_tmux::PopupCommand,
            _options: &wisp_tmux::PopupOptions,
        ) -> Result<(), wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn open_sidebar_pane(
            &self,
            _spec: &wisp_tmux::SidebarPaneSpec,
        ) -> Result<(), wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn close_sidebar_pane(&self, _target: Option<&str>) -> Result<(), wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }

        fn update_status_line(
            &self,
            _line: usize,
            _content: &str,
        ) -> Result<(), wisp_tmux::TmuxError> {
            unreachable!("not used in test");
        }
    }

    #[test]
    fn active_pane_preview_uses_tmux_capture() {
        let provider = ActivePanePreviewProvider {
            tmux: StubTmuxClient {
                capture: "one\ntwo\nthree".to_string(),
            },
            max_lines: 2,
        };

        let preview = provider
            .generate(&PreviewRequest::SessionSummary {
                key: PreviewKey::Session("alpha".to_string()),
                session_name: "alpha".to_string(),
            })
            .expect("active pane preview");

        assert_eq!(preview.body, vec!["two".to_string(), "three".to_string()]);
        assert!(preview.truncated);
    }
}
