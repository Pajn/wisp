use crate::{AlertState, ClientFocus, DirectoryRecord, DomainSnapshot, DomainState};

#[derive(Debug, Clone, PartialEq)]
pub enum DomainEvent {
    SnapshotLoaded(DomainSnapshot),
    FocusChanged {
        client_id: String,
        focus: ClientFocus,
    },
    AlertChanged {
        window_id: String,
        alerts: AlertState,
    },
    OutputChanged {
        pane_id: String,
    },
    DirectoriesUpdated(Vec<DirectoryRecord>),
}

pub fn reduce_domain_event(state: &mut DomainState, event: DomainEvent) {
    match event {
        DomainEvent::SnapshotLoaded(snapshot) => {
            let mut previous = state.previous_session_by_client.clone();
            for (client_id, next_focus) in &snapshot.clients {
                if let Some(current_focus) = state.clients.get(client_id)
                    && current_focus.session_id != next_focus.session_id
                {
                    previous.insert(client_id.clone(), current_focus.session_id.clone());
                }
            }

            state.sessions = snapshot.sessions;
            state.clients = snapshot.clients;
            state.directories = snapshot.directories;
            state.previous_session_by_client = previous;
            state.recompute_aggregates();
        }
        DomainEvent::FocusChanged { client_id, focus } => {
            if let Some(current_focus) = state.clients.get(&client_id)
                && current_focus.session_id != focus.session_id
            {
                state
                    .previous_session_by_client
                    .insert(client_id.clone(), current_focus.session_id.clone());
            }

            let session_id = focus.session_id.clone();
            let window_id = focus.window_id.clone();
            state.clients.insert(client_id, focus);
            if state.config.notifications.clear_on_focus {
                state.clear_unseen_for_window(&session_id, &window_id);
            } else {
                state.recompute_session_aggregate(&session_id);
            }
        }
        DomainEvent::AlertChanged { window_id, alerts } => {
            if let Some(session_id) = state.session_id_for_window(&window_id) {
                if let Some(window) = state
                    .sessions
                    .get_mut(&session_id)
                    .and_then(|session| session.windows.get_mut(&window_id))
                {
                    window.alerts = AlertState {
                        unseen_output: window.alerts.unseen_output,
                        ..alerts
                    };
                }
                state.recompute_session_aggregate(&session_id);
            }
        }
        DomainEvent::OutputChanged { pane_id } => {
            if !state.config.notifications.track_unseen_output {
                return;
            }

            if let Some((session_id, window_id)) = state.session_window_for_pane(&pane_id) {
                let focused_session = state.focused_session_for_window(&window_id);
                if focused_session.as_deref() == Some(session_id.as_str()) {
                    return;
                }

                if let Some(window) = state
                    .sessions
                    .get_mut(&session_id)
                    .and_then(|session| session.windows.get_mut(&window_id))
                {
                    window.has_unseen = true;
                    window.alerts.unseen_output = true;
                }
                state.recompute_session_aggregate(&session_id);
            }
        }
        DomainEvent::DirectoriesUpdated(entries) => {
            state.directories = entries;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        AlertState, AttentionBadge, ClientFocus, DomainConfig, DomainEvent, DomainState,
        NotificationConfig, PaneRecord, SessionRecord, SessionSortKey, WindowRecord,
        reduce_domain_event,
    };

    fn seeded_state() -> DomainState {
        DomainState {
            sessions: BTreeMap::from([(
                "alpha".to_string(),
                SessionRecord {
                    id: "alpha".to_string(),
                    tmux_id: None,
                    name: "alpha".to_string(),
                    attached: true,
                    windows: BTreeMap::from([(
                        "alpha:1".to_string(),
                        WindowRecord {
                            id: "alpha:1".to_string(),
                            index: 1,
                            name: "shell".to_string(),
                            active: true,
                            panes: BTreeMap::from([(
                                "alpha:1.1".to_string(),
                                PaneRecord {
                                    id: "alpha:1.1".to_string(),
                                    index: 1,
                                    title: None,
                                    current_path: None,
                                    current_command: None,
                                    is_active: true,
                                },
                            )]),
                            alerts: AlertState::default(),
                            has_unseen: false,
                            current_path: None,
                            active_command: None,
                        },
                    )]),
                    aggregate_alerts: Default::default(),
                    has_unseen: false,
                    sort_key: SessionSortKey::default(),
                },
            )]),
            clients: BTreeMap::from([(
                "client-1".to_string(),
                ClientFocus {
                    session_id: "alpha".to_string(),
                    window_id: "alpha:1".to_string(),
                    pane_id: Some("alpha:1.1".to_string()),
                },
            )]),
            previous_session_by_client: BTreeMap::new(),
            directories: Vec::new(),
            config: DomainConfig {
                notifications: NotificationConfig {
                    track_unseen_output: true,
                    clear_on_focus: true,
                    show_silence: true,
                },
            },
        }
    }

    #[test]
    fn tracks_previous_session_per_client() {
        let mut state = seeded_state();
        state.sessions.insert(
            "beta".to_string(),
            SessionRecord {
                id: "beta".to_string(),
                tmux_id: None,
                name: "beta".to_string(),
                attached: false,
                windows: BTreeMap::from([(
                    "beta:1".to_string(),
                    WindowRecord {
                        id: "beta:1".to_string(),
                        index: 1,
                        name: "editor".to_string(),
                        active: true,
                        panes: BTreeMap::new(),
                        alerts: AlertState::default(),
                        has_unseen: false,
                        current_path: None,
                        active_command: None,
                    },
                )]),
                aggregate_alerts: Default::default(),
                has_unseen: false,
                sort_key: SessionSortKey::default(),
            },
        );

        reduce_domain_event(
            &mut state,
            DomainEvent::FocusChanged {
                client_id: "client-1".to_string(),
                focus: ClientFocus {
                    session_id: "beta".to_string(),
                    window_id: "beta:1".to_string(),
                    pane_id: None,
                },
            },
        );

        assert_eq!(
            state.previous_session_by_client.get("client-1"),
            Some(&"alpha".to_string())
        );
    }

    #[test]
    fn marks_unseen_output_on_non_focused_windows() {
        let mut state = seeded_state();
        state.sessions.insert(
            "beta".to_string(),
            SessionRecord {
                id: "beta".to_string(),
                tmux_id: None,
                name: "beta".to_string(),
                attached: false,
                windows: BTreeMap::from([(
                    "beta:1".to_string(),
                    WindowRecord {
                        id: "beta:1".to_string(),
                        index: 1,
                        name: "logs".to_string(),
                        active: true,
                        panes: BTreeMap::from([(
                            "beta:1.1".to_string(),
                            PaneRecord {
                                id: "beta:1.1".to_string(),
                                index: 1,
                                title: None,
                                current_path: None,
                                current_command: None,
                                is_active: true,
                            },
                        )]),
                        alerts: AlertState::default(),
                        has_unseen: false,
                        current_path: None,
                        active_command: None,
                    },
                )]),
                aggregate_alerts: Default::default(),
                has_unseen: false,
                sort_key: SessionSortKey::default(),
            },
        );

        reduce_domain_event(
            &mut state,
            DomainEvent::OutputChanged {
                pane_id: "beta:1.1".to_string(),
            },
        );

        assert!(state.sessions["beta"].has_unseen);
        assert_eq!(
            state.sessions["beta"].aggregate_alerts.highest_priority,
            AttentionBadge::Unseen
        );
    }

    #[test]
    fn clears_unseen_on_focus_when_configured() {
        let mut state = seeded_state();
        state.sessions.insert(
            "beta".to_string(),
            SessionRecord {
                id: "beta".to_string(),
                tmux_id: None,
                name: "beta".to_string(),
                attached: false,
                windows: BTreeMap::from([(
                    "beta:1".to_string(),
                    WindowRecord {
                        id: "beta:1".to_string(),
                        index: 1,
                        name: "logs".to_string(),
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
                has_unseen: true,
                sort_key: SessionSortKey::default(),
            },
        );
        state.recompute_aggregates();

        reduce_domain_event(
            &mut state,
            DomainEvent::FocusChanged {
                client_id: "client-1".to_string(),
                focus: ClientFocus {
                    session_id: "beta".to_string(),
                    window_id: "beta:1".to_string(),
                    pane_id: None,
                },
            },
        );

        assert!(!state.sessions["beta"].has_unseen);
    }
}
