use std::path::PathBuf;

use crate::{Candidate, CandidateMetadata};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PreviewKey {
    Session(String),
    Directory(PathBuf),
    File(PathBuf),
    Metadata(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewKind {
    SessionSummary,
    Directory,
    File,
    Metadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewRequest {
    SessionSummary {
        key: PreviewKey,
        session_name: String,
    },
    Directory {
        key: PreviewKey,
        path: PathBuf,
    },
    File {
        key: PreviewKey,
        path: PathBuf,
    },
    Metadata {
        key: PreviewKey,
        title: String,
    },
}

impl PreviewRequest {
    #[must_use]
    pub fn key(&self) -> &PreviewKey {
        match self {
            Self::SessionSummary { key, .. }
            | Self::Directory { key, .. }
            | Self::File { key, .. }
            | Self::Metadata { key, .. } => key,
        }
    }

    #[must_use]
    pub fn kind(&self) -> PreviewKind {
        match self {
            Self::SessionSummary { .. } => PreviewKind::SessionSummary,
            Self::Directory { .. } => PreviewKind::Directory,
            Self::File { .. } => PreviewKind::File,
            Self::Metadata { .. } => PreviewKind::Metadata,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewContent {
    pub title: String,
    pub body: Vec<String>,
    pub truncated: bool,
}

impl PreviewContent {
    #[must_use]
    pub fn from_text(title: impl Into<String>, text: impl AsRef<str>, max_lines: usize) -> Self {
        let mut body = text
            .as_ref()
            .lines()
            .take(max_lines)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let truncated = text.as_ref().lines().count() > max_lines;

        if body.is_empty() {
            body.push(String::new());
        }

        Self {
            title: title.into(),
            body,
            truncated,
        }
    }

    #[must_use]
    pub fn from_text_tail(
        title: impl Into<String>,
        text: impl AsRef<str>,
        max_lines: usize,
    ) -> Self {
        let lines = text
            .as_ref()
            .lines()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        let truncated = lines.len() > max_lines;
        let start = lines.len().saturating_sub(max_lines);
        let mut body = lines.into_iter().skip(start).collect::<Vec<_>>();

        if body.is_empty() {
            body.push(String::new());
        }

        Self {
            title: title.into(),
            body,
            truncated,
        }
    }
}

#[must_use]
pub fn preview_request_for_candidate(candidate: &Candidate) -> PreviewRequest {
    match &candidate.metadata {
        CandidateMetadata::Session(metadata) => PreviewRequest::SessionSummary {
            key: candidate.preview_key.clone(),
            session_name: metadata.session_name.clone(),
        },
        CandidateMetadata::Directory(metadata) => PreviewRequest::Directory {
            key: candidate.preview_key.clone(),
            path: metadata.full_path.clone(),
        },
        CandidateMetadata::Window(metadata) => PreviewRequest::Metadata {
            key: candidate.preview_key.clone(),
            title: format!("{}:{}", metadata.session_name, metadata.index),
        },
        CandidateMetadata::Project(metadata) => PreviewRequest::Directory {
            key: candidate.preview_key.clone(),
            path: metadata.root.clone(),
        },
        CandidateMetadata::Worktree(metadata) => PreviewRequest::Directory {
            key: candidate.preview_key.clone(),
            path: metadata.full_path.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{
        Candidate, PreviewKey, PreviewRequest, WorktreeMetadata, preview_request_for_candidate,
    };

    #[test]
    fn maps_worktree_candidates_to_directory_previews_using_full_path() {
        let full_path = PathBuf::from("/tmp/demo-worktree");
        let candidate = Candidate::worktree(WorktreeMetadata {
            full_path: full_path.clone(),
            display_path: "~/demo-worktree".to_string(),
            branch: Some("feature/demo".to_string()),
        });

        let request = preview_request_for_candidate(&candidate);

        assert_eq!(
            request,
            PreviewRequest::Directory {
                key: PreviewKey::Directory(full_path.clone()),
                path: full_path,
            }
        );
    }
}
