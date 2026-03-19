use std::path::{Path, PathBuf};

use crate::{
    AttentionBadge, Candidate, DirectoryMetadata, DirectoryRecord, DomainState, SessionMetadata,
    deduplicate_candidates, normalize_display_path, sort_candidates,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListItem {
    pub session_id: String,
    pub label: String,
    pub kind: SessionListItemKind,
    pub is_current: bool,
    pub is_previous: bool,
    pub last_activity: Option<u64>,
    pub attached: bool,
    pub attention: AttentionBadge,
    pub attention_count: usize,
    pub active_window_label: Option<String>,
    pub path_hint: Option<String>,
    pub command_hint: Option<String>,
    pub git_branch: Option<GitBranchStatus>,
    pub worktree_path: Option<PathBuf>,
    pub worktree_branch: Option<String>,
}

impl SessionListItem {
    #[must_use]
    pub fn picker_search_text(&self) -> String {
        [
            Some(self.label.as_str()),
            self.active_window_label.as_deref(),
            self.path_hint.as_deref(),
            self.command_hint.as_deref(),
            self.git_branch.as_ref().map(|branch| branch.name.as_str()),
            self.worktree_branch.as_deref(),
        ]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBranchStatus {
    pub name: String,
    pub sync: GitBranchSync,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_locked: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PickerMode {
    #[default]
    AllSessions,
    Worktree,
}

/// Synchronization state for a git branch in the UI.
///
/// The current status pipeline only distinguishes whether the branch still needs to be pushed.
/// Branches that are only behind their upstream remain [`GitBranchSync::Pushed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitBranchSync {
    Unknown,
    Pushed,
    NotPushed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionListItemKind {
    Info,            // informational row, not an actionable session
    Session,         // regular tmux session (not in a worktree)
    WorktreeSession, // session running in a worktree
    Worktree,        // worktree with no session
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSessionItem {
    pub session_id: String,
    pub session_name: String,
    pub is_current: bool,
    pub is_previous: bool,
    pub badge: AttentionBadge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionListSortMode {
    Recent,
    Alphabetical,
}

#[must_use]
pub fn derive_candidates(
    state: &DomainState,
    home: Option<&Path>,
    include_missing_directories: bool,
) -> Vec<Candidate> {
    let mut candidates = state
        .sessions
        .iter()
        .map(|(session_id, session)| {
            Candidate::session(SessionMetadata {
                session_name: session.name.clone(),
                attached: session.attached,
                current: state.current_session_id(None) == Some(session_id),
                window_count: session.windows.len(),
                last_activity: session.sort_key.last_activity,
            })
        })
        .collect::<Vec<_>>();

    candidates.extend(
        state
            .directories
            .iter()
            .filter(|entry| include_missing_directories || entry.exists)
            .map(|entry| directory_candidate(entry, home)),
    );

    let mut candidates = deduplicate_candidates(candidates);
    sort_candidates(&mut candidates);
    candidates
}

#[must_use]
pub fn derive_session_list(state: &DomainState, client_id: Option<&str>) -> Vec<SessionListItem> {
    let current = state.current_session_id(client_id);
    let previous = state.previous_session_id(client_id);

    let mut items = state
        .sessions
        .iter()
        .map(|(session_id, session)| {
            let active_window = session
                .windows
                .values()
                .find(|window| window.active)
                .or_else(|| session.windows.values().next());

            SessionListItem {
                session_id: session_id.clone(),
                label: session.name.clone(),
                kind: SessionListItemKind::Session,
                is_current: current == Some(session_id),
                is_previous: previous == Some(session_id),
                last_activity: session.sort_key.last_activity,
                attached: session.attached,
                attention: session.aggregate_alerts.highest_priority,
                attention_count: session.aggregate_alerts.attention_count,
                active_window_label: active_window.map(|window| window.name.clone()),
                path_hint: active_window.and_then(|window| {
                    window
                        .current_path
                        .as_deref()
                        .map(|path| normalize_display_path(path, None))
                }),
                command_hint: active_window.and_then(|window| window.active_command.clone()),
                git_branch: None,
                worktree_path: None,
                worktree_branch: None,
            }
        })
        .collect::<Vec<_>>();

    sort_session_list_items(&mut items, SessionListSortMode::Recent);
    items
}

/// Derives a session list that shows only worktree-related items.
/// - Sessions in worktrees are shown as WorktreeSession
/// - Worktrees without sessions are shown as Worktree
/// - Regular sessions (not in any worktree) are excluded
pub fn derive_session_list_with_worktrees(
    state: &DomainState,
    client_id: Option<&str>,
    worktrees: &[WorktreeInfo],
) -> Vec<SessionListItem> {
    use std::collections::{BTreeMap, BTreeSet};

    let current = state.current_session_id(client_id);
    let previous = state.previous_session_id(client_id);

    // Build a map of worktree path -> worktree info for matching
    let worktree_map: BTreeMap<&Path, &WorktreeInfo> =
        worktrees.iter().map(|w| (w.path.as_path(), w)).collect();

    // Find which worktree a path belongs to (if any)
    fn find_worktree_for_path<'a>(
        path: &Path,
        worktree_map: &'a BTreeMap<&Path, &WorktreeInfo>,
    ) -> Option<&'a WorktreeInfo> {
        worktree_map
            .iter()
            .filter(|(wt_path, _)| path == **wt_path || path.starts_with(*wt_path))
            .max_by_key(|(wt_path, _)| wt_path.as_os_str().len())
            .map(|(_, wt)| *wt)
    }

    // Process sessions: only include if they match a worktree
    let mut items: Vec<SessionListItem> = state
        .sessions
        .iter()
        .filter_map(|(session_id, session)| {
            let active_window = session
                .windows
                .values()
                .find(|window| window.active)
                .or_else(|| session.windows.values().next());

            let current_path = active_window.and_then(|w| w.current_path.as_deref());

            let worktree = current_path.and_then(|p| find_worktree_for_path(p, &worktree_map))?;

            Some(SessionListItem {
                session_id: session_id.clone(),
                label: session.name.clone(),
                kind: SessionListItemKind::WorktreeSession,
                is_current: current == Some(session_id),
                is_previous: previous == Some(session_id),
                last_activity: session.sort_key.last_activity,
                attached: session.attached,
                attention: session.aggregate_alerts.highest_priority,
                attention_count: session.aggregate_alerts.attention_count,
                active_window_label: active_window.map(|window| window.name.clone()),
                path_hint: active_window.and_then(|window| {
                    window
                        .current_path
                        .as_deref()
                        .map(|path| normalize_display_path(path, None))
                }),
                command_hint: active_window.and_then(|window| window.active_command.clone()),
                git_branch: Some(GitBranchStatus {
                    name: worktree
                        .branch
                        .clone()
                        .unwrap_or_else(|| "(detached)".to_string()),
                    sync: GitBranchSync::Unknown,
                    dirty: false,
                }),
                worktree_path: Some(worktree.path.clone()),
                worktree_branch: worktree.branch.clone(),
            })
        })
        .collect();

    // Add worktrees that don't have matching sessions
    let session_paths: BTreeSet<&Path> = state
        .sessions
        .iter()
        .filter_map(|(_, session)| {
            session
                .windows
                .values()
                .find(|window| window.active)
                .or_else(|| session.windows.values().next())
                .and_then(|w| w.current_path.as_deref())
        })
        .collect();

    for worktree in worktrees {
        let matched_session = session_paths.iter().any(|path| {
            *path == worktree.path.as_path() || path.starts_with(worktree.path.as_path())
        });

        if !matched_session {
            let basename = worktree
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();

            items.push(SessionListItem {
                session_id: format!("worktree:{}", worktree.path.display()),
                label: basename,
                kind: SessionListItemKind::Worktree,
                is_current: false,
                is_previous: false,
                last_activity: None,
                attached: false,
                attention: AttentionBadge::None,
                attention_count: 0,
                active_window_label: None,
                path_hint: Some(normalize_display_path(&worktree.path, None)),
                command_hint: None,
                git_branch: Some(GitBranchStatus {
                    name: worktree.branch.clone().unwrap_or_default(),
                    sync: GitBranchSync::Unknown,
                    dirty: false,
                }),
                worktree_path: Some(worktree.path.clone()),
                worktree_branch: worktree.branch.clone(),
            });
        }
    }

    sort_session_list_items(&mut items, SessionListSortMode::Recent);
    items
}

#[must_use]
pub fn derive_status_items(state: &DomainState, client_id: Option<&str>) -> Vec<StatusSessionItem> {
    let current = state.current_session_id(client_id);
    let previous = state.previous_session_id(client_id);

    let mut items = state
        .sessions
        .iter()
        .map(|(session_id, session)| StatusSessionItem {
            session_id: session
                .tmux_id
                .clone()
                .unwrap_or_else(|| session_id.clone()),
            session_name: session.name.clone(),
            is_current: current == Some(session_id),
            is_previous: previous == Some(session_id),
            badge: session.aggregate_alerts.highest_priority,
        })
        .collect::<Vec<_>>();

    items.sort_by(|left, right| left.session_name.cmp(&right.session_name));
    items
}

pub fn sort_session_list_items(items: &mut [SessionListItem], mode: SessionListSortMode) {
    match mode {
        SessionListSortMode::Recent => items.sort_by(recent_session_cmp),
        SessionListSortMode::Alphabetical => {
            items.sort_by(|left, right| left.label.cmp(&right.label));
        }
    }
}

fn recent_session_cmp(left: &SessionListItem, right: &SessionListItem) -> std::cmp::Ordering {
    right
        .is_current
        .cmp(&left.is_current)
        .then_with(|| right.is_previous.cmp(&left.is_previous))
        .then_with(|| right.last_activity.cmp(&left.last_activity))
        .then_with(|| right.attention.cmp(&left.attention))
        .then_with(|| left.label.cmp(&right.label))
}

fn directory_candidate(entry: &DirectoryRecord, home: Option<&Path>) -> Candidate {
    Candidate::directory(DirectoryMetadata {
        full_path: entry.path.clone(),
        display_path: normalize_display_path(&entry.path, home),
        zoxide_score: entry.score,
        git_root_hint: None,
        exists: entry.exists,
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use crate::{
        AlertAggregate, AttentionBadge, ClientFocus, DirectoryRecord, DomainState, GitBranchStatus,
        GitBranchSync, SessionListItem, SessionListItemKind, SessionListSortMode, SessionRecord,
        SessionSortKey, WindowRecord, WorktreeInfo, derive_candidates, derive_session_list,
        derive_session_list_with_worktrees, derive_status_items, sort_session_list_items,
    };

    fn seeded_state() -> DomainState {
        DomainState {
            sessions: BTreeMap::from([(
                "alpha".to_string(),
                SessionRecord {
                    id: "alpha".to_string(),
                    tmux_id: Some("$1".to_string()),
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
                            current_path: Some(PathBuf::from("/tmp/alpha")),
                            active_command: Some("nvim".to_string()),
                        },
                    )]),
                    aggregate_alerts: AlertAggregate {
                        any_activity: true,
                        any_bell: false,
                        any_silence: false,
                        any_unseen: false,
                        attention_count: 1,
                        highest_priority: AttentionBadge::Activity,
                    },
                    has_unseen: false,
                    sort_key: SessionSortKey {
                        last_activity: Some(10),
                    },
                },
            )]),
            clients: BTreeMap::from([(
                "client-1".to_string(),
                ClientFocus {
                    session_id: "alpha".to_string(),
                    window_id: "alpha:1".to_string(),
                    pane_id: None,
                },
            )]),
            previous_session_by_client: BTreeMap::from([(
                "client-1".to_string(),
                "beta".to_string(),
            )]),
            directories: vec![DirectoryRecord {
                path: PathBuf::from("/tmp/project"),
                score: Some(5.0),
                exists: true,
            }],
            ..DomainState::default()
        }
    }

    #[test]
    fn derives_candidates_from_canonical_state() {
        let candidates = derive_candidates(&seeded_state(), None, false);

        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.primary_text == "alpha")
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.primary_text == "/tmp/project")
        );
    }

    #[test]
    fn derives_session_list_markers() {
        let items = derive_session_list(&seeded_state(), Some("client-1"));

        assert_eq!(items.len(), 1);
        assert!(items[0].is_current);
        assert_eq!(items[0].attention, AttentionBadge::Activity);
    }

    #[test]
    fn derives_status_items_from_session_projection() {
        let items = derive_status_items(&seeded_state(), Some("client-1"));

        assert_eq!(items[0].session_id, "$1");
        assert_eq!(items[0].session_name, "alpha");
        assert!(items[0].is_current);
    }

    #[test]
    fn derives_status_items_in_alphabetical_order() {
        let mut state = seeded_state();
        state.sessions.insert(
            "beta".to_string(),
            SessionRecord {
                id: "beta".to_string(),
                tmux_id: Some("$2".to_string()),
                name: "beta".to_string(),
                attached: false,
                windows: BTreeMap::new(),
                aggregate_alerts: AlertAggregate::default(),
                has_unseen: false,
                sort_key: SessionSortKey::default(),
            },
        );
        state.sessions.insert(
            "aardvark".to_string(),
            SessionRecord {
                id: "aardvark".to_string(),
                tmux_id: Some("$3".to_string()),
                name: "aardvark".to_string(),
                attached: false,
                windows: BTreeMap::new(),
                aggregate_alerts: AlertAggregate::default(),
                has_unseen: false,
                sort_key: SessionSortKey::default(),
            },
        );

        let items = derive_status_items(&state, Some("client-1"));
        let names = items
            .into_iter()
            .map(|item| item.session_name)
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["aardvark", "alpha", "beta"]);
    }

    #[test]
    fn sorts_session_lists_by_requested_mode() {
        let mut items = vec![
            SessionListItem {
                session_id: "beta".to_string(),
                label: "beta".to_string(),
                kind: SessionListItemKind::Session,
                is_current: false,
                is_previous: true,
                last_activity: Some(2),
                attached: false,
                attention: AttentionBadge::None,
                attention_count: 0,
                active_window_label: None,
                path_hint: None,
                command_hint: None,
                git_branch: None,
                worktree_path: None,
                worktree_branch: None,
            },
            SessionListItem {
                session_id: "alpha".to_string(),
                label: "alpha".to_string(),
                kind: SessionListItemKind::Session,
                is_current: true,
                is_previous: false,
                last_activity: Some(3),
                attached: false,
                attention: AttentionBadge::None,
                attention_count: 0,
                active_window_label: None,
                path_hint: None,
                command_hint: None,
                git_branch: None,
                worktree_path: None,
                worktree_branch: None,
            },
            SessionListItem {
                session_id: "aardvark".to_string(),
                label: "aardvark".to_string(),
                kind: SessionListItemKind::Session,
                is_current: false,
                is_previous: false,
                last_activity: Some(1),
                attached: false,
                attention: AttentionBadge::None,
                attention_count: 0,
                active_window_label: None,
                path_hint: None,
                command_hint: None,
                git_branch: None,
                worktree_path: None,
                worktree_branch: None,
            },
        ];

        sort_session_list_items(&mut items, SessionListSortMode::Recent);
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "aardvark"]
        );

        sort_session_list_items(&mut items, SessionListSortMode::Alphabetical);
        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["aardvark", "alpha", "beta"]
        );
    }

    #[test]
    fn picker_search_text_includes_git_branch_name() {
        let item = SessionListItem {
            session_id: "alpha".to_string(),
            label: "alpha".to_string(),
            kind: SessionListItemKind::Session,
            is_current: false,
            is_previous: false,
            last_activity: None,
            attached: false,
            attention: AttentionBadge::None,
            attention_count: 0,
            active_window_label: Some("editor".to_string()),
            path_hint: None,
            command_hint: Some("nvim".to_string()),
            git_branch: Some(GitBranchStatus {
                name: "feature/picker-branches".to_string(),
                sync: GitBranchSync::Unknown,
                dirty: false,
            }),
            worktree_path: None,
            worktree_branch: None,
        };

        assert_eq!(
            item.picker_search_text(),
            "alpha editor nvim feature/picker-branches"
        );
    }

    #[test]
    fn picker_search_text_includes_path_hint() {
        let item = SessionListItem {
            session_id: "worktree:/tmp/demo/app".to_string(),
            label: "app".to_string(),
            kind: SessionListItemKind::Worktree,
            is_current: false,
            is_previous: false,
            last_activity: None,
            attached: false,
            attention: AttentionBadge::None,
            attention_count: 0,
            active_window_label: None,
            path_hint: Some("~/src/demo/app".to_string()),
            command_hint: None,
            git_branch: None,
            worktree_path: Some(PathBuf::from("/tmp/demo/app")),
            worktree_branch: Some("feature/demo".to_string()),
        };

        assert_eq!(item.picker_search_text(), "app ~/src/demo/app feature/demo");
    }

    #[test]
    fn omits_worktree_rows_when_a_session_path_is_nested_inside_the_worktree() {
        let state = seeded_state();
        let worktrees = vec![WorktreeInfo {
            path: PathBuf::from("/tmp"),
            branch: Some("main".to_string()),
            is_locked: false,
        }];

        let items = derive_session_list_with_worktrees(&state, Some("client-1"), &worktrees);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, SessionListItemKind::WorktreeSession);
        assert_eq!(
            items[0].worktree_path.as_deref(),
            Some(std::path::Path::new("/tmp"))
        );
    }

    #[test]
    fn picks_the_deepest_matching_worktree_for_nested_paths() {
        let state = seeded_state();
        let worktrees = vec![
            WorktreeInfo {
                path: PathBuf::from("/tmp"),
                branch: Some("root".to_string()),
                is_locked: false,
            },
            WorktreeInfo {
                path: PathBuf::from("/tmp/alpha"),
                branch: Some("nested".to_string()),
                is_locked: false,
            },
        ];

        let items = derive_session_list_with_worktrees(&state, Some("client-1"), &worktrees);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, SessionListItemKind::WorktreeSession);
        assert_eq!(
            items[0].worktree_path.as_deref(),
            Some(std::path::Path::new("/tmp/alpha"))
        );
        assert_eq!(items[0].worktree_branch.as_deref(), Some("nested"));
    }

    #[test]
    fn detached_worktrees_still_get_git_branch_status_placeholders() {
        let state = seeded_state();
        let worktrees = vec![WorktreeInfo {
            path: PathBuf::from("/tmp/detached"),
            branch: None,
            is_locked: false,
        }];

        let items = derive_session_list_with_worktrees(&state, Some("client-1"), &worktrees);
        let detached = items
            .into_iter()
            .find(|item| item.kind == SessionListItemKind::Worktree)
            .expect("detached worktree row");

        assert_eq!(
            detached.git_branch,
            Some(GitBranchStatus {
                name: String::new(),
                sync: GitBranchSync::Unknown,
                dirty: false,
            })
        );
    }

    #[test]
    fn sorts_worktree_projection_with_recent_ordering() {
        let state = DomainState::default();
        let worktrees = vec![
            WorktreeInfo {
                path: PathBuf::from("/tmp/zeta"),
                branch: Some("zeta".to_string()),
                is_locked: false,
            },
            WorktreeInfo {
                path: PathBuf::from("/tmp/alpha"),
                branch: Some("alpha".to_string()),
                is_locked: false,
            },
        ];

        let items = derive_session_list_with_worktrees(&state, None, &worktrees);

        assert_eq!(
            items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"]
        );
    }
}
