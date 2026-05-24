use std::path::PathBuf;

use wisp_config::{ResolvedConfig, UiMode};
use wisp_core::{
    AlertState, Candidate, CandidateId, ClientFocus, DirectoryRecord, DomainState, PaneRecord,
    PreviewContent, PreviewKey, PreviewRequest, ResolvedAction, SessionRecord, SessionSortKey,
    WindowRecord, deduplicate_candidates, derive_candidates, preview_request_for_candidate,
    resolve_action, sort_candidates,
};
#[cfg(feature = "embers")]
use wisp_embers::{EmbersPane, EmbersSnapshot};
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
pub enum BackendSnapshot {
    Tmux(TmuxSnapshot),
    #[cfg(feature = "embers")]
    Embers(EmbersSnapshot),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CandidateSources {
    pub backend: BackendSnapshot,
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
    let (sessions, clients, previous_session_by_client) = match &sources.backend {
        BackendSnapshot::Tmux(snapshot) => build_tmux_state(snapshot),
        #[cfg(feature = "embers")]
        BackendSnapshot::Embers(snapshot) => build_embers_state(snapshot),
    };
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
        previous_session_by_client,
        directories,
        config: Default::default(),
    };
    state.recompute_aggregates();
    state
}

fn build_tmux_state(
    snapshot: &TmuxSnapshot,
) -> (
    std::collections::BTreeMap<String, SessionRecord>,
    std::collections::BTreeMap<String, ClientFocus>,
    std::collections::BTreeMap<String, String>,
) {
    let sessions = snapshot
        .sessions
        .iter()
        .map(|session| {
            let windows = snapshot
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
                                    current_path: window.current_path.clone(),
                                    current_command: window.current_command.clone(),
                                    is_active: window.active,
                                },
                            )]),
                            alerts: AlertState {
                                activity: window.activity,
                                bell: window.bell,
                                silence: window.silence,
                                unseen_output: false,
                            },
                            has_unseen: false,
                            current_path: window.current_path.clone(),
                            active_command: window.current_command.clone(),
                        },
                    )
                })
                .collect();

            (
                session.name.clone(),
                SessionRecord {
                    id: session.name.clone(),
                    native_id: Some(session.id.clone()),
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
    let clients = snapshot
        .context
        .session_name
        .as_ref()
        .zip(snapshot.context.window_index)
        .map(|(session_name, window_index)| {
            (
                "default".to_string(),
                ClientFocus {
                    session_id: session_name.clone(),
                    window_id: format!("{session_name}:{window_index}"),
                    // Use the same synthetic pane id as the projected panes
                    // (`session:window.1`) so the focus record resolves against
                    // WindowRecord.panes, rather than the raw tmux pane id.
                    pane_id: snapshot
                        .context
                        .pane_id
                        .as_ref()
                        .map(|_| format!("{session_name}:{window_index}.1")),
                },
            )
        })
        .into_iter()
        .collect();
    (sessions, clients, Default::default())
}

#[cfg(feature = "embers")]
fn build_embers_state(
    snapshot: &EmbersSnapshot,
) -> (
    std::collections::BTreeMap<String, SessionRecord>,
    std::collections::BTreeMap<String, ClientFocus>,
    std::collections::BTreeMap<String, String>,
) {
    // Group panes by (session, window) once so window projection is linear in the
    // pane count rather than rescanning the full panes vector per (session, window).
    let mut panes_by_window: std::collections::BTreeMap<(&str, u32), Vec<&EmbersPane>> =
        std::collections::BTreeMap::new();
    for pane in &snapshot.panes {
        panes_by_window
            .entry((pane.session_name.as_str(), pane.window_index))
            .or_default()
            .push(pane);
    }
    let sessions = snapshot
        .sessions
        .iter()
        .map(|session| {
            let windows = snapshot
                .windows
                .iter()
                .filter(|window| window.session_name == session.name)
                .map(|window| {
                    let window_id = format!("{}:{}", session.name, window.index);
                    let panes = panes_by_window
                        .get(&(session.name.as_str(), window.index))
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(|(pane_index, pane)| {
                            (
                                pane.pane_id.clone(),
                                PaneRecord {
                                    id: pane.pane_id.clone(),
                                    index: (pane_index + 1) as i32,
                                    title: Some(pane.title.clone()),
                                    current_path: pane.current_path.clone(),
                                    current_command: pane.current_command.clone(),
                                    is_active: pane.active,
                                },
                            )
                        })
                        .collect();
                    (
                        window_id.clone(),
                        WindowRecord {
                            id: window_id,
                            index: window.index as i32,
                            name: window.name.clone(),
                            active: window.active,
                            panes,
                            alerts: AlertState {
                                activity: window.activity,
                                bell: window.bell,
                                silence: window.silence,
                                unseen_output: false,
                            },
                            has_unseen: false,
                            current_path: window.current_path.clone(),
                            active_command: window.current_command.clone(),
                        },
                    )
                })
                .collect();

            (
                session.name.clone(),
                SessionRecord {
                    id: session.name.clone(),
                    native_id: Some(session.native_id.clone()),
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
    let clients = snapshot
        .context
        .current_session_name
        .as_ref()
        .zip(snapshot.context.current_window_index)
        .map(|(session_name, window_index)| {
            let client_id = snapshot
                .context
                .client_id
                .clone()
                .unwrap_or_else(|| "default".to_string());
            (
                client_id,
                ClientFocus {
                    session_id: session_name.clone(),
                    window_id: format!("{session_name}:{window_index}"),
                    pane_id: snapshot.context.pane_id.clone(),
                },
            )
        })
        .into_iter()
        .collect();
    let previous_session_by_client = snapshot
        .context
        .previous_session_name
        .as_ref()
        .map(|session_name| {
            let client_id = snapshot
                .context
                .client_id
                .clone()
                .unwrap_or_else(|| "default".to_string());
            (client_id, session_name.clone())
        })
        .into_iter()
        .collect();
    (sessions, clients, previous_session_by_client)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wisp_config::ResolvedConfig;
    use wisp_core::{
        AttentionBadge, Candidate, DirectoryMetadata, PreviewContent, PreviewKey, SessionMetadata,
    };
    #[cfg(feature = "embers")]
    use wisp_embers::{
        EmbersActivityState, EmbersContext, EmbersPane, EmbersSession, EmbersSnapshot, EmbersWindow,
    };
    use wisp_tmux::{
        TmuxCapabilities, TmuxContext, TmuxSession, TmuxSnapshot, TmuxVersion, TmuxWindow,
    };
    use wisp_zoxide::DirectoryEntry;

    use crate::{
        AppCommand, AppMode, AppState, BackendSnapshot, CandidateBuildOptions, CandidateSources,
        StatusLevel, UserIntent, build_domain_state, rebuild_candidates,
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
    fn build_domain_state_preserves_tmux_alert_flags() {
        let state = build_domain_state(&CandidateSources {
            backend: BackendSnapshot::Tmux(TmuxSnapshot {
                context: TmuxContext {
                    session_name: Some("alpha".to_string()),
                    window_index: Some(1),
                    ..TmuxContext::default()
                },
                capabilities: TmuxCapabilities {
                    version: TmuxVersion {
                        major: 3,
                        minor: 6,
                        patch: None,
                    },
                    supports_popup: true,
                    supports_multi_status_lines: true,
                    supports_status_mouse_ranges: true,
                    mouse_enabled: true,
                },
                sessions: vec![TmuxSession {
                    id: "$1".to_string(),
                    name: "alpha".to_string(),
                    attached: true,
                    windows: 1,
                    current: true,
                    last_activity: Some(10),
                }],
                windows: vec![TmuxWindow {
                    session_name: "alpha".to_string(),
                    index: 1,
                    name: "shell".to_string(),
                    active: true,
                    activity: false,
                    bell: true,
                    silence: false,
                    current_path: Some(PathBuf::from("/tmp")),
                    current_command: Some("bash".to_string()),
                }],
            }),
            zoxide: Vec::new(),
        });

        assert_eq!(
            state.sessions["alpha"].aggregate_alerts.highest_priority,
            AttentionBadge::Bell
        );
        assert!(state.sessions["alpha"].aggregate_alerts.any_bell);
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
            backend: BackendSnapshot::Tmux(TmuxSnapshot {
                context: TmuxContext::default(),
                capabilities: TmuxCapabilities {
                    version: TmuxVersion {
                        major: 3,
                        minor: 6,
                        patch: None,
                    },
                    supports_popup: true,
                    supports_multi_status_lines: true,
                    supports_status_mouse_ranges: true,
                    mouse_enabled: true,
                },
                sessions: vec![TmuxSession {
                    id: "$1".to_string(),
                    name: "alpha".to_string(),
                    attached: true,
                    windows: 2,
                    current: true,
                    last_activity: Some(5),
                }],
                windows: Vec::new(),
            }),
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
            backend: BackendSnapshot::Tmux(TmuxSnapshot {
                context: TmuxContext::default(),
                capabilities: TmuxCapabilities {
                    version: TmuxVersion {
                        major: 3,
                        minor: 6,
                        patch: None,
                    },
                    supports_popup: true,
                    supports_multi_status_lines: true,
                    supports_status_mouse_ranges: true,
                    mouse_enabled: true,
                },
                sessions: Vec::new(),
                windows: Vec::new(),
            }),
            zoxide: vec![DirectoryEntry {
                path: PathBuf::from("/path/that/does/not/exist"),
                score: Some(99.0),
                exists: false,
            }],
        };

        let candidates = rebuild_candidates(&sources, &CandidateBuildOptions::default());

        assert!(candidates.is_empty());
    }

    #[cfg(feature = "embers")]
    #[test]
    fn build_domain_state_projects_embers_tabs_and_panes() {
        let state = build_domain_state(&CandidateSources {
            backend: BackendSnapshot::Embers(EmbersSnapshot {
                context: EmbersContext {
                    client_id: Some("client-7".to_string()),
                    current_session_name: Some("alpha".to_string()),
                    current_window_index: Some(2),
                    pane_id: Some("201".to_string()),
                    previous_session_name: None,
                },
                sessions: vec![EmbersSession {
                    native_id: "7".to_string(),
                    name: "alpha".to_string(),
                    attached: true,
                    last_activity: None,
                }],
                windows: vec![
                    EmbersWindow {
                        session_name: "alpha".to_string(),
                        index: 1,
                        name: "editor".to_string(),
                        active: false,
                        activity: false,
                        bell: false,
                        silence: false,
                        current_path: Some(PathBuf::from("/tmp/editor")),
                        current_command: Some("nvim".to_string()),
                    },
                    EmbersWindow {
                        session_name: "alpha".to_string(),
                        index: 2,
                        name: "shell".to_string(),
                        active: true,
                        activity: true,
                        bell: false,
                        silence: false,
                        current_path: Some(PathBuf::from("/tmp/shell")),
                        current_command: Some("bash".to_string()),
                    },
                ],
                panes: vec![
                    EmbersPane {
                        session_name: "alpha".to_string(),
                        window_index: 1,
                        pane_id: "101".to_string(),
                        title: "editor".to_string(),
                        active: false,
                        current_path: Some(PathBuf::from("/tmp/editor")),
                        current_command: Some("nvim".to_string()),
                        activity: EmbersActivityState::Idle,
                    },
                    EmbersPane {
                        session_name: "alpha".to_string(),
                        window_index: 2,
                        pane_id: "201".to_string(),
                        title: "shell".to_string(),
                        active: true,
                        current_path: Some(PathBuf::from("/tmp/shell")),
                        current_command: Some("bash".to_string()),
                        activity: EmbersActivityState::Activity,
                    },
                ],
            }),
            zoxide: Vec::new(),
        });

        assert_eq!(
            state.current_session_id(Some("client-7")),
            Some(&"alpha".to_string())
        );
        assert_eq!(state.sessions["alpha"].native_id.as_deref(), Some("7"));
        assert_eq!(state.sessions["alpha"].windows.len(), 2);
        assert_eq!(state.sessions["alpha"].windows["alpha:2"].panes.len(), 1);
        assert!(state.sessions["alpha"].windows["alpha:2"].alerts.activity);
        assert_eq!(
            state.sessions["alpha"].windows["alpha:2"].current_path,
            Some(PathBuf::from("/tmp/shell"))
        );
    }
}
