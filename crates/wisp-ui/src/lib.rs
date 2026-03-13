use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget},
};
use wisp_core::{GitBranchStatus, GitBranchSync, SessionListItem};

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
    pub bindings: KeyBindings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiIntent {
    SelectNext,
    SelectPrev,
    ActivateSelected,
    RenameSession,
    CloseSession,
    FilterChanged(String),
    Backspace,
    ToggleCompactSidebar,
    TogglePreview,
    ToggleDetails,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindings {
    pub enter: UiIntent,
    pub ctrl_r: UiIntent,
    pub ctrl_x: UiIntent,
    pub ctrl_p: UiIntent,
    pub ctrl_d: UiIntent,
    pub ctrl_m: UiIntent,
    pub esc: UiIntent,
    pub ctrl_c: UiIntent,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self {
            enter: UiIntent::ActivateSelected,
            ctrl_r: UiIntent::RenameSession,
            ctrl_x: UiIntent::CloseSession,
            ctrl_p: UiIntent::TogglePreview,
            ctrl_d: UiIntent::ToggleDetails,
            ctrl_m: UiIntent::ToggleCompactSidebar,
            esc: UiIntent::Close,
            ctrl_c: UiIntent::Close,
        }
    }
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
pub fn translate_key(key: KeyEvent, bindings: &KeyBindings) -> Option<UiIntent> {
    match key.code {
        KeyCode::Down => Some(UiIntent::SelectNext),
        KeyCode::Up => Some(UiIntent::SelectPrev),
        KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::SelectNext)
        }
        KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(UiIntent::SelectPrev)
        }
        KeyCode::Enter => Some(bindings.enter.clone()),
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(bindings.ctrl_r.clone())
        }
        KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(bindings.ctrl_x.clone())
        }
        KeyCode::Esc => Some(bindings.esc.clone()),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(bindings.ctrl_c.clone())
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(bindings.ctrl_p.clone())
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(bindings.ctrl_d.clone())
        }
        KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(bindings.ctrl_m.clone())
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
            Constraint::Length(if model.show_help { 3 } else { 1 }),
        ])
        .split(area);

    render_boxed_paragraph(
        chunks[0],
        buffer,
        model.title.as_str(),
        Text::from(model.query.as_str()),
    );

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
        render_boxed_paragraph(
            body_chunks[1],
            buffer,
            "Preview",
            ansi_preview_text(preview),
        );
    }

    render_footer(chunks[2], buffer, model);
}

fn render_sidebar(area: Rect, buffer: &mut Buffer, model: &SurfaceModel) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(4),
            Constraint::Length(if model.show_help { 3 } else { 1 }),
        ])
        .split(area);

    render_boxed_paragraph(
        chunks[0],
        buffer,
        model.title.as_str(),
        Text::from(model.query.as_str()),
    );

    render_list(
        chunks[1],
        buffer,
        model,
        matches!(model.kind, SurfaceKind::SidebarCompact),
    );
    render_footer(chunks[2], buffer, model);
}

fn render_list(area: Rect, buffer: &mut Buffer, model: &SurfaceModel, compact: bool) {
    let branch_width = if compact {
        0
    } else {
        model
            .items
            .iter()
            .filter_map(|item| item.git_branch.as_ref())
            .map(|branch| branch.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(18)
    };
    let dirty_width = if compact || branch_width == 0 { 0 } else { 1 };
    let marker_width = 3usize;
    let available_width = usize::from(area.width.saturating_sub(2));
    let gap_width = if compact { 0 } else { 2 };
    let session_width = if compact {
        available_width.saturating_sub(marker_width)
    } else {
        let max_session_width = model
            .items
            .iter()
            .map(|item| item.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(28);
        let branch_space = if branch_width == 0 {
            0
        } else {
            branch_width + dirty_width + gap_width
        };
        let title_budget = available_width
            .saturating_sub(marker_width + gap_width + branch_space)
            .max(12);
        max_session_width.min(title_budget.saturating_sub(8)).max(8)
    };

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
            let icon = format!("{marker}{badge}");
            let style = if index == model.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            let line = if compact {
                Line::from(Span::styled(
                    format!("{icon} {}", truncate_text(&item.label, session_width)),
                    style,
                ))
            } else {
                let branch_space = if branch_width == 0 {
                    0
                } else {
                    branch_width + dirty_width + gap_width
                };
                let title_width = available_width
                    .saturating_sub(marker_width + session_width + gap_width + branch_space);
                let session = pad_text(&truncate_text(&item.label, session_width), session_width);
                let title = pad_text(
                    &truncate_text(
                        item.active_window_label.as_deref().unwrap_or_default(),
                        title_width,
                    ),
                    title_width,
                );
                let prefix = if branch_width == 0 {
                    format!("{icon} {session}  {title}")
                } else {
                    format!("{icon} {session}  {title}  ")
                };

                let mut spans = vec![Span::styled(prefix, style)];
                if branch_width > 0 {
                    if let Some(branch) = item.git_branch.as_ref() {
                        spans.push(Span::styled(
                            pad_left(&truncate_left(&branch.name, branch_width), branch_width),
                            style.patch(branch_style(branch)),
                        ));
                        spans.push(Span::styled(
                            if branch.dirty { "*" } else { " " },
                            style.patch(Style::default().fg(Color::Yellow)),
                        ));
                    } else {
                        spans.push(Span::styled(" ".repeat(branch_width + dirty_width), style));
                    }
                }
                Line::from(spans)
            };

            ListItem::new(line)
        })
        .collect::<Vec<_>>();

    let block = rounded_block("Sessions");
    let inner = block.inner(area);
    block.render(area, buffer);
    Clear.render(inner, buffer);
    List::new(items).render(inner, buffer);
}

fn render_footer(area: Rect, buffer: &mut Buffer, model: &SurfaceModel) {
    let text = if model.show_help {
        bindings_help_text(&model.bindings)
    } else {
        compact_bindings_help_text(&model.bindings)
    };

    let block = rounded_block("");
    let inner = block.inner(area);
    block.render(area, buffer);
    Clear.render(inner, buffer);
    Paragraph::new(text).render(inner, buffer);
}

fn render_boxed_paragraph(area: Rect, buffer: &mut Buffer, title: &str, text: Text<'_>) {
    let block = rounded_block(title);
    let inner = block.inner(area);
    block.render(area, buffer);
    Clear.render(inner, buffer);
    Paragraph::new(text).render(inner, buffer);
}

fn rounded_block(title: &str) -> Block<'_> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
}

fn bindings_help_text(bindings: &KeyBindings) -> String {
    format!(
        "up/down or ^j/^k move  enter {}  ^r {}  ^x {}  ^p {}  ^d {}  ^m {}  esc {}  ^c {}",
        intent_label(&bindings.enter),
        intent_label(&bindings.ctrl_r),
        intent_label(&bindings.ctrl_x),
        intent_label(&bindings.ctrl_p),
        intent_label(&bindings.ctrl_d),
        intent_label(&bindings.ctrl_m),
        intent_label(&bindings.esc),
        intent_label(&bindings.ctrl_c),
    )
}

fn compact_bindings_help_text(bindings: &KeyBindings) -> String {
    format!(
        "esc {}  ^c {}",
        intent_label(&bindings.esc),
        intent_label(&bindings.ctrl_c),
    )
}

fn intent_label(intent: &UiIntent) -> &'static str {
    match intent {
        UiIntent::ActivateSelected => "open",
        UiIntent::RenameSession => "rename",
        UiIntent::CloseSession => "close session",
        UiIntent::TogglePreview => "preview",
        UiIntent::ToggleDetails => "details",
        UiIntent::ToggleCompactSidebar => "compact",
        UiIntent::Close => "close",
        UiIntent::SelectNext => "move down",
        UiIntent::SelectPrev => "move up",
        UiIntent::FilterChanged(_) => "filter",
        UiIntent::Backspace => "backspace",
    }
}

fn pad_text(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len >= width {
        value.to_string()
    } else {
        format!("{value}{}", " ".repeat(width - len))
    }
}

fn pad_left(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len >= width {
        value.to_string()
    } else {
        format!("{}{value}", " ".repeat(width - len))
    }
}

fn truncate_text(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= width {
        return value.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    chars[..width - 1].iter().collect::<String>() + "…"
}

fn truncate_left(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= width {
        return value.to_string();
    }
    if width == 1 {
        return "…".to_string();
    }
    format!(
        "…{}",
        chars[chars.len() - (width - 1)..]
            .iter()
            .collect::<String>()
    )
}

fn branch_style(branch: &GitBranchStatus) -> Style {
    let color = match branch.sync {
        GitBranchSync::Unknown => Color::Gray,
        GitBranchSync::Pushed => Color::Green,
        GitBranchSync::NotPushed => Color::Red,
    };
    Style::default().fg(color)
}

fn ansi_preview_text(preview: &[String]) -> Text<'static> {
    let mut lines = Vec::with_capacity(preview.len().max(1));
    for line in preview {
        lines.push(parse_ansi_line(&sanitize_ansi_input(line)));
    }
    if lines.is_empty() {
        lines.push(Line::default());
    }
    Text::from(lines)
}

fn sanitize_ansi_input(input: &str) -> String {
    let mut sanitized = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    let mut sequence = String::from("\u{1b}[");
                    let mut final_byte = None;
                    for next in chars.by_ref() {
                        sequence.push(next);
                        if ('@'..='~').contains(&next) {
                            final_byte = Some(next);
                            break;
                        }
                    }
                    if final_byte == Some('m') {
                        sanitized.push_str(&sequence);
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' && matches!(chars.peek(), Some('\\')) {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            ch if ch.is_control() => {}
            _ => sanitized.push(ch),
        }
    }

    sanitized
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
            for next in chars.by_ref() {
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
        KeyBindings, SurfaceKind, SurfaceModel, UiIntent, ansi_preview_text, render_surface,
        sanitize_ansi_input, translate_key,
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
            git_branch: None,
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
            bindings: KeyBindings::default(),
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
    fn strips_non_sgr_escape_sequences_from_preview_content() {
        let sanitized = sanitize_ansi_input("hello\u{1b}[2K\u{1b}[1G\u{1b}[31mred\u{1b}[0m\r");

        assert_eq!(sanitized, "hello\u{1b}[31mred\u{1b}[0m");
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
            bindings: KeyBindings::default(),
        };

        render_surface(buffer.area, &mut buffer, &model);

        let rendered = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Sidebar"));
        assert!(rendered.contains("•! alpha"));
    }

    #[test]
    fn translates_supported_keys() {
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                &KeyBindings::default()
            ),
            Some(UiIntent::SelectNext)
        );
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
                &KeyBindings::default(),
            ),
            Some(UiIntent::SelectNext)
        );
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                &KeyBindings::default(),
            ),
            Some(UiIntent::RenameSession)
        );
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
                &KeyBindings::default(),
            ),
            Some(UiIntent::ToggleDetails)
        );
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
                &KeyBindings::default(),
            ),
            Some(UiIntent::CloseSession)
        );
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL),
                &KeyBindings::default(),
            ),
            Some(UiIntent::TogglePreview)
        );
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                &KeyBindings::default(),
            ),
            Some(UiIntent::FilterChanged("q".to_string()))
        );
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
                &KeyBindings::default(),
            ),
            Some(UiIntent::FilterChanged("x".to_string()))
        );
        assert_eq!(
            translate_key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
                &KeyBindings::default()
            ),
            Some(UiIntent::Close)
        );
    }
}
