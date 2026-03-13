use std::path::Path;

use crate::{
    AttentionBadge, Candidate, DirectoryMetadata, DirectoryRecord, DomainState, SessionMetadata,
    deduplicate_candidates, normalize_display_path, sort_candidates,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionListItem {
    pub session_id: String,
    pub label: String,
    pub is_current: bool,
    pub is_previous: bool,
    pub attached: bool,
    pub attention: AttentionBadge,
    pub attention_count: usize,
    pub active_window_label: Option<String>,
    pub path_hint: Option<String>,
    pub command_hint: Option<String>,
    pub git_branch: Option<GitBranchStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitBranchStatus {
    pub name: String,
    pub pushed: bool,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSessionItem {
    pub label: String,
    pub is_current: bool,
    pub is_previous: bool,
    pub badge: AttentionBadge,
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
                is_current: current == Some(session_id),
                is_previous: previous == Some(session_id),
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
            }
        })
        .collect::<Vec<_>>();

    items.sort_by(|left, right| {
        right
            .is_current
            .cmp(&left.is_current)
            .then_with(|| right.is_previous.cmp(&left.is_previous))
            .then_with(|| right.attention.cmp(&left.attention))
            .then_with(|| left.label.cmp(&right.label))
    });
    items
}

#[must_use]
pub fn derive_status_items(state: &DomainState, client_id: Option<&str>) -> Vec<StatusSessionItem> {
    derive_session_list(state, client_id)
        .into_iter()
        .map(|item| StatusSessionItem {
            label: item.label,
            is_current: item.is_current,
            is_previous: item.is_previous,
            badge: item.attention,
        })
        .collect()
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
        AlertAggregate, AttentionBadge, ClientFocus, DirectoryRecord, DomainState, SessionRecord,
        SessionSortKey, WindowRecord, derive_candidates, derive_session_list, derive_status_items,
    };

    fn seeded_state() -> DomainState {
        DomainState {
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

        assert_eq!(items[0].label, "alpha");
        assert!(items[0].is_current);
    }
}
