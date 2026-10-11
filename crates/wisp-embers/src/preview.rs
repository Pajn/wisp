//! Turns an Embers visible snapshot into preview text.
//!
//! The tmux backend previews a pane with `capture-pane -e`, whose output
//! carries SGR escapes that the UI parses into styled spans. Embers ships
//! styles as run-length [`StyledRun`]s over each line's text instead, so this
//! writes them back out as the same SGR escapes. Both backends then share one
//! parser and one rendering path from `PreviewContent` on.
//!
//! Each line is self-contained: a styled run starts with a full reset
//! (`0;…`) and a styled line ends with `ESC[0m`, so no style leaks into the
//! next line. Text, including combining marks such as the diacritics of a
//! kitty image placeholder, is copied unchanged. Hyperlinks are dropped: the
//! preview cannot follow them, and the UI strips OSC 8 from tmux captures too.

use std::fmt::Write as _;

use embers_core::{CellAttrs, SnapshotLine, StyledRun, TermColor};

const UNDERLINE_STYLES: [(u16, &str); 5] = [
    (CellAttrs::UNDERLINE, "4"),
    (CellAttrs::DOUBLE_UNDERLINE, "4:2"),
    (CellAttrs::CURLY_UNDERLINE, "4:3"),
    (CellAttrs::DOTTED_UNDERLINE, "4:4"),
    (CellAttrs::DASHED_UNDERLINE, "4:5"),
];

/// The last `max_lines` lines of `lines`, after dropping trailing blank
/// lines, as newline-separated text with SGR escapes.
///
/// A visible snapshot has a line for every screen row, while tmux and the
/// previous full capture end at the last line with content, so trailing blank
/// rows would otherwise push the content out of a short preview.
pub(crate) fn preview_text(lines: &[SnapshotLine], max_lines: usize) -> String {
    let end = lines
        .iter()
        .rposition(|line| !is_blank(line))
        .map_or(0, |index| index + 1);
    let start = end.saturating_sub(max_lines);
    lines[start..end]
        .iter()
        .map(styled_line_to_ansi)
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_blank(line: &SnapshotLine) -> bool {
    line.runs.is_empty() && line.text.trim().is_empty()
}

/// One line's text with its styles written as SGR escapes.
pub(crate) fn styled_line_to_ansi(line: &SnapshotLine) -> String {
    let text = line.text.as_str();
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    // The parameters of the escape in effect; `None` while unstyled.
    let mut current: Option<String> = None;

    for run in &line.runs {
        let end = offset + usize::try_from(run.len).unwrap_or(usize::MAX);
        // `len` counts bytes of `text`; if a run does not land on a character
        // boundary inside the line, the runs do not describe this text.
        let Some(segment) = text.get(offset..end) else {
            break;
        };
        let params = sgr_params(run);
        if params != current {
            output.push_str("\u{1b}[");
            output.push_str(params.as_deref().unwrap_or("0"));
            output.push('m');
            current = params;
        }
        output.push_str(segment);
        offset = end;
    }

    if offset < text.len() && current.is_some() {
        output.push_str("\u{1b}[0m");
        current = None;
    }
    output.push_str(&text[offset..]);
    if current.is_some() {
        output.push_str("\u{1b}[0m");
    }
    output
}

/// The SGR parameters for `run`, starting from a reset, or `None` for the
/// default style.
fn sgr_params(run: &StyledRun) -> Option<String> {
    let attrs = run.attrs;
    let mut params = String::from("0");
    for (flag, code) in [
        (CellAttrs::BOLD, "1"),
        (CellAttrs::DIM, "2"),
        (CellAttrs::ITALIC, "3"),
    ] {
        push_flag(&mut params, attrs, flag, code);
    }
    // The underline styles are mutually exclusive.
    if let Some((_, code)) = UNDERLINE_STYLES
        .iter()
        .find(|(flag, _)| attrs.contains(*flag))
    {
        params.push(';');
        params.push_str(code);
    }
    for (flag, code) in [
        (CellAttrs::INVERSE, "7"),
        (CellAttrs::HIDDEN, "8"),
        (CellAttrs::STRIKEOUT, "9"),
    ] {
        push_flag(&mut params, attrs, flag, code);
    }
    push_color(&mut params, run.fg, ColorSlot::Foreground);
    push_color(&mut params, run.bg, ColorSlot::Background);
    push_color(&mut params, run.underline_color, ColorSlot::Underline);

    (params != "0").then_some(params)
}

fn push_flag(params: &mut String, attrs: CellAttrs, flag: u16, code: &str) {
    if attrs.contains(flag) {
        params.push(';');
        params.push_str(code);
    }
}

#[derive(Clone, Copy)]
enum ColorSlot {
    Foreground,
    Background,
    Underline,
}

/// Writes `color` the way tmux `capture-pane -e` does: the 16 ANSI colors as
/// their 30–37/90–97 (40–47/100–107) codes, other indexed colors as `;5;n`
/// and true colors as `;2;r;g;b`. The underline color has no short codes.
fn push_color(params: &mut String, color: TermColor, slot: ColorSlot) {
    let (short_codes, extended) = match slot {
        ColorSlot::Foreground => (Some((30, 90)), 38),
        ColorSlot::Background => (Some((40, 100)), 48),
        ColorSlot::Underline => (None, 58),
    };
    // Writing to a `String` cannot fail.
    let _ = match (color, short_codes) {
        (TermColor::Default, _) => Ok(()),
        (TermColor::Indexed(index @ 0..=7), Some((normal, _))) => {
            write!(params, ";{}", normal + index)
        }
        (TermColor::Indexed(index @ 8..=15), Some((_, bright))) => {
            write!(params, ";{}", bright + (index - 8))
        }
        (TermColor::Indexed(index), _) => write!(params, ";{extended};5;{index}"),
        (TermColor::Rgb { r, g, b }, _) => write!(params, ";{extended};2;{r};{g};{b}"),
    };
}

#[cfg(test)]
mod tests {
    use embers_core::{CellAttrs, SnapshotLine, StyledRun, TermColor};

    use super::{preview_text, styled_line_to_ansi};

    fn run(len: usize, fg: TermColor, bg: TermColor, attrs: u16) -> StyledRun {
        StyledRun {
            len: u32::try_from(len).expect("run length fits"),
            fg,
            bg,
            attrs: CellAttrs(attrs),
            underline_color: TermColor::Default,
            link: 0,
        }
    }

    fn plain(len: usize) -> StyledRun {
        run(len, TermColor::Default, TermColor::Default, 0)
    }

    fn line(text: &str, runs: Vec<StyledRun>) -> SnapshotLine {
        SnapshotLine {
            text: text.to_string(),
            runs,
            links: Vec::new(),
        }
    }

    #[test]
    fn plain_line_has_no_escapes() {
        assert_eq!(styled_line_to_ansi(&SnapshotLine::plain("$ ls")), "$ ls");
    }

    #[test]
    fn colored_word_is_wrapped_in_sgr() {
        let line = line(
            "a red word",
            vec![
                plain(2),
                run(3, TermColor::Indexed(1), TermColor::Default, 0),
                plain(5),
            ],
        );

        assert_eq!(
            styled_line_to_ansi(&line),
            "a \u{1b}[0;31mred\u{1b}[0m word"
        );
    }

    #[test]
    fn indexed_colors_use_the_codes_tmux_writes() {
        let line = line(
            "abcdef",
            vec![
                run(1, TermColor::Indexed(7), TermColor::Default, 0),
                run(1, TermColor::Indexed(9), TermColor::Default, 0),
                run(1, TermColor::Indexed(200), TermColor::Default, 0),
                run(1, TermColor::Default, TermColor::Indexed(4), 0),
                run(1, TermColor::Default, TermColor::Indexed(15), 0),
                run(1, TermColor::Default, TermColor::Indexed(17), 0),
            ],
        );

        assert_eq!(
            styled_line_to_ansi(&line),
            "\u{1b}[0;37ma\u{1b}[0;91mb\u{1b}[0;38;5;200mc\u{1b}[0;44md\
             \u{1b}[0;107me\u{1b}[0;48;5;17mf\u{1b}[0m"
        );
    }

    #[test]
    fn rgb_colors_and_bold_are_written() {
        let line = line(
            "rgb",
            vec![run(
                3,
                TermColor::Rgb {
                    r: 10,
                    g: 20,
                    b: 30,
                },
                TermColor::Rgb { r: 1, g: 2, b: 3 },
                CellAttrs::BOLD,
            )],
        );

        assert_eq!(
            styled_line_to_ansi(&line),
            "\u{1b}[0;1;38;2;10;20;30;48;2;1;2;3mrgb\u{1b}[0m"
        );
    }

    #[test]
    fn attributes_are_written() {
        let line = line(
            "abcdefghijk",
            vec![
                run(1, TermColor::Default, TermColor::Default, CellAttrs::DIM),
                run(1, TermColor::Default, TermColor::Default, CellAttrs::ITALIC),
                run(
                    1,
                    TermColor::Default,
                    TermColor::Default,
                    CellAttrs::UNDERLINE,
                ),
                run(
                    1,
                    TermColor::Default,
                    TermColor::Default,
                    CellAttrs::DOUBLE_UNDERLINE,
                ),
                run(
                    1,
                    TermColor::Default,
                    TermColor::Default,
                    CellAttrs::CURLY_UNDERLINE,
                ),
                run(
                    1,
                    TermColor::Default,
                    TermColor::Default,
                    CellAttrs::DOTTED_UNDERLINE,
                ),
                run(
                    1,
                    TermColor::Default,
                    TermColor::Default,
                    CellAttrs::DASHED_UNDERLINE,
                ),
                run(
                    1,
                    TermColor::Default,
                    TermColor::Default,
                    CellAttrs::INVERSE,
                ),
                run(1, TermColor::Default, TermColor::Default, CellAttrs::HIDDEN),
                run(
                    1,
                    TermColor::Default,
                    TermColor::Default,
                    CellAttrs::STRIKEOUT,
                ),
                run(
                    1,
                    TermColor::Default,
                    TermColor::Default,
                    CellAttrs::BOLD | CellAttrs::ITALIC,
                ),
            ],
        );

        assert_eq!(
            styled_line_to_ansi(&line),
            "\u{1b}[0;2ma\u{1b}[0;3mb\u{1b}[0;4mc\u{1b}[0;4:2md\u{1b}[0;4:3me\
             \u{1b}[0;4:4mf\u{1b}[0;4:5mg\u{1b}[0;7mh\u{1b}[0;8mi\u{1b}[0;9mj\
             \u{1b}[0;1;3mk\u{1b}[0m"
        );
    }

    #[test]
    fn kitty_placeholder_keeps_diacritics_and_both_colors() {
        // U+10EEEE with row and column diacritics; the image id is in the
        // foreground color and the placement id in the underline color.
        let placeholder = "\u{10EEEE}\u{0305}\u{030D}";
        let text = format!("{placeholder}{placeholder} ");
        let mut image = run(
            placeholder.len() * 2,
            TermColor::Rgb { r: 0, g: 0, b: 42 },
            TermColor::Default,
            0,
        );
        image.underline_color = TermColor::Indexed(7);
        let line = line(&text, vec![image, plain(1)]);

        assert_eq!(
            styled_line_to_ansi(&line),
            format!("\u{1b}[0;38;2;0;0;42;58;5;7m{placeholder}{placeholder}\u{1b}[0m ")
        );
    }

    #[test]
    fn rgb_underline_color_is_written() {
        let mut underlined = run(
            2,
            TermColor::Default,
            TermColor::Default,
            CellAttrs::UNDERLINE,
        );
        underlined.underline_color = TermColor::Rgb { r: 4, g: 5, b: 6 };

        assert_eq!(
            styled_line_to_ansi(&line("ul", vec![underlined])),
            "\u{1b}[0;4;58;2;4;5;6mul\u{1b}[0m"
        );
    }

    #[test]
    fn adjacent_runs_with_one_style_share_an_escape() {
        let red = run(1, TermColor::Indexed(1), TermColor::Default, 0);

        assert_eq!(
            styled_line_to_ansi(&line("ab", vec![red, red])),
            "\u{1b}[0;31mab\u{1b}[0m"
        );
    }

    #[test]
    fn runs_that_do_not_match_the_text_fall_back_to_plain_text() {
        // `len` counts bytes; a run ending inside a character or past the end
        // of the line breaks the invariant, so the rest is written unstyled.
        let red = run(1, TermColor::Indexed(1), TermColor::Default, 0);

        assert_eq!(
            styled_line_to_ansi(&line("\u{e9}x", vec![red, plain(2)])),
            "\u{e9}x"
        );
        assert_eq!(
            styled_line_to_ansi(&line("ab", vec![red, plain(5)])),
            "\u{1b}[0;31ma\u{1b}[0mb"
        );
    }

    #[test]
    fn preview_text_takes_the_tail_without_trailing_blank_lines() {
        let lines = vec![
            SnapshotLine::plain("one"),
            line(
                "two",
                vec![run(3, TermColor::Indexed(2), TermColor::Default, 0)],
            ),
            SnapshotLine::plain("three"),
            SnapshotLine::plain(""),
            SnapshotLine::plain(""),
        ];

        assert_eq!(preview_text(&lines, 2), "\u{1b}[0;32mtwo\u{1b}[0m\nthree");
    }

    #[test]
    fn preview_text_keeps_painted_blank_lines() {
        // A line of spaces on a colored background is content.
        let lines = vec![
            SnapshotLine::plain("top"),
            line(
                "  ",
                vec![run(2, TermColor::Default, TermColor::Indexed(4), 0)],
            ),
        ];

        assert_eq!(preview_text(&lines, 5), "top\n\u{1b}[0;44m  \u{1b}[0m");
    }
}
