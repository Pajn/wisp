use std::{
    cmp::Ordering,
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{CandidateAction, PreviewKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CandidateKind {
    TmuxSession,
    TmuxWindow,
    Directory,
    Project,
    Worktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CandidateId {
    Session(String),
    Window { session: String, index: u32 },
    Directory(PathBuf),
    Worktree(PathBuf),
    Project(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: CandidateId,
    pub kind: CandidateKind,
    pub primary_text: String,
    pub secondary_text: Option<String>,
    pub search_terms: Vec<String>,
    pub preview_key: PreviewKey,
    pub score_hints: ScoreHints,
    pub action: CandidateAction,
    pub metadata: CandidateMetadata,
}

impl Candidate {
    #[must_use]
    pub fn session(metadata: SessionMetadata) -> Self {
        let primary_text = metadata.session_name.clone();
        let secondary_text = Some(format!("{} windows", metadata.window_count));

        Self {
            id: CandidateId::Session(metadata.session_name.clone()),
            kind: CandidateKind::TmuxSession,
            search_terms: vec![metadata.session_name.clone()],
            preview_key: PreviewKey::Session(metadata.session_name.clone()),
            score_hints: ScoreHints {
                recency: metadata.last_activity,
                source_score: Some(i64::from(metadata.window_count as i32)),
                is_current: metadata.current,
                is_attached: metadata.attached,
            },
            action: CandidateAction::SwitchSession,
            metadata: CandidateMetadata::Session(metadata),
            primary_text,
            secondary_text,
        }
    }

    #[must_use]
    pub fn directory(metadata: DirectoryMetadata) -> Self {
        let primary_text = metadata.display_path.clone();

        Self {
            id: CandidateId::Directory(metadata.full_path.clone()),
            kind: CandidateKind::Directory,
            search_terms: vec![
                metadata.display_path.clone(),
                metadata.full_path.display().to_string(),
            ],
            preview_key: PreviewKey::Directory(metadata.full_path.clone()),
            score_hints: ScoreHints {
                source_score: metadata.zoxide_score.map(|score| score.round() as i64),
                ..ScoreHints::default()
            },
            action: CandidateAction::CreateOrSwitchSession,
            metadata: CandidateMetadata::Directory(metadata),
            primary_text,
            secondary_text: Some("directory".to_string()),
        }
    }

    #[must_use]
    pub fn worktree(metadata: WorktreeMetadata) -> Self {
        let primary_text = metadata.display_path.clone();
        let mut search_terms = vec![
            metadata.display_path.clone(),
            metadata.full_path.display().to_string(),
        ];
        if let Some(branch) = &metadata.branch {
            search_terms.push(branch.clone());
        }

        Self {
            id: CandidateId::Worktree(metadata.full_path.clone()),
            kind: CandidateKind::Worktree,
            search_terms,
            preview_key: PreviewKey::Directory(metadata.full_path.clone()),
            score_hints: ScoreHints::default(),
            action: CandidateAction::CreateOrSwitchSession,
            metadata: CandidateMetadata::Worktree(metadata),
            primary_text,
            secondary_text: Some("worktree".to_string()),
        }
    }

    #[must_use]
    pub fn searchable_text(&self) -> String {
        self.search_terms.join(" ")
    }

    #[must_use]
    pub fn matches_query(&self, query: &str) -> bool {
        let normalized_query = normalize_text(query);
        if normalized_query.is_empty() {
            return true;
        }

        let haystack = normalize_text(&self.searchable_text());
        normalized_query
            .split_whitespace()
            .all(|needle| haystack.contains(needle))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CandidateMetadata {
    Session(SessionMetadata),
    Window(WindowMetadata),
    Directory(DirectoryMetadata),
    Project(ProjectMetadata),
    Worktree(WorktreeMetadata),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub session_name: String,
    pub attached: bool,
    pub current: bool,
    pub window_count: usize,
    pub last_activity: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowMetadata {
    pub session_name: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryMetadata {
    pub full_path: PathBuf,
    pub display_path: String,
    pub zoxide_score: Option<f64>,
    pub git_root_hint: Option<PathBuf>,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectMetadata {
    pub name: String,
    pub root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeMetadata {
    pub full_path: PathBuf,
    pub display_path: String,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScoreHints {
    pub recency: Option<u64>,
    pub source_score: Option<i64>,
    pub is_current: bool,
    pub is_attached: bool,
}

impl ScoreHints {
    fn priority_tuple(&self) -> (bool, bool, i64, u64) {
        (
            self.is_current,
            self.is_attached,
            self.source_score.unwrap_or_default(),
            self.recency.unwrap_or_default(),
        )
    }
}

#[must_use]
pub fn normalize_display_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home_path) = home
        && let Ok(relative) = path.strip_prefix(home_path)
    {
        let suffix = relative.display().to_string();
        return if suffix.is_empty() {
            "~".to_string()
        } else {
            format!("~/{suffix}")
        };
    }

    path.display().to_string()
}

#[must_use]
pub fn deduplicate_candidates(candidates: impl IntoIterator<Item = Candidate>) -> Vec<Candidate> {
    let mut deduplicated: BTreeMap<CandidateId, Candidate> = BTreeMap::new();

    for candidate in candidates {
        match deduplicated.get(&candidate.id) {
            Some(existing) if candidate_priority(&candidate) <= candidate_priority(existing) => {}
            _ => {
                deduplicated.insert(candidate.id.clone(), candidate);
            }
        }
    }

    deduplicated.into_values().collect()
}

pub fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(candidate_cmp);
}

fn candidate_cmp(left: &Candidate, right: &Candidate) -> Ordering {
    candidate_priority(left)
        .cmp(&candidate_priority(right))
        .reverse()
        .then_with(|| left.kind.cmp(&right.kind))
        .then_with(|| left.primary_text.cmp(&right.primary_text))
        .then_with(|| left.secondary_text.cmp(&right.secondary_text))
}

fn candidate_priority(candidate: &Candidate) -> (bool, bool, i64, u64) {
    candidate.score_hints.priority_tuple()
}

fn normalize_text(input: &str) -> String {
    input
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}
