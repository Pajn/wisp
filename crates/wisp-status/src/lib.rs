use std::collections::HashSet;

use wisp_core::{AttentionBadge, StatusSessionItem};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFormatOptions {
    pub prefix: String,
    pub max_sessions: usize,
    pub show_previous: bool,
    pub show_counts: bool,
}

impl Default for StatusFormatOptions {
    fn default() -> Self {
        Self {
            prefix: "Wisp".to_string(),
            max_sessions: 8,
            show_previous: true,
            show_counts: false,
        }
    }
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
    ) -> Option<String> {
        let rendered = format_status_line(items, options);
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
    let mut parts = vec![escape_tmux_status(&options.prefix)];
    let visible = visible_items(items, options.max_sessions);

    for item in visible {
        parts.push(format_status_item(item, options.show_previous));
    }

    parts.join("  ")
}

#[must_use]
pub fn escape_tmux_status(input: &str) -> String {
    input.replace('#', "##")
}

#[must_use]
pub fn visible_items(items: &[StatusSessionItem], max_sessions: usize) -> Vec<&StatusSessionItem> {
    if items.len() <= max_sessions {
        return items.iter().collect();
    }

    let mut chosen = Vec::new();
    let mut seen = HashSet::new();

    for (index, item) in items.iter().enumerate() {
        if item.is_current || item.is_previous || item.badge != AttentionBadge::None {
            chosen.push((index, item));
            seen.insert(index);
        }
    }

    for (index, item) in items.iter().enumerate() {
        if chosen.len() >= max_sessions {
            break;
        }
        if seen.insert(index) {
            chosen.push((index, item));
        }
    }

    chosen.sort_by_key(|(index, _)| *index);
    chosen
        .into_iter()
        .take(max_sessions)
        .map(|(_, item)| item)
        .collect()
}

fn format_status_item(item: &StatusSessionItem, show_previous: bool) -> String {
    let label = escape_tmux_status(&item.label);
    let decorated = if item.is_current {
        format!("{label}•")
    } else if item.is_previous && show_previous {
        format!("‹{label}›")
    } else {
        label
    };

    format!("{decorated}{}", badge_suffix(item.badge))
}

fn badge_suffix(badge: AttentionBadge) -> &'static str {
    match badge {
        AttentionBadge::None => "",
        AttentionBadge::Silence => "~",
        AttentionBadge::Unseen => "+",
        AttentionBadge::Activity => "#",
        AttentionBadge::Bell => "!",
    }
}

#[cfg(test)]
mod tests {
    use wisp_core::{AttentionBadge, StatusSessionItem};

    use crate::{StatusFormatOptions, StatusRenderState, escape_tmux_status, format_status_line};

    #[test]
    fn formats_current_previous_and_badge_markers() {
        let line = format_status_line(
            &[
                StatusSessionItem {
                    label: "main".to_string(),
                    is_current: true,
                    is_previous: false,
                    badge: AttentionBadge::None,
                },
                StatusSessionItem {
                    label: "api".to_string(),
                    is_current: false,
                    is_previous: true,
                    badge: AttentionBadge::Activity,
                },
                StatusSessionItem {
                    label: "ops".to_string(),
                    is_current: false,
                    is_previous: false,
                    badge: AttentionBadge::Bell,
                },
            ],
            &StatusFormatOptions::default(),
        );

        assert_eq!(line, "Wisp  main•  ‹api›#  ops!");
    }

    #[test]
    fn escapes_tmux_interpolation_markers() {
        assert_eq!(escape_tmux_status("build#1"), "build##1");
    }

    #[test]
    fn truncation_keeps_current_previous_and_attention() {
        let items = vec![
            StatusSessionItem {
                label: "idle-a".to_string(),
                is_current: false,
                is_previous: false,
                badge: AttentionBadge::None,
            },
            StatusSessionItem {
                label: "current".to_string(),
                is_current: true,
                is_previous: false,
                badge: AttentionBadge::None,
            },
            StatusSessionItem {
                label: "prev".to_string(),
                is_current: false,
                is_previous: true,
                badge: AttentionBadge::None,
            },
            StatusSessionItem {
                label: "alert".to_string(),
                is_current: false,
                is_previous: false,
                badge: AttentionBadge::Bell,
            },
        ];

        let line = format_status_line(
            &items,
            &StatusFormatOptions {
                max_sessions: 3,
                ..StatusFormatOptions::default()
            },
        );

        assert!(line.contains("current•"));
        assert!(line.contains("‹prev›"));
        assert!(line.contains("alert!"));
    }

    #[test]
    fn suppresses_duplicate_renders() {
        let items = vec![StatusSessionItem {
            label: "main".to_string(),
            is_current: true,
            is_previous: false,
            badge: AttentionBadge::None,
        }];
        let mut state = StatusRenderState::default();

        assert_eq!(
            state.next_update(&items, &StatusFormatOptions::default()),
            Some("Wisp  main•".to_string())
        );
        assert_eq!(
            state.next_update(&items, &StatusFormatOptions::default()),
            None
        );
    }
}
