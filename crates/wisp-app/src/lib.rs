use std::path::PathBuf;

use wisp_config::{ResolvedConfig, UiMode};
use wisp_core::{
    Candidate, CandidateId, ClientFocus, DirectoryRecord, DomainState, PaneRecord, PreviewContent,
    PreviewKey, PreviewRequest, ResolvedAction, SessionRecord, SessionSortKey, WindowRecord,
    deduplicate_candidates, derive_candidates, preview_request_for_candidate, resolve_action,
    sort_candidates,
};
use wisp_tmux::TmuxSnapshot;
use wisp_zoxide::DirectoryEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Popup,
    Fullscreen,
    Auto,
}

impl From<UiMode> for AppMode {
    fn from(value: UiMode) -> Self {
        match value {
            UiMode::Popup => Self::Popup,
            UiMode::Fullscreen => Self::Fullscreen,
            UiMode::Auto => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadState<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusLine {
    pub level: StatusLevel,
    pub message: String,
}

impl Default for StatusLine {
    fn default() -> Self {
        Self {
            level: StatusLevel::Info,
            message: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewState {
    pub generation: u64,
    pub active_key: Option<PreviewKey>,
    pub content: Option<PreviewContent>,
    pub loading: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PendingTasks {
    pub loading_sources: usize,
    pub action_in_flight: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserIntent {
    MoveUp,
    MoveDown,
    QueryChanged(String),
    ConfirmSelection,
    Refresh,
    ToggleHelp,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppCommand {
    RequestPreview {
        generation: u64,
        request: PreviewRequest,
    },
    ExecuteAction(ResolvedAction),
    RefreshSources,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateBuildOptions {
    pub home: Option<PathBuf>,
    pub include_missing_directories: bool,
}

impl Default for CandidateBuildOptions {
    fn default() -> Self {
        Self {
            home: std::env::var("HOME").ok().map(PathBuf::from),
            include_missing_directories: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateSources {
    pub tmux: TmuxSnapshot,
    pub zoxide: Vec<DirectoryEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    Startup,
    Input(UserIntent),
    CandidatesLoaded(Vec<Candidate>),
    PreviewReady {
        generation: u64,
        key: PreviewKey,
        result: Result<PreviewContent, String>,
    },
    ActionCompleted(Result<(), String>),
    Quit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    pub mode: AppMode,
    pub config: ResolvedConfig,
    pub candidates: Vec<Candidate>,
    pub filtered: Vec<CandidateId>,
    pub selection: usize,
    pub query: String,
    pub preview: PreviewState,
    pub status: StatusLine,
    pub pending_tasks: PendingTasks,
    pub show_help: bool,
}

impl AppState {
    #[must_use]
    pub fn new(config: ResolvedConfig) -> Self {
        Self {
            mode: AppMode::from(config.ui.mode),
            show_help: config.ui.show_help,
            config,
            candidates: Vec::new(),
            filtered: Vec::new(),
            selection: 0,
            query: String::new(),
            preview: PreviewState::default(),
            status: StatusLine::default(),
            pending_tasks: PendingTasks::default(),
        }
    }

    pub fn replace_candidates(&mut self, candidates: Vec<Candidate>) {
        let mut candidates = deduplicate_candidates(candidates);
        sort_candidates(&mut candidates);
        self.candidates = candidates;
        self.refresh_filter();
        self.status = StatusLine {
            level: StatusLevel::Info,
            message: format!("Loaded {} candidates", self.candidates.len()),
        };
    }

    pub fn apply_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.refresh_filter();
        self.status = StatusLine {
            level: StatusLevel::Info,
            message: format!("{} matches", self.filtered.len()),
        };
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            self.selection = 0;
            return;
        }

        let max_index = self.filtered.len().saturating_sub(1) as isize;
        let next = (self.selection as isize + delta).clamp(0, max_index);
        self.selection = next as usize;
    }

    #[must_use]
    pub fn selected_candidate(&self) -> Option<&Candidate> {
        let selected_id = self.filtered.get(self.selection)?;
        self.candidates
            .iter()
            .find(|candidate| &candidate.id == selected_id)
    }

    pub fn request_preview(&mut self) -> Option<AppCommand> {
        let candidate = self.selected_candidate()?;
        let request = preview_request_for_candidate(candidate);

        self.preview.generation += 1;
        self.preview.active_key = Some(request.key().clone());
        self.preview.loading = true;
        self.preview.content = None;

        Some(AppCommand::RequestPreview {
            generation: self.preview.generation,
            request,
        })
    }

    pub fn handle_intent(&mut self, intent: UserIntent) -> Option<AppCommand> {
        match intent {
            UserIntent::MoveUp => {
                self.move_selection(-1);
                self.request_preview()
            }
            UserIntent::MoveDown => {
                self.move_selection(1);
                self.request_preview()
            }
            UserIntent::QueryChanged(query) => {
                self.apply_query(query);
                self.request_preview()
            }
            UserIntent::ConfirmSelection => self
                .selected_candidate()
                .and_then(resolve_action)
                .map(AppCommand::ExecuteAction),
            UserIntent::Refresh => Some(AppCommand::RefreshSources),
            UserIntent::ToggleHelp => {
                self.show_help = !self.show_help;
                None
            }
            UserIntent::Cancel => Some(AppCommand::Quit),
        }
    }

    pub fn apply_preview_result(
        &mut self,
        generation: u64,
        key: &PreviewKey,
        result: Result<PreviewContent, String>,
    ) {
        if generation != self.preview.generation || self.preview.active_key.as_ref() != Some(key) {
            return;
        }

        self.preview.loading = false;
        match result {
            Ok(content) => {
                self.preview.content = Some(content);
            }
            Err(message) => {
                self.status = StatusLine {
                    level: StatusLevel::Warning,
                    message,
                };
            }
        }
    }

    fn refresh_filter(&mut self) {
        self.filtered = self
            .candidates
            .iter()
            .filter(|candidate| candidate.matches_query(&self.query))
            .map(|candidate| candidate.id.clone())
            .collect();
        self.selection = self.selection.min(self.filtered.len().saturating_sub(1));
    }
}

#[must_use]
pub fn rebuild_candidates(
    sources: &CandidateSources,
    options: &CandidateBuildOptions,
) -> Vec<Candidate> {
    let state = build_domain_state(sources);
    derive_candidates(
        &state,
        options.home.as_deref(),
        options.include_missing_directories,
    )
}

#[must_use]
pub fn build_domain_state(sources: &CandidateSources) -> DomainState {
    let sessions = sources
        .tmux
        .sessions
        .iter()
        .map(|session| {
            let windows = sources
                .tmux
                .windows
                .iter()
                .filter(|window| window.session_name == session.name)
                .map(|window| {
                    let pane_id = format!("{}:{}.1", session.name, window.index);
                    (
                        format!("{}:{}", session.name, window.index),
                        WindowRecord {
                            id: format!("{}:{}", session.name, window.index),
                            index: window.index as i32,
                            name: window.name.clone(),
                            active: window.active,
                            panes: std::collections::BTreeMap::from([(
                                pane_id.clone(),
                                PaneRecord {
                                    id: pane_id,
                                    index: 1,
                                    title: None,
                                    current_path: None,
                                    current_command: None,
                                    is_active: window.active,
                                },
                            )]),
                            alerts: Default::default(),
                            has_unseen: false,
                            current_path: None,
                            active_command: None,
                        },
                    )
                })
                .collect();

            (
                session.name.clone(),
                SessionRecord {
                    id: session.name.clone(),
                    name: session.name.clone(),
                    attached: session.attached,
                    windows,
                    aggregate_alerts: Default::default(),
                    has_unseen: false,
                    sort_key: SessionSortKey {
                        last_activity: session.last_activity,
                    },
                },
            )
        })
        .collect();
    let clients = sources
        .tmux
        .context
        .session_name
        .as_ref()
        .zip(sources.tmux.context.window_index)
        .map(|(session_name, window_index)| {
            (
                "default".to_string(),
                ClientFocus {
                    session_id: session_name.clone(),
                    window_id: format!("{session_name}:{window_index}"),
                    pane_id: sources.tmux.context.pane_id.clone(),
                },
            )
        })
        .into_iter()
        .collect();
    let directories = sources
        .zoxide
        .iter()
        .map(|entry| DirectoryRecord {
            path: entry.path.clone(),
            score: entry.score,
            exists: entry.exists,
        })
        .collect();

    let mut state = DomainState {
        sessions,
        clients,
        previous_session_by_client: Default::default(),
        directories,
        config: Default::default(),
    };
    state.recompute_aggregates();
    state
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wisp_config::ResolvedConfig;
    use wisp_core::{Candidate, DirectoryMetadata, PreviewContent, PreviewKey, SessionMetadata};
    use wisp_tmux::{TmuxCapabilities, TmuxContext, TmuxSession, TmuxSnapshot, TmuxVersion};
    use wisp_zoxide::DirectoryEntry;

    use crate::{
        AppCommand, AppMode, AppState, CandidateBuildOptions, CandidateSources, StatusLevel,
        UserIntent, rebuild_candidates,
    };

    #[test]
    fn bootstraps_from_config() {
        let config = ResolvedConfig::default();

        let state = AppState::new(config.clone());

        assert_eq!(state.mode, AppMode::Auto);
        assert!(state.show_help);
        assert_eq!(state.config, config);
    }

    #[test]
    fn query_changes_rebuild_the_filtered_set() {
        let mut state = AppState::new(ResolvedConfig::default());
        state.replace_candidates(vec![
            Candidate::session(SessionMetadata {
                session_name: "alpha".to_string(),
                attached: false,
                current: true,
                window_count: 1,
                last_activity: Some(10),
            }),
            Candidate::directory(DirectoryMetadata {
                full_path: PathBuf::from("/tmp/project-beta"),
                display_path: "/tmp/project-beta".to_string(),
                zoxide_score: Some(5.0),
                git_root_hint: None,
                exists: true,
            }),
        ]);

        state.apply_query("beta");

        assert_eq!(state.filtered.len(), 1);
        assert_eq!(state.selection, 0);
    }

    #[test]
    fn selection_is_clamped_to_the_available_results() {
        let mut state = AppState::new(ResolvedConfig::default());
        state.replace_candidates(vec![Candidate::session(SessionMetadata {
            session_name: "alpha".to_string(),
            attached: false,
            current: true,
            window_count: 1,
            last_activity: Some(10),
        })]);

        state.move_selection(5);

        assert_eq!(state.selection, 0);
    }

    #[test]
    fn confirm_selection_resolves_actions() {
        let mut state = AppState::new(ResolvedConfig::default());
        state.replace_candidates(vec![Candidate::directory(DirectoryMetadata {
            full_path: PathBuf::from("/tmp/wisp"),
            display_path: "/tmp/wisp".to_string(),
            zoxide_score: Some(8.0),
            git_root_hint: None,
            exists: true,
        })]);

        let command = state.handle_intent(UserIntent::ConfirmSelection);

        assert!(matches!(command, Some(AppCommand::ExecuteAction(_))));
    }

    #[test]
    fn stale_preview_results_are_ignored() {
        let mut state = AppState::new(ResolvedConfig::default());
        state.replace_candidates(vec![Candidate::session(SessionMetadata {
            session_name: "alpha".to_string(),
            attached: false,
            current: true,
            window_count: 1,
            last_activity: Some(10),
        })]);

        let command = state.request_preview().expect("preview request");
        let AppCommand::RequestPreview {
            generation,
            request,
        } = command
        else {
            panic!("expected preview request");
        };

        state.apply_preview_result(
            generation + 1,
            request.key(),
            Ok(PreviewContent::from_text("preview", "ignored", 8)),
        );

        assert!(state.preview.content.is_none());

        state.apply_preview_result(
            generation,
            request.key(),
            Ok(PreviewContent::from_text("preview", "accepted", 8)),
        );

        let content = state.preview.content.expect("preview content");
        assert_eq!(content.body, vec!["accepted".to_string()]);
    }

    #[test]
    fn preview_failures_update_status_without_crashing() {
        let mut state = AppState::new(ResolvedConfig::default());
        state.preview.generation = 3;
        state.preview.active_key = Some(PreviewKey::Session("alpha".to_string()));
        state.preview.loading = true;

        state.apply_preview_result(
            3,
            &PreviewKey::Session("alpha".to_string()),
            Err("preview unavailable".to_string()),
        );

        assert_eq!(state.status.level, StatusLevel::Warning);
        assert_eq!(state.status.message, "preview unavailable");
        assert!(!state.preview.loading);
    }

    #[test]
    fn rebuilds_unified_candidates_from_tmux_and_zoxide() {
        let existing = std::env::temp_dir().join("wisp-app-candidate-existing");
        std::fs::create_dir_all(&existing).expect("existing directory");
        let sources = CandidateSources {
            tmux: TmuxSnapshot {
                context: TmuxContext::default(),
                capabilities: TmuxCapabilities {
                    version: TmuxVersion {
                        major: 3,
                        minor: 6,
                        patch: None,
                    },
                    supports_popup: true,
                },
                sessions: vec![TmuxSession {
                    name: "alpha".to_string(),
                    attached: true,
                    windows: 2,
                    current: true,
                    last_activity: Some(5),
                }],
                windows: Vec::new(),
            },
            zoxide: vec![DirectoryEntry {
                path: existing.clone(),
                score: Some(10.0),
                exists: true,
            }],
        };

        let candidates = rebuild_candidates(
            &sources,
            &CandidateBuildOptions {
                home: None,
                include_missing_directories: false,
            },
        );

        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.primary_text == "alpha")
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.primary_text == existing.display().to_string())
        );

        let _ = std::fs::remove_dir_all(existing);
    }

    #[test]
    fn omits_missing_zoxide_directories_by_default() {
        let sources = CandidateSources {
            tmux: TmuxSnapshot {
                context: TmuxContext::default(),
                capabilities: TmuxCapabilities {
                    version: TmuxVersion {
                        major: 3,
                        minor: 6,
                        patch: None,
                    },
                    supports_popup: true,
                },
                sessions: Vec::new(),
                windows: Vec::new(),
            },
            zoxide: vec![DirectoryEntry {
                path: PathBuf::from("/path/that/does/not/exist"),
                score: Some(99.0),
                exists: false,
            }],
        };

        let candidates = rebuild_candidates(&sources, &CandidateBuildOptions::default());

        assert!(candidates.is_empty());
    }
}
