use wisp_core::{AttentionBadge, StatusSessionItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFormatOptions {
    pub prefix: String,
    pub max_sessions: Option<usize>,
    pub show_previous: bool,
    pub show_counts: bool,
}

impl Default for StatusFormatOptions {
    fn default() -> Self {
        Self {
            prefix: "Wisp".to_string(),
            max_sessions: None,
            show_previous: true,
            show_counts: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusRenderMode {
    Passive,
    Clickable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusSegment {
    pub text: String,
    pub click_target: Option<String>,
}

impl StatusSegment {
    fn passive(text: String) -> Self {
        Self {
            text,
            click_target: None,
        }
    }

    fn clickable(text: String, session_id: String) -> Self {
        Self {
            text,
            click_target: Some(session_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusRenderOutput {
    pub text: String,
    pub interactive: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusRenderState {
    last_rendered: Option<String>,
}

impl StatusRenderState {
    pub fn next_update(
        &mut self,
        items: &[StatusSessionItem],
        options: &StatusFormatOptions,
        mode: StatusRenderMode,
    ) -> Option<String> {
        let rendered = render_status_line(items, options, mode).text;
        if self.last_rendered.as_ref() == Some(&rendered) {
            None
        } else {
            self.last_rendered = Some(rendered.clone());
            Some(rendered)
        }
    }
}

#[must_use]
pub fn format_status_line(items: &[StatusSessionItem], options: &StatusFormatOptions) -> String {
    render_status_line(items, options, StatusRenderMode::Passive).text
}

#[must_use]
pub fn render_status_line(
    items: &[StatusSessionItem],
    options: &StatusFormatOptions,
    mode: StatusRenderMode,
) -> StatusRenderOutput {
    render_status_segments(&build_status_segments(items, options), mode)
}

#[must_use]
pub fn build_status_segments(
    items: &[StatusSessionItem],
    options: &StatusFormatOptions,
) -> Vec<StatusSegment> {
    let mut segments = vec![StatusSegment::passive(escape_tmux_status(&options.prefix))];
    segments.extend(
        visible_items(items, options.max_sessions)
            .into_iter()
            .map(|item| {
                StatusSegment::clickable(
                    format_status_item(item, options.show_previous),
                    item.session_id.clone(),
                )
            }),
    );
    segments
}

#[must_use]
pub fn render_status_segments(
    segments: &[StatusSegment],
    mode: StatusRenderMode,
) -> StatusRenderOutput {
    let interactive = mode == StatusRenderMode::Clickable
        && segments
            .iter()
            .any(|segment| segment.click_target.is_some());
    let text = segments
        .iter()
        .map(|segment| render_segment(segment, mode))
        .collect::<Vec<_>>()
        .join("  ");

    StatusRenderOutput { text, interactive }
}

#[must_use]
pub fn escape_tmux_status(input: &str) -> String {
    input.replace('#', "##")
}

#[must_use]
pub fn visible_items(
    items: &[StatusSessionItem],
    max_sessions: Option<usize>,
) -> Vec<&StatusSessionItem> {
    match max_sessions {
        Some(max_sessions) => items.iter().take(max_sessions).collect(),
        None => items.iter().collect(),
    }
}

fn render_segment(segment: &StatusSegment, mode: StatusRenderMode) -> String {
    match (mode, &segment.click_target) {
        (StatusRenderMode::Clickable, Some(session_id)) => {
            format!("#[range=session|{session_id}]{}#[norange]", segment.text)
        }
        _ => segment.text.clone(),
    }
}

fn format_status_item(item: &StatusSessionItem, show_previous: bool) -> String {
    let label = escape_tmux_status(&item.session_name);
    let prefix = if item.is_previous && show_previous {
        '‹'
    } else {
        ' '
    };
    let suffix = if item.is_current {
        '•'
    } else if item.is_previous && show_previous {
        '›'
    } else {
        ' '
    };

    format!("{prefix}{label}{suffix}{}", badge_suffix(item.badge))
}

fn badge_suffix(badge: AttentionBadge) -> &'static str {
    match badge {
        AttentionBadge::None => "",
        AttentionBadge::Silence => "~",
        AttentionBadge::Unseen => "+",
        AttentionBadge::Activity => "##",
        AttentionBadge::Bell => "!",
    }
}

#[cfg(test)]
mod tests {
    use wisp_core::{AttentionBadge, StatusSessionItem};

    use crate::{
        StatusFormatOptions, StatusRenderMode, StatusRenderState, build_status_segments,
        escape_tmux_status, format_status_line, render_status_line,
    };

    fn session(
        id: &str,
        name: &str,
        is_current: bool,
        is_previous: bool,
        badge: AttentionBadge,
    ) -> StatusSessionItem {
        StatusSessionItem {
            session_id: id.to_string(),
            session_name: name.to_string(),
            is_current,
            is_previous,
            badge,
        }
    }

    #[test]
    fn formats_current_previous_and_badge_markers() {
        let line = format_status_line(
            &[
                session("$1", "main", true, false, AttentionBadge::None),
                session("$2", "api", false, true, AttentionBadge::Activity),
                session("$3", "ops", false, false, AttentionBadge::Bell),
            ],
            &StatusFormatOptions::default(),
        );

        assert_eq!(line, "Wisp   main•  ‹api›##   ops !");
    }

    #[test]
    fn escapes_tmux_interpolation_markers() {
        assert_eq!(escape_tmux_status("build#1"), "build##1");
    }

    #[test]
    fn truncation_is_deterministic_when_capped() {
        let items = vec![
            session("$1", "idle-a", false, false, AttentionBadge::None),
            session("$2", "current", true, false, AttentionBadge::None),
            session("$3", "prev", false, true, AttentionBadge::None),
            session("$4", "alert", false, false, AttentionBadge::Bell),
        ];

        let line = format_status_line(
            &items,
            &StatusFormatOptions {
                max_sessions: Some(3),
                ..StatusFormatOptions::default()
            },
        );

        assert!(line.contains("idle-a"));
        assert!(line.contains("current•"));
        assert!(line.contains("‹prev›"));
        assert!(!line.contains("alert!"));
    }

    #[test]
    fn renders_all_sessions_by_default() {
        let line = format_status_line(
            &[
                session("$1", "alpha", false, false, AttentionBadge::None),
                session("$2", "beta", false, false, AttentionBadge::None),
                session("$3", "gamma", false, false, AttentionBadge::None),
            ],
            &StatusFormatOptions::default(),
        );

        assert!(line.contains("alpha"));
        assert!(line.contains("beta"));
        assert!(line.contains("gamma"));
    }

    #[test]
    fn builds_clickable_segments_from_session_items() {
        let segments = build_status_segments(
            &[session("$1", "alpha", true, false, AttentionBadge::Unseen)],
            &StatusFormatOptions::default(),
        );

        assert_eq!(segments[0].click_target, None);
        assert_eq!(segments[1].click_target.as_deref(), Some("$1"));
    }

    #[test]
    fn renders_click_targets_without_capturing_separators() {
        let rendered = render_status_line(
            &[
                session("$1", "alpha", true, false, AttentionBadge::None),
                session("$2", "beta", false, false, AttentionBadge::None),
            ],
            &StatusFormatOptions::default(),
            StatusRenderMode::Clickable,
        );

        assert_eq!(
            rendered.text,
            "Wisp  #[range=session|$1] alpha•#[norange]  #[range=session|$2] beta #[norange]"
        );
        assert!(rendered.interactive);
    }

    #[test]
    fn suppresses_duplicate_renders_across_modes() {
        let items = vec![session("$1", "main", true, false, AttentionBadge::None)];
        let mut state = StatusRenderState::default();

        assert_eq!(
            state.next_update(
                &items,
                &StatusFormatOptions::default(),
                StatusRenderMode::Passive
            ),
            Some("Wisp   main•".to_string())
        );
        assert_eq!(
            state.next_update(
                &items,
                &StatusFormatOptions::default(),
                StatusRenderMode::Passive
            ),
            None
        );
        assert_eq!(
            state.next_update(
                &items,
                &StatusFormatOptions::default(),
                StatusRenderMode::Clickable
            ),
            Some("Wisp  #[range=session|$1] main•#[norange]".to_string())
        );
    }
}
