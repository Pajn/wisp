use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Widget},
};
use wisp_core::SessionListItem;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceKind {
    Picker,
    SidebarCompact,
    SidebarExpanded,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceModel {
    pub title: String,
    pub query: String,
    pub items: Vec<SessionListItem>,
    pub selected: usize,
    pub show_help: bool,
    pub preview: Option<Vec<String>>,
    pub kind: SurfaceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiIntent {
    SelectNext,
    SelectPrev,
    ActivateSelected,
    FilterChanged(String),
    Backspace,
    ToggleCompactSidebar,
    TogglePreview,
    ToggleDetails,
    Close,
}

pub fn render_surface(area: Rect, buffer: &mut Buffer, model: &SurfaceModel) {
    match model.kind {
        SurfaceKind::Picker => render_picker(area, buffer, model),
        SurfaceKind::SidebarCompact | SurfaceKind::SidebarExpanded => {
            render_sidebar(area, buffer, model)
        }
    }
}

#[must_use]
pub fn translate_key(key: KeyEvent) -> Option<UiIntent> {
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => Some(UiIntent::SelectNext),
        KeyCode::Up | KeyCode::Char('k') => Some(UiIntent::SelectPrev),
        KeyCode::Enter => Some(UiIntent::ActivateSelected),
        KeyCode::Esc | KeyCode::Char('q') => Some(UiIntent::Close),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::Close)
        }
        KeyCode::Char('p') => Some(UiIntent::TogglePreview),
        KeyCode::Char('d') => Some(UiIntent::ToggleDetails),
        KeyCode::Char('m') => Some(UiIntent::ToggleCompactSidebar),
        KeyCode::Backspace => Some(UiIntent::Backspace),
        KeyCode::Char(character)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(UiIntent::FilterChanged(character.to_string()))
        }
        _ => None,
    }
}

fn render_picker(area: Rect, buffer: &mut Buffer, model: &SurfaceModel) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(if model.show_help { 2 } else { 1 }),
        ])
        .split(area);

    Paragraph::new(model.query.as_str())
        .block(
            Block::default()
                .title(model.title.as_str())
                .borders(Borders::ALL),
        )
        .render(chunks[0], buffer);

    let body_chunks = if model.preview.is_some() {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(100)])
            .split(chunks[1])
    };

    render_list(body_chunks[0], buffer, model, false);

    if let Some(preview) = &model.preview {
        Paragraph::new(preview.join("\n"))
            .block(Block::default().title("Preview").borders(Borders::ALL))
            .render(body_chunks[1], buffer);
    }

    render_footer(chunks[2], buffer, model);
}

fn render_sidebar(area: Rect, buffer: &mut Buffer, model: &SurfaceModel) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(if model.show_help { 2 } else { 1 }),
        ])
        .split(area);

    Paragraph::new(model.query.as_str())
        .block(
            Block::default()
                .title(model.title.as_str())
                .borders(Borders::ALL),
        )
        .render(chunks[0], buffer);

    render_list(
        chunks[1],
        buffer,
        model,
        matches!(model.kind, SurfaceKind::SidebarCompact),
    );
    render_footer(chunks[2], buffer, model);
}

fn render_list(area: Rect, buffer: &mut Buffer, model: &SurfaceModel, compact: bool) {
    let items = model
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let marker = if item.is_current {
                "•"
            } else if item.is_previous {
                "‹›"
            } else {
                " "
            };
            let badge = match item.attention {
                wisp_core::AttentionBadge::None => "",
                wisp_core::AttentionBadge::Silence => "~",
                wisp_core::AttentionBadge::Unseen => "+",
                wisp_core::AttentionBadge::Activity => "#",
                wisp_core::AttentionBadge::Bell => "!",
            };
            let text = if compact {
                format!("{marker} {}{badge}", item.label)
            } else {
                format!(
                    "{marker} {}{} {}",
                    item.label,
                    badge,
                    item.active_window_label.clone().unwrap_or_default()
                )
                .trim()
                .to_string()
            };

            let style = if index == model.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };

            ListItem::new(Line::from(Span::styled(text, style)))
        })
        .collect::<Vec<_>>();

    List::new(items)
        .block(Block::default().title("Sessions").borders(Borders::ALL))
        .render(area, buffer);
}

fn render_footer(area: Rect, buffer: &mut Buffer, model: &SurfaceModel) {
    let text = if model.show_help {
        "j/k move  enter open  p preview  d details  m compact  q close"
    } else {
        "q close"
    };

    Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL))
        .render(area, buffer);
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use wisp_core::{AttentionBadge, SessionListItem};

    use crate::{SurfaceKind, SurfaceModel, UiIntent, render_surface, translate_key};

    fn item(label: &str) -> SessionListItem {
        SessionListItem {
            session_id: label.to_string(),
            label: label.to_string(),
            is_current: false,
            is_previous: false,
            attached: false,
            attention: AttentionBadge::None,
            attention_count: 0,
            active_window_label: Some("shell".to_string()),
            path_hint: None,
            command_hint: None,
        }
    }

    #[test]
    fn renders_picker_with_preview() {
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 60, 12));
        let model = SurfaceModel {
            title: "Wisp Picker".to_string(),
            query: "alp".to_string(),
            items: vec![item("alpha"), item("beta")],
            selected: 0,
            show_help: true,
            preview: Some(vec!["preview line".to_string()]),
            kind: SurfaceKind::Picker,
        };

        render_surface(buffer.area, &mut buffer, &model);

        let rendered = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Wisp Picker"));
        assert!(rendered.contains("Preview"));
        assert!(rendered.contains("alpha"));
    }

    #[test]
    fn renders_compact_sidebar() {
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 30, 10));
        let mut current = item("alpha");
        current.is_current = true;
        current.attention = AttentionBadge::Bell;
        let model = SurfaceModel {
            title: "Sidebar".to_string(),
            query: String::new(),
            items: vec![current],
            selected: 0,
            show_help: false,
            preview: None,
            kind: SurfaceKind::SidebarCompact,
        };

        render_surface(buffer.area, &mut buffer, &model);

        let rendered = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Sidebar"));
        assert!(rendered.contains("alpha!"));
    }

    #[test]
    fn translates_supported_keys() {
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Some(UiIntent::SelectNext)
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(UiIntent::ToggleDetails)
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Some(UiIntent::FilterChanged("x".to_string()))
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(UiIntent::Close)
        );
    }
}
