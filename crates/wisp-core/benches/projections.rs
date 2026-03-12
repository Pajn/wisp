use std::{collections::BTreeMap, path::PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};
use wisp_core::{
    AlertAggregate, AttentionBadge, ClientFocus, DirectoryRecord, DomainState, SessionRecord,
    SessionSortKey, WindowRecord, derive_candidates, derive_session_list,
};

fn seeded_state() -> DomainState {
    let sessions = (0..250)
        .map(|index| {
            let name = format!("session-{index}");
            (
                name.clone(),
                SessionRecord {
                    id: name.clone(),
                    name,
                    attached: index % 3 == 0,
                    windows: BTreeMap::from([(
                        format!("session-{index}:1"),
                        WindowRecord {
                            id: format!("session-{index}:1"),
                            index: 1,
                            name: "shell".to_string(),
                            active: true,
                            panes: BTreeMap::new(),
                            alerts: Default::default(),
                            has_unseen: false,
                            current_path: Some(PathBuf::from("/tmp")),
                            active_command: Some("nvim".to_string()),
                        },
                    )]),
                    aggregate_alerts: AlertAggregate {
                        any_activity: index % 5 == 0,
                        any_bell: false,
                        any_silence: false,
                        any_unseen: index % 7 == 0,
                        attention_count: usize::from(index % 5 == 0 || index % 7 == 0),
                        highest_priority: if index % 5 == 0 {
                            AttentionBadge::Activity
                        } else if index % 7 == 0 {
                            AttentionBadge::Unseen
                        } else {
                            AttentionBadge::None
                        },
                    },
                    has_unseen: index % 7 == 0,
                    sort_key: SessionSortKey {
                        last_activity: Some(index as u64),
                    },
                },
            )
        })
        .collect();

    DomainState {
        sessions,
        clients: BTreeMap::from([(
            "default".to_string(),
            ClientFocus {
                session_id: "session-0".to_string(),
                window_id: "session-0:1".to_string(),
                pane_id: None,
            },
        )]),
        previous_session_by_client: BTreeMap::from([(
            "default".to_string(),
            "session-1".to_string(),
        )]),
        directories: vec![DirectoryRecord {
            path: PathBuf::from("/tmp/project"),
            score: Some(5.0),
            exists: true,
        }],
        config: Default::default(),
    }
}

fn bench_projections(criterion: &mut Criterion) {
    let state = seeded_state();

    criterion.bench_function("derive_candidates", |bench| {
        bench.iter(|| derive_candidates(&state, None, false));
    });
    criterion.bench_function("derive_session_list", |bench| {
        bench.iter(|| derive_session_list(&state, Some("default")));
    });
}

criterion_group!(benches, bench_projections);
criterion_main!(benches);
