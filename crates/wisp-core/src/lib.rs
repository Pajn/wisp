mod action;
mod candidate;
mod domain;
mod preview;
mod reduce;
mod view;

pub use action::{Action, CandidateAction, ResolvedAction, resolve_action, sanitize_session_name};
pub use candidate::{
    Candidate, CandidateId, CandidateKind, CandidateMetadata, DirectoryMetadata, ScoreHints,
    SessionMetadata, WindowMetadata, deduplicate_candidates, normalize_display_path,
    sort_candidates,
};
pub use domain::{
    AlertAggregate, AlertState, AttentionBadge, ClientFocus, ClientId, DirectoryRecord,
    DomainConfig, DomainSnapshot, DomainState, NotificationConfig, PaneId, PaneRecord, SessionId,
    SessionRecord, SessionSortKey, WindowId, WindowRecord, aggregate_alerts,
};
pub use preview::{
    PreviewContent, PreviewKey, PreviewKind, PreviewRequest, preview_request_for_candidate,
};
pub use reduce::{DomainEvent, reduce_domain_event};
pub use view::{
    SessionListItem, StatusSessionItem, derive_candidates, derive_session_list, derive_status_items,
};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{
        Candidate, CandidateAction, CandidateMetadata, DirectoryMetadata, PreviewKey, PreviewKind,
        ResolvedAction, ScoreHints, SessionMetadata, deduplicate_candidates,
        normalize_display_path, preview_request_for_candidate, resolve_action, sort_candidates,
    };

    #[test]
    fn normalizes_home_directory_display_paths() {
        let path = PathBuf::from("/Users/emma/projects/wisp");
        let home = PathBuf::from("/Users/emma");

        let display = normalize_display_path(&path, Some(home.as_path()));

        assert_eq!(display, "~/projects/wisp");
    }

    #[test]
    fn deduplicates_candidates_by_identity_while_keeping_the_better_match() {
        let duplicate_a = Candidate::directory(DirectoryMetadata {
            full_path: PathBuf::from("/tmp/wisp"),
            display_path: "/tmp/wisp".to_string(),
            zoxide_score: Some(5.0),
            git_root_hint: None,
            exists: true,
        });
        let duplicate_b = Candidate {
            score_hints: ScoreHints {
                source_score: Some(9),
                ..ScoreHints::default()
            },
            ..duplicate_a.clone()
        };

        let deduplicated = deduplicate_candidates([duplicate_a, duplicate_b]);

        assert_eq!(deduplicated.len(), 1);
        assert_eq!(deduplicated[0].score_hints.source_score, Some(9));
    }

    #[test]
    fn resolves_actions_from_candidate_metadata() {
        let session = Candidate::session(SessionMetadata {
            session_name: "workbench".to_string(),
            attached: true,
            current: true,
            window_count: 4,
            last_activity: Some(42),
        });
        let directory = Candidate::directory(DirectoryMetadata {
            full_path: PathBuf::from("/Users/emma/projects/wisp"),
            display_path: "~/projects/wisp".to_string(),
            zoxide_score: Some(8.5),
            git_root_hint: None,
            exists: true,
        });

        assert_eq!(
            resolve_action(&session),
            Some(ResolvedAction::SwitchSession {
                session_name: "workbench".to_string(),
            })
        );
        assert_eq!(
            resolve_action(&directory),
            Some(ResolvedAction::CreateOrSwitchSession {
                session_name: "wisp".to_string(),
                directory: PathBuf::from("/Users/emma/projects/wisp"),
            })
        );
    }

    #[test]
    fn derives_preview_keys_from_candidates() {
        let candidate = Candidate::session(SessionMetadata {
            session_name: "ops".to_string(),
            attached: false,
            current: false,
            window_count: 2,
            last_activity: None,
        });

        let request = preview_request_for_candidate(&candidate);

        assert_eq!(request.key(), &PreviewKey::Session("ops".to_string()));
        assert_eq!(request.kind(), PreviewKind::SessionSummary);
    }

    #[test]
    fn sorts_current_and_high_signal_candidates_first() {
        let mut candidates = vec![
            Candidate::directory(DirectoryMetadata {
                full_path: PathBuf::from("/tmp/zeta"),
                display_path: "/tmp/zeta".to_string(),
                zoxide_score: Some(1.0),
                git_root_hint: None,
                exists: true,
            }),
            Candidate::session(SessionMetadata {
                session_name: "alpha".to_string(),
                attached: false,
                current: true,
                window_count: 1,
                last_activity: Some(1),
            }),
        ];

        sort_candidates(&mut candidates);

        assert!(matches!(
            candidates[0].metadata,
            CandidateMetadata::Session(SessionMetadata { current: true, .. })
        ));
    }

    #[test]
    fn exposes_candidate_action_on_domain_values() {
        let candidate = Candidate::directory(DirectoryMetadata {
            full_path: PathBuf::from("/tmp/project"),
            display_path: "/tmp/project".to_string(),
            zoxide_score: Some(3.0),
            git_root_hint: None,
            exists: true,
        });

        assert_eq!(candidate.action, CandidateAction::CreateOrSwitchSession);
    }
}
