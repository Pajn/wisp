use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
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
        KeyCode::Down => Some(UiIntent::SelectNext),
        KeyCode::Up => Some(UiIntent::SelectPrev),
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::SelectNext)
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::SelectPrev)
        }
        KeyCode::Enter => Some(UiIntent::ActivateSelected),
        KeyCode::Esc => Some(UiIntent::Close),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::Close)
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::TogglePreview)
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::ToggleDetails)
        }
        KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::ToggleCompactSidebar)
        }
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
        Paragraph::new(ansi_preview_text(preview))
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
        "up/down or ^j/^k move  enter open  ^p preview  ^d details  ^m compact  esc close"
    } else {
        "esc close"
    };

    Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL))
        .render(area, buffer);
}

fn ansi_preview_text(preview: &[String]) -> Text<'static> {
    let mut lines = Vec::with_capacity(preview.len().max(1));
    for line in preview {
        lines.push(parse_ansi_line(line));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    Text::from(lines)
}

fn parse_ansi_line(input: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut chars = input.chars().peekable();
    let mut plain = String::new();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next();
            flush_span(&mut spans, &mut plain, style);

            let mut sequence = String::new();
            while let Some(next) = chars.next() {
                if next == 'm' {
                    style = apply_sgr(style, &sequence);
                    break;
                }
                sequence.push(next);
            }
        } else {
            plain.push(ch);
        }
    }

    flush_span(&mut spans, &mut plain, style);
    Line::from(spans)
}

fn flush_span(spans: &mut Vec<Span<'static>>, plain: &mut String, style: Style) {
    if plain.is_empty() {
        return;
    }

    spans.push(Span::styled(std::mem::take(plain), style));
}

fn apply_sgr(mut style: Style, sequence: &str) -> Style {
    let codes = if sequence.is_empty() {
        vec![0]
    } else {
        sequence
            .split(';')
            .map(|part| part.parse::<u16>().unwrap_or(0))
            .collect::<Vec<_>>()
    };

    let mut index = 0;
    while index < codes.len() {
        match codes[index] {
            0 => style = Style::default(),
            1 => style = style.add_modifier(Modifier::BOLD),
            2 => style = style.add_modifier(Modifier::DIM),
            3 => style = style.add_modifier(Modifier::ITALIC),
            4 => style = style.add_modifier(Modifier::UNDERLINED),
            5 => style = style.add_modifier(Modifier::SLOW_BLINK),
            7 => style = style.add_modifier(Modifier::REVERSED),
            9 => style = style.add_modifier(Modifier::CROSSED_OUT),
            22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style = style.remove_modifier(Modifier::ITALIC),
            24 => style = style.remove_modifier(Modifier::UNDERLINED),
            25 => style = style.remove_modifier(Modifier::SLOW_BLINK),
            27 => style = style.remove_modifier(Modifier::REVERSED),
            29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 | 90..=97 => {
                style.fg = Some(ansi_named_color(codes[index]));
            }
            39 => style.fg = Some(Color::Reset),
            40..=47 | 100..=107 => {
                style.bg = Some(ansi_named_color(codes[index]));
            }
            49 => style.bg = Some(Color::Reset),
            38 | 48 => {
                let is_foreground = codes[index] == 38;
                let slice = &codes[index + 1..];
                if let Some((color, consumed)) = ansi_extended_color(slice) {
                    if is_foreground {
                        style.fg = Some(color);
                    } else {
                        style.bg = Some(color);
                    }
                    index += consumed;
                }
            }
            _ => {}
        }
        index += 1;
    }

    style
}

fn ansi_extended_color(codes: &[u16]) -> Option<(Color, usize)> {
    match codes {
        [5, value, ..] => Some((Color::Indexed((*value).min(u8::MAX as u16) as u8), 2)),
        [2, red, green, blue, ..] => Some((
            Color::Rgb(
                (*red).min(u8::MAX as u16) as u8,
                (*green).min(u8::MAX as u16) as u8,
                (*blue).min(u8::MAX as u16) as u8,
            ),
            4,
        )),
        _ => None,
    }
}

fn ansi_named_color(code: u16) -> Color {
    match code {
        30 | 40 => Color::Black,
        31 | 41 => Color::Red,
        32 | 42 => Color::Green,
        33 | 43 => Color::Yellow,
        34 | 44 => Color::Blue,
        35 | 45 => Color::Magenta,
        36 | 46 => Color::Cyan,
        37 | 47 => Color::Gray,
        90 | 100 => Color::DarkGray,
        91 | 101 => Color::LightRed,
        92 | 102 => Color::LightGreen,
        93 | 103 => Color::LightYellow,
        94 | 104 => Color::LightBlue,
        95 | 105 => Color::LightMagenta,
        96 | 106 => Color::LightCyan,
        97 | 107 => Color::White,
        _ => Color::Reset,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;
    use wisp_core::{AttentionBadge, SessionListItem};

    use crate::{
        SurfaceKind, SurfaceModel, UiIntent, ansi_preview_text, render_surface, translate_key,
    };

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
    fn renders_ansi_colored_preview_content() {
        let text = ansi_preview_text(&["\u{1b}[31mred\u{1b}[0m".to_string()]);
        let first_span = &text.lines[0].spans[0];

        assert_eq!(first_span.content, "red");
        assert_eq!(first_span.style.fg, Some(Color::Red));
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
            translate_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(UiIntent::SelectNext)
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(UiIntent::ToggleDetails)
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(UiIntent::TogglePreview)
        );
        assert_eq!(
            translate_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(UiIntent::FilterChanged("q".to_string()))
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
