//! Frames as what a terminal is sent: each row's spans with their colours, and only the rows that
//! changed since the last frame.

use baton_core::lines::{Line, Span};
use baton_core::term::escape;
use baton_core::theme::Theme;

use crate::ui::kit::{cut, props};
use crate::ui::login::LinkAt;

/// No line wrap: a row as wide as the terminal never spills onto the next one.
pub const WRAP_OFF: &str = "\x1b[?7l";
/// Line wrap back on.
pub const WRAP_ON: &str = "\x1b[?7h";
/// The alternate screen, cleared, the cursor hidden.
pub const SCREEN_ON: &str = "\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H";
/// Colours off, the cursor shown, the screen that was there before.
pub const SCREEN_OFF: &str = "\x1b[0m\x1b[?25h\x1b[?1049l";
/// Mouse reports for presses and the wheel, as SGR.
pub const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
/// Every mouse report off.
pub const MOUSE_OFF: &str = "\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l";
/// Pastes between marks, so that a pasted line break is not the Enter key.
pub const PASTE_ON: &str = "\x1b[?2004h";
/// Pastes as they are typed.
pub const PASTE_OFF: &str = "\x1b[?2004l";
/// Everything the menu changes of a terminal, undone: for whoever has to give the terminal back
/// without the menu's help (a signal handler, a panic hook).
pub const RESTORE: &str =
    "\x1b[?2004l\x1b[0m\x1b[?25h\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?7h";
/// The terminal holds what it is sent back until the frame is whole.
const FRAME_START: &str = "\x1b[?2026h";
/// The terminal shows the frame.
const FRAME_END: &str = "\x1b[?2026l";

/// Spans as text with their colours at a colour depth. Neighbouring spans of one style share an
/// escape.
fn styled(spans: &[Span], theme: &Theme, depth: u8) -> String {
    let mut out = String::new();
    let mut runs: Vec<Span> = Vec::new();
    for span in spans {
        match runs.last_mut() {
            Some(last) if last.style == span.style => last.text.push_str(&span.text),
            _ => runs.push(span.clone()),
        }
    }
    for run in runs {
        let open = escape(&props(theme, &run.style), depth);
        out.push_str(&open);
        out.push_str(&run.text);
        if !open.is_empty() {
            out.push_str("\x1b[0m");
        }
    }
    out
}

/// A row as text with its colours at a colour depth. `link`: the cells of the row that open a link,
/// wrapped as a hyperlink (OSC 8).
pub fn row_text(row: &[Span], theme: &Theme, depth: u8, link: Option<&LinkAt>) -> String {
    let Some(link) = link else {
        return styled(row, theme, depth);
    };
    let end = link.col + link.width;
    format!(
        "{}\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\{}",
        styled(&cut(row, 0, link.col), theme, depth),
        link.url,
        styled(&cut(row, link.col, end), theme, depth),
        styled(&cut(row, end, usize::MAX), theme, depth)
    )
}

/// What was last drawn, to draw only what changed.
#[derive(Clone, Debug, Default)]
pub struct Painter {
    /// The rows as they were last sent.
    rows: Vec<String>,
    /// Whether the next frame is drawn whole, on a cleared screen.
    whole: bool,
}

impl Painter {
    /// A painter that has drawn nothing.
    pub fn new() -> Painter {
        Painter {
            rows: Vec::new(),
            whole: true,
        }
    }

    /// Forgets what was drawn: the next frame clears the screen and draws every row.
    pub fn reset(&mut self) {
        self.rows.clear();
        self.whole = true;
    }

    /// What to send the terminal for a frame: the rows that changed, each at its place. Empty when
    /// nothing changed.
    pub fn paint(&mut self, frame: &[Line], theme: &Theme, depth: u8) -> String {
        let rows: Vec<String> = frame
            .iter()
            .map(|row| row_text(row, theme, depth, None))
            .collect();
        self.send(rows)
    }

    /// The same for rows that are text already.
    pub fn send(&mut self, rows: Vec<String>) -> String {
        let mut out = String::new();
        if self.whole {
            out.push_str("\x1b[0m\x1b[2J");
        }
        for (y, row) in rows.iter().enumerate() {
            if self.whole || self.rows.get(y) != Some(row) {
                out.push_str(&format!("\x1b[{};1H", y + 1));
                out.push_str(row);
            }
        }
        // Rows of a taller frame that this one does not have.
        for y in rows.len()..self.rows.len() {
            out.push_str(&format!("\x1b[{};1H\x1b[2K", y + 1));
        }
        self.rows = rows;
        self.whole = false;
        if out.is_empty() {
            return out;
        }
        format!("{FRAME_START}{out}{FRAME_END}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::lines::{p, s};
    use baton_core::theme::{GlyphSet, make_theme};

    #[test]
    fn a_row_with_its_colours() {
        let mono = make_theme(false, GlyphSet::Rich);
        let row = vec![s("a", "bold"), s("b", "bold"), p(" c")];
        assert_eq!(row_text(&row, &mono, 1, None), "\x1b[1mab\x1b[0m c");
        let colour = make_theme(true, GlyphSet::Rich);
        assert_eq!(
            row_text(&[s("x", "ok bg-surface")], &colour, 24, None),
            "\x1b[38;2;146;174;153;48;2;24;27;32mx\x1b[0m"
        );
        // The rule sits on the terminal's own background.
        assert_eq!(
            row_text(&[s("|", "divider bg-none")], &colour, 24, None),
            "\x1b[38;2;52;58;67m|\x1b[0m"
        );
    }

    #[test]
    fn the_cells_of_a_link_are_a_hyperlink() {
        let mono = make_theme(false, GlyphSet::Rich);
        let row = vec![p("see "), s("the link here", "underline"), p(" ok")];
        let link = LinkAt {
            row: 0,
            col: 8,
            width: 4,
            url: "https://claude.ai/x".to_owned(),
        };
        assert_eq!(
            row_text(&row, &mono, 1, Some(&link)),
            "see \x1b[4mthe \x1b[0m\x1b]8;;https://claude.ai/x\x1b\\\x1b[4mlink\x1b[0m\x1b]8;;\x1b\\\x1b[4m here\x1b[0m ok"
        );
    }

    #[test]
    fn only_the_rows_that_changed_are_sent() {
        let mono = make_theme(false, GlyphSet::Rich);
        let mut painter = Painter::new();
        let frame = vec![vec![p("one")], vec![p("two")], vec![p("three")]];
        assert_eq!(
            painter.paint(&frame, &mono, 1),
            "\x1b[?2026h\x1b[0m\x1b[2J\x1b[1;1Hone\x1b[2;1Htwo\x1b[3;1Hthree\x1b[?2026l"
        );
        assert_eq!(painter.paint(&frame, &mono, 1), "");
        let next = vec![vec![p("one")], vec![p("TWO")]];
        assert_eq!(
            painter.paint(&next, &mono, 1),
            "\x1b[?2026h\x1b[2;1HTWO\x1b[3;1H\x1b[2K\x1b[?2026l"
        );
        painter.reset();
        assert_eq!(
            painter.paint(&next, &mono, 1),
            "\x1b[?2026h\x1b[0m\x1b[2J\x1b[1;1Hone\x1b[2;1HTWO\x1b[?2026l"
        );
    }
}
