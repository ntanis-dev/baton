//! The building blocks the screen is composed from: blocks of lines padded to a box, panel edges, the
//! spinner, cutting a row at cell boundaries, floating one block over another, and how a span's style
//! words are drawn inside the menu's frame.

use baton_core::lines::{Line, Span, fit_line, line_width, p, s};
use baton_core::text::char_width;
use baton_core::theme::{Glyphs, TextProps, Theme, make_theme, role};

use crate::state::State;

/// The style word that puts a span on the surface colour (the sidebar, dialogs and toasts) instead of
/// the frame's background.
pub const ON_SURFACE: &str = "bg-surface";

/// The style word that leaves a span on the terminal's own background: the rule between the sidebar
/// and the pane, as the border it replaces was drawn.
pub const ON_TERMINAL: &str = "bg-none";

/// The look a state is drawn in.
pub fn theme_of(state: &State) -> Theme {
    make_theme(state.color, state.glyphs)
}

/// How a span of a frame row is drawn: its own style words, over the frame's text colour and the
/// background of where it sits. Without colour there is neither.
pub fn props(theme: &Theme, style: &str) -> TextProps {
    let mut props = theme.props(style);
    if theme.color {
        if props.color.is_none() {
            props.color = role("text").map(str::to_owned);
        }
        let has = |word: &str| style.split(' ').any(|own| own == word);
        if props.background.is_none() && !has(ON_TERMINAL) {
            let surface = has(ON_SURFACE);
            props.background = role(if surface { "surface" } else { "background" }).map(str::to_owned);
        }
    }
    props
}

/// A line with every span on the surface colour.
pub fn on_surface(line: &[Span]) -> Line {
    line.iter()
        .map(|span| Span {
            text: span.text.clone(),
            style: if span.style.is_empty() {
                ON_SURFACE.to_owned()
            } else {
                format!("{} {ON_SURFACE}", span.style)
            },
        })
        .collect()
}

/// Lines in a box: each exactly `width` cells, and with a `height` exactly that many of them (the
/// first ones, blank lines after the last).
pub fn block(lines: &[Line], width: usize, height: Option<usize>) -> Vec<Line> {
    let count = height.unwrap_or(lines.len());
    (0..count)
        .map(|i| fit_line(lines.get(i).map_or(&[][..], Vec::as_slice), width, ""))
        .collect()
}

/// The frame of the spinner at a tick.
pub fn spinner_frame(g: &Glyphs, tick: u64) -> &'static str {
    g.spinner[(tick % g.spinner.len() as u64) as usize]
}

/// A line with its spinners at the frame of this tick. A span styled "spin" is a spinner: its sign
/// becomes the frame, the spaces around it stay.
pub fn spin(line: &[Span], g: &Glyphs, tick: u64) -> Line {
    let mut out = Vec::with_capacity(line.len());
    for span in line {
        if !span.style.split(' ').any(|word| word == "spin") {
            out.push(span.clone());
            continue;
        }
        let own = span
            .style
            .split(' ')
            .filter(|word| !word.is_empty() && *word != "spin")
            .collect::<Vec<_>>()
            .join(" ");
        let lead: String = span.text.chars().take_while(|c| c.is_whitespace()).collect();
        let rest = &span.text[lead.len()..];
        let mut signs = rest.chars();
        let Some(sign) = signs.next() else {
            // Nothing but spaces: no spinner to draw.
            out.push(Span {
                text: span.text.clone(),
                style: own,
            });
            continue;
        };
        let trail = signs.as_str();
        if !lead.is_empty() {
            out.push(p(&lead));
        }
        // A sign of another width keeps its own cells, so the row stays as wide as it was laid out.
        let frame = spinner_frame(g, tick);
        let pad = char_width(sign).saturating_sub(baton_core::text::width(frame));
        out.push(Span {
            text: format!("{frame}{}", " ".repeat(pad)),
            style: own.clone(),
        });
        if !trail.is_empty() {
            out.push(Span {
                text: trail.to_owned(),
                style: if trail.trim().is_empty() {
                    String::new()
                } else {
                    own
                },
            });
        }
    }
    out
}

/// The cells `from..to` of a line. A wide character cut in half leaves a space.
pub fn cut(line: &[Span], from: usize, to: usize) -> Line {
    let mut out: Line = Vec::new();
    let mut at = 0usize;
    for span in line {
        if at >= to {
            break;
        }
        let mut text = String::new();
        for ch in span.text.chars() {
            let w = char_width(ch);
            let (start, end) = (at, at + w);
            at = end;
            if w == 0 {
                if start > from && start <= to {
                    text.push(ch);
                }
            } else if start >= from && end <= to {
                text.push(ch);
            } else if end > from && start < to {
                text.push_str(&" ".repeat(end.min(to) - start.max(from)));
            }
        }
        if !text.is_empty() {
            out.push(Span {
                text,
                style: span.style.clone(),
            });
        }
    }
    out
}

/// Where a block floats over the rows of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Float {
    /// Its first row.
    pub top: usize,
    /// Its first column.
    pub left: usize,
    /// Its width.
    pub width: usize,
    /// Whether it sits on the surface colour (dialogs and toasts) or on what is under it.
    pub surface: bool,
}

/// Puts `lines`, each made as wide as the float, over `rows`. What falls outside the rows is left out.
pub fn overlay(rows: &mut [Line], float: Float, lines: &[Line]) {
    let Float {
        top,
        left,
        width,
        surface,
    } = float;
    for (i, line) in lines.iter().enumerate() {
        let Some(row) = rows.get_mut(top + i) else {
            break;
        };
        let total = line_width(row);
        if left >= total {
            continue;
        }
        let width = width.min(total - left);
        let mut next = fit_line(&cut(row, 0, left), left, "");
        let fitted = fit_line(line, width, "");
        next.extend(if surface { on_surface(&fitted) } else { fitted });
        next.extend(cut(row, left + width, total));
        *row = fit_line(&next, total, "");
    }
}

/// A line in the middle of `cols` cells. One wider than that is cut and ends in an ellipsis.
pub fn centered(line: &[Span], cols: usize) -> Line {
    let w = line_width(line);
    if w > cols {
        let mut cut = fit_line(line, cols.saturating_sub(1), "");
        cut.push(p("\u{2026}"));
        return fit_line(&cut, cols, "");
    }
    let mut out = vec![p(" ".repeat((cols - w) / 2))];
    out.extend(line.iter().cloned());
    fit_line(&out, cols, "")
}

/// What an edge carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Edge<'a> {
    /// The text at its left.
    pub left: &'a [Span],
    /// The text at its right; gives way first.
    pub right: &'a [Span],
    /// Whether it is the bottom edge.
    pub bottom: bool,
    /// The style of the border.
    pub style: &'a str,
}

/// A panel's top or bottom edge with text in it: "╭─ Title ────── note ─╮". The note gives way first,
/// then the title wraps no further: it is left out.
pub fn edge(theme: &Theme, width: usize, parts: &Edge<'_>) -> Line {
    let c = theme.g.corners;
    let style = parts.style;
    let (a, b) = if parts.bottom { (c.bl, c.br) } else { (c.tl, c.tr) };
    let mut left_part: Line = Vec::new();
    if !parts.left.is_empty() {
        left_part.push(s(format!("{} ", c.h), style));
        left_part.extend(parts.left.iter().cloned());
        left_part.push(s(" ", style));
    }
    let mut right_part: Line = Vec::new();
    if !parts.right.is_empty() {
        right_part.push(s(" ", style));
        right_part.extend(parts.right.iter().cloned());
        right_part.push(s(format!(" {}", c.h), style));
    }
    let inside = width.saturating_sub(2);
    if line_width(&right_part) + line_width(&left_part) + 1 > inside {
        right_part.clear();
    }
    if line_width(&left_part) + 1 > inside {
        left_part.clear();
    }
    let room = inside.saturating_sub(line_width(&right_part) + line_width(&left_part));
    let mut line = vec![s(a, style)];
    line.extend(left_part);
    line.push(s(c.h.repeat(room), style));
    line.extend(right_part);
    line.push(s(b, style));
    fit_line(&line, width, "")
}

/// A bordered panel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Panel<'a> {
    /// The title in the top edge.
    pub title: &'a [Span],
    /// A note in the top edge.
    pub right: &'a [Span],
    /// A note in the bottom edge.
    pub footer: &'a [Span],
    /// A second note in the bottom edge.
    pub footer_right: &'a [Span],
    /// Its width.
    pub width: usize,
    /// Its height.
    pub height: usize,
    /// Whether it has the keyboard: its border is in the accent colour, else dim.
    pub active: bool,
    /// The style of the border, when not the one `active` gives.
    pub tone: Option<&'a str>,
    /// The cells between the border and the content, on each side.
    pub pad: usize,
}

/// A bordered panel around `content`: title and a note in the top edge, a note in the bottom edge,
/// content `width - 2 - 2 * pad` wide and `height - 2` tall.
pub fn panel(theme: &Theme, look: &Panel<'_>, content: &[Line]) -> Vec<Line> {
    let style = look.tone.unwrap_or(if look.active { "accent" } else { "muted" });
    let v = theme.g.corners.v;
    let inside = look.width.saturating_sub(2 + 2 * look.pad);
    let mut out = vec![edge(
        theme,
        look.width,
        &Edge {
            left: look.title,
            right: look.right,
            bottom: false,
            style,
        },
    )];
    for line in block(content, inside, Some(look.height.saturating_sub(2))) {
        let mut row = vec![s(v, style), p(" ".repeat(look.pad))];
        row.extend(line);
        row.push(p(" ".repeat(look.pad)));
        row.push(s(v, style));
        out.push(fit_line(&row, look.width, ""));
    }
    out.push(edge(
        theme,
        look.width,
        &Edge {
            left: look.footer,
            right: look.footer_right,
            bottom: true,
            style,
        },
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::lines::plain;
    use baton_core::theme::{GlyphSet, RICH};

    #[test]
    fn blocks_are_exact() {
        let lines = vec![vec![p("abc")], vec![p("abcdefgh")]];
        let texts = |lines: Vec<Line>| lines.iter().map(|l| plain(l)).collect::<Vec<_>>();
        assert_eq!(texts(block(&lines, 5, None)), ["abc  ", "abcde"]);
        assert_eq!(texts(block(&lines, 5, Some(3))), ["abc  ", "abcde", "     "]);
        assert_eq!(texts(block(&lines, 5, Some(1))), ["abc  "]);
    }

    #[test]
    fn spinners_turn() {
        let line = vec![
            s("⠋", "accent spin"),
            p(" x"),
            s("⠋ ", "selected bold spin"),
            s("  ", "spin"),
        ];
        let at = |tick| spin(&line, &RICH, tick);
        assert_eq!(plain(&at(0)), "⠋ x⠋   ");
        assert_eq!(plain(&at(1)), "⠙ x⠙   ");
        assert_eq!(plain(&at(10)), "⠋ x⠋   ");
        let turned = at(3);
        assert_eq!(turned[0], s("⠸", "accent"));
        assert_eq!(turned[2], s("⠸", "selected bold"));
        assert_eq!(turned[3], p(" "));
        assert_eq!(line_width(&turned), line_width(&line));
    }

    #[test]
    fn cutting_and_floating() {
        let row = vec![s("abc", "x"), s("日本", "y"), p("def")];
        assert_eq!(plain(&cut(&row, 0, 3)), "abc");
        assert_eq!(plain(&cut(&row, 2, 5)), "c日");
        assert_eq!(plain(&cut(&row, 2, 4)), "c ");
        assert_eq!(plain(&cut(&row, 4, 8)), " 本d");
        assert_eq!(plain(&cut(&row, 7, 20)), "def");
        assert_eq!(cut(&row, 2, 5), vec![s("c", "x"), s("日", "y")]);
        let mut rows = vec![
            fit_line(&[p("0123456789")], 10, ""),
            fit_line(&[p("abcdefghij")], 10, ""),
        ];
        let float = |top, left, surface| Float {
            top,
            left,
            width: 4,
            surface,
        };
        overlay(
            &mut rows,
            float(1, 3, true),
            &[vec![s("XY", "bold")], vec![p("never")]],
        );
        assert_eq!(plain(&rows[0]), "0123456789");
        assert_eq!(plain(&rows[1]), "abcXY  hij");
        assert_eq!(rows[1][1], s("XY", "bold bg-surface"));
        assert_eq!(line_width(&rows[1]), 10);
        // A float past the right edge is cut at the edge.
        overlay(&mut rows, float(0, 8, false), &[vec![p("WXYZ")]]);
        assert_eq!(plain(&rows[0]), "01234567WX");
        assert_eq!(rows[0].last(), Some(&p("WX")));
    }

    #[test]
    fn centering() {
        assert_eq!(plain(&centered(&[p("abc")], 9)), "   abc   ");
        assert_eq!(plain(&centered(&[p("abc")], 8)), "  abc   ");
        assert_eq!(plain(&centered(&[p("abcdefgh")], 5)), "abcd\u{2026}");
    }

    #[test]
    fn edges_and_panels() {
        let theme = make_theme(false, GlyphSet::Rich);
        let title = [s("Title", "bold")];
        let note = [s("note", "muted")];
        let top = |width, right: &[Span]| {
            plain(&edge(
                &theme,
                width,
                &Edge {
                    left: &title,
                    right,
                    bottom: false,
                    style: "muted",
                },
            ))
        };
        assert_eq!(top(24, &note), "╭─ Title ─────── note ─╮");
        assert_eq!(top(16, &note), "╭─ Title ──────╮", "the note gives way first");
        assert_eq!(top(8, &[]), "╭──────╮", "then the title is left out");
        assert_eq!(
            plain(&edge(
                &theme,
                6,
                &Edge {
                    bottom: true,
                    style: "muted",
                    ..Edge::default()
                }
            )),
            "╰────╯"
        );
        let drawn = panel(
            &theme,
            &Panel {
                title: &title,
                width: 14,
                height: 4,
                pad: 1,
                ..Panel::default()
            },
            &[vec![p("one")], vec![p("two")], vec![p("three")]],
        );
        assert_eq!(
            drawn.iter().map(|l| plain(l)).collect::<Vec<_>>(),
            [
                "╭─ Title ────╮",
                "│ one        │",
                "│ two        │",
                "╰────────────╯"
            ]
        );
    }

    #[test]
    fn how_a_span_is_drawn() {
        let colour = make_theme(true, GlyphSet::Rich);
        let plain_text = props(&colour, "");
        assert_eq!(plain_text.color.as_deref(), Some("#d6d8dc"));
        assert_eq!(plain_text.background.as_deref(), Some("#121519"));
        let side = props(&colour, "muted bg-surface");
        assert_eq!(
            (side.color.as_deref(), side.background.as_deref()),
            (Some("#7e858e"), Some("#181b20"))
        );
        let selected = props(&colour, "selected bold bg-surface");
        assert_eq!(selected.background.as_deref(), Some("#35302a"));
        let mono = make_theme(false, GlyphSet::Rich);
        assert_eq!(props(&mono, "muted bg-surface"), TextProps::default());
        let rule = props(&colour, "divider bg-none");
        assert_eq!((rule.color.as_deref(), rule.background), (Some("#343a43"), None));
        assert_eq!(
            on_surface(&[p("a"), s("b", "bold")]),
            vec![s("a", "bg-surface"), s("b", "bold bg-surface")]
        );
    }
}
