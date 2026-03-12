use std::path::{Path, PathBuf};

use crate::{Candidate, CandidateMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    SwitchSession,
    CreateOrSwitchSession,
    OpenShellHere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateAction {
    Open,
    SwitchSession,
    CreateOrSwitchSession,
    OpenShellHere,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAction {
    SwitchSession {
        session_name: String,
    },
    CreateOrSwitchSession {
        session_name: String,
        directory: PathBuf,
    },
    OpenShellHere {
        directory: PathBuf,
    },
}

#[must_use]
pub fn resolve_action(candidate: &Candidate) -> Option<ResolvedAction> {
    match (&candidate.action, &candidate.metadata) {
        (CandidateAction::SwitchSession, CandidateMetadata::Session(metadata)) => {
            Some(ResolvedAction::SwitchSession {
                session_name: metadata.session_name.clone(),
            })
        }
        (CandidateAction::CreateOrSwitchSession, CandidateMetadata::Directory(metadata)) => {
            Some(ResolvedAction::CreateOrSwitchSession {
                session_name: sanitize_session_name(&metadata.full_path),
                directory: metadata.full_path.clone(),
            })
        }
        (CandidateAction::OpenShellHere, CandidateMetadata::Directory(metadata)) => {
            Some(ResolvedAction::OpenShellHere {
                directory: metadata.full_path.clone(),
            })
        }
        _ => None,
    }
}

#[must_use]
pub fn sanitize_session_name(path: &Path) -> String {
    let raw_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("wisp");

    let sanitized: String = raw_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect();

    let trimmed = sanitized.trim_matches('-');
    if trimmed.is_empty() {
        "wisp".to_string()
    } else {
        trimmed.to_string()
    }
}
