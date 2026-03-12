use std::{collections::BTreeMap, path::PathBuf};

pub type SessionId = String;
pub type WindowId = String;
pub type PaneId = String;
pub type ClientId = String;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DomainState {
    pub sessions: BTreeMap<SessionId, SessionRecord>,
    pub clients: BTreeMap<ClientId, ClientFocus>,
    pub previous_session_by_client: BTreeMap<ClientId, SessionId>,
    pub directories: Vec<DirectoryRecord>,
    pub config: DomainConfig,
}

impl DomainState {
    pub fn recompute_aggregates(&mut self) {
        let session_ids = self.sessions.keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            self.recompute_session_aggregate(&session_id);
        }
    }

    #[must_use]
    pub fn current_session_id(&self, client_id: Option<&str>) -> Option<&SessionId> {
        match client_id {
            Some(client_id) => self.clients.get(client_id).map(|focus| &focus.session_id),
            None => self.clients.values().next().map(|focus| &focus.session_id),
        }
    }

    #[must_use]
    pub fn previous_session_id(&self, client_id: Option<&str>) -> Option<&SessionId> {
        match client_id {
            Some(client_id) => self.previous_session_by_client.get(client_id),
            None => self.previous_session_by_client.values().next(),
        }
    }

    #[must_use]
    pub fn focused_session_for_window(&self, window_id: &str) -> Option<SessionId> {
        self.clients.values().find_map(|focus| {
            if focus.window_id == window_id {
                Some(focus.session_id.clone())
            } else {
                None
            }
        })
    }

    #[must_use]
    pub fn session_id_for_window(&self, window_id: &str) -> Option<SessionId> {
        self.sessions.iter().find_map(|(session_id, session)| {
            if session.windows.contains_key(window_id) {
                Some(session_id.clone())
            } else {
                None
            }
        })
    }

    #[must_use]
    pub fn session_window_for_pane(&self, pane_id: &str) -> Option<(SessionId, WindowId)> {
        self.sessions.iter().find_map(|(session_id, session)| {
            session.windows.iter().find_map(|(window_id, window)| {
                if window.panes.contains_key(pane_id) {
                    Some((session_id.clone(), window_id.clone()))
                } else {
                    None
                }
            })
        })
    }

    pub fn clear_unseen_for_window(&mut self, session_id: &str, window_id: &str) {
        if let Some(window) = self
            .sessions
            .get_mut(session_id)
            .and_then(|session| session.windows.get_mut(window_id))
        {
            window.has_unseen = false;
            window.alerts.unseen_output = false;
        }
        self.recompute_session_aggregate(session_id);
    }

    pub fn recompute_session_aggregate(&mut self, session_id: &str) {
        let Some(session) = self.sessions.get_mut(session_id) else {
            return;
        };

        session.has_unseen = session.windows.values().any(|window| window.has_unseen);
        session.aggregate_alerts = aggregate_alerts(session.windows.values());
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainConfig {
    pub notifications: NotificationConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationConfig {
    pub track_unseen_output: bool,
    pub clear_on_focus: bool,
    pub show_silence: bool,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            track_unseen_output: true,
            clear_on_focus: true,
            show_silence: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DomainSnapshot {
    pub sessions: BTreeMap<SessionId, SessionRecord>,
    pub clients: BTreeMap<ClientId, ClientFocus>,
    pub directories: Vec<DirectoryRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub name: String,
    pub attached: bool,
    pub windows: BTreeMap<WindowId, WindowRecord>,
    pub aggregate_alerts: AlertAggregate,
    pub has_unseen: bool,
    pub sort_key: SessionSortKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionSortKey {
    pub last_activity: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRecord {
    pub id: WindowId,
    pub index: i32,
    pub name: String,
    pub active: bool,
    pub panes: BTreeMap<PaneId, PaneRecord>,
    pub alerts: AlertState,
    pub has_unseen: bool,
    pub current_path: Option<PathBuf>,
    pub active_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneRecord {
    pub id: PaneId,
    pub index: i32,
    pub title: Option<String>,
    pub current_path: Option<PathBuf>,
    pub current_command: Option<String>,
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientFocus {
    pub session_id: SessionId,
    pub window_id: WindowId,
    pub pane_id: Option<PaneId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryRecord {
    pub path: PathBuf,
    pub score: Option<f64>,
    pub exists: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AlertState {
    pub activity: bool,
    pub bell: bool,
    pub silence: bool,
    pub unseen_output: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertAggregate {
    pub any_activity: bool,
    pub any_bell: bool,
    pub any_silence: bool,
    pub any_unseen: bool,
    pub attention_count: usize,
    pub highest_priority: AttentionBadge,
}

impl Default for AlertAggregate {
    fn default() -> Self {
        Self {
            any_activity: false,
            any_bell: false,
            any_silence: false,
            any_unseen: false,
            attention_count: 0,
            highest_priority: AttentionBadge::None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum AttentionBadge {
    #[default]
    None,
    Silence,
    Unseen,
    Activity,
    Bell,
}

impl AttentionBadge {
    #[must_use]
    pub fn from_alerts(alerts: AlertState) -> Self {
        if alerts.bell {
            Self::Bell
        } else if alerts.activity {
            Self::Activity
        } else if alerts.unseen_output {
            Self::Unseen
        } else if alerts.silence {
            Self::Silence
        } else {
            Self::None
        }
    }
}

#[must_use]
pub fn aggregate_alerts<'a>(windows: impl IntoIterator<Item = &'a WindowRecord>) -> AlertAggregate {
    let mut aggregate = AlertAggregate::default();

    for window in windows {
        aggregate.any_activity |= window.alerts.activity;
        aggregate.any_bell |= window.alerts.bell;
        aggregate.any_silence |= window.alerts.silence;
        aggregate.any_unseen |= window.alerts.unseen_output;

        let badge = AttentionBadge::from_alerts(window.alerts);
        if badge != AttentionBadge::None {
            aggregate.attention_count += 1;
            if badge > aggregate.highest_priority {
                aggregate.highest_priority = badge;
            }
        }
    }

    aggregate
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        AlertState, AttentionBadge, DomainState, PaneRecord, SessionRecord, SessionSortKey,
        WindowRecord, aggregate_alerts,
    };

    #[test]
    fn aggregates_alerts_using_consistent_priority() {
        let windows = BTreeMap::from([
            (
                "w1".to_string(),
                WindowRecord {
                    id: "w1".to_string(),
                    index: 1,
                    name: "dev".to_string(),
                    active: true,
                    panes: BTreeMap::new(),
                    alerts: AlertState {
                        activity: true,
                        ..AlertState::default()
                    },
                    has_unseen: false,
                    current_path: None,
                    active_command: None,
                },
            ),
            (
                "w2".to_string(),
                WindowRecord {
                    id: "w2".to_string(),
                    index: 2,
                    name: "ops".to_string(),
                    active: false,
                    panes: BTreeMap::from([(
                        "p1".to_string(),
                        PaneRecord {
                            id: "p1".to_string(),
                            index: 1,
                            title: None,
                            current_path: None,
                            current_command: None,
                            is_active: false,
                        },
                    )]),
                    alerts: AlertState {
                        bell: true,
                        unseen_output: true,
                        ..AlertState::default()
                    },
                    has_unseen: true,
                    current_path: None,
                    active_command: None,
                },
            ),
        ]);

        let aggregate = aggregate_alerts(windows.values());

        assert_eq!(aggregate.attention_count, 2);
        assert_eq!(aggregate.highest_priority, AttentionBadge::Bell);
        assert!(aggregate.any_unseen);
    }

    #[test]
    fn recomputes_session_aggregates_from_window_state() {
        let mut state = DomainState {
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
                            alerts: AlertState {
                                unseen_output: true,
                                ..AlertState::default()
                            },
                            has_unseen: true,
                            current_path: None,
                            active_command: None,
                        },
                    )]),
                    aggregate_alerts: Default::default(),
                    has_unseen: false,
                    sort_key: SessionSortKey::default(),
                },
            )]),
            ..DomainState::default()
        };

        state.recompute_aggregates();

        assert!(state.sessions["alpha"].has_unseen);
        assert_eq!(
            state.sessions["alpha"].aggregate_alerts.highest_priority,
            AttentionBadge::Unseen
        );
    }
}
