//! Dense content is built as lines of spans, each a text and a style, measured in terminal cells; the
//! commands print them and the menu draws them. A style is space-separated words (`theme`): bold, dim,
//! italic, underline, inverse, selected, spin, colour names or the theme's roles (accent, ok, warn, err,
//! info, muted, brand, hot).

use crate::text::{char_width, clean, width};

/// A run of text in one style.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    /// The text, already clean and on one line.
    pub text: String,
    /// The style words.
    pub style: String,
}

/// A line: spans, left to right.
pub type Line = Vec<Span>;

/// A span of `text` in `style` (the text cleaned, a line break in it a space).
pub fn s(text: impl AsRef<str>, style: &str) -> Span {
    Span {
        text: clean(text.as_ref()).replace('\n', " "),
        style: style.to_owned(),
    }
}

/// A span of `text` with no style.
pub fn p(text: impl AsRef<str>) -> Span {
    s(text, "")
}

/// A span of text that is already clean, as it is.
fn raw(text: impl Into<String>, style: &str) -> Span {
    Span {
        text: text.into(),
        style: style.to_owned(),
    }
}

/// The cells a line takes.
pub fn line_width(line: &[Span]) -> usize {
    line.iter().map(|span| width(&span.text)).sum()
}

/// Exactly `cells` wide: padded with `fill`'s style. A line that was not made to fit is cut at the
/// edge as a last resort, without an ellipsis (the views wrap or drop parts instead, so this does not
/// happen in practice).
pub fn fit_line(line: &[Span], cells: usize, fill: &str) -> Line {
    let mut out = Vec::new();
    let mut used = 0;
    for span in line {
        if used >= cells {
            break;
        }
        let w = width(&span.text);
        if used + w <= cells {
            out.push(span.clone());
            used += w;
            continue;
        }
        let mut part = String::new();
        for ch in span.text.chars() {
            let cw = char_width(ch);
            if used + cw > cells {
                break;
            }
            part.push(ch);
            used += cw;
        }
        out.push(raw(part, &span.style));
        break;
    }
    if used < cells {
        out.push(raw(" ".repeat(cells - used), fill));
    }
    out
}

/// Left and right parts on one line, the gap padded; a right part that does not fit is left out.
pub fn spread(left: &[Span], right: &[Span], cells: usize, fill: &str) -> Line {
    let right_width = line_width(right);
    if right.is_empty() || cells < right_width + line_width(left) + 1 {
        return fit_line(left, cells, fill);
    }
    let mut out = fit_line(left, cells - right_width, fill);
    out.extend(right.iter().cloned());
    out
}

/// The style words a restyled span keeps of its own.
const KEPT: [&str; 2] = ["bold", "spin"];

/// A line in one `style`; each span keeps its own bold and spin.
pub fn restyle(line: &[Span], style: &str) -> Line {
    line.iter()
        .map(|span| {
            let mut words = vec![style];
            for kept in KEPT {
                if span.style.split(' ').any(|w| w == kept) && !style.split(' ').any(|w| w == kept) {
                    words.push(kept);
                }
            }
            raw(span.text.clone(), &words.join(" "))
        })
        .collect()
}

/// A line's text without its styles.
pub fn plain(line: &[Span]) -> String {
    line.iter().map(|span| span.text.as_str()).collect()
}

/// The characters a long word may be broken after.
fn is_break(ch: char) -> bool {
    matches!(ch, '-' | '/' | '\\' | ':' | '_' | '.')
}

/// Whether a token is a run of spaces.
fn is_blank(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b == b' ')
}

/// A text as its words and the runs of spaces between them, in order.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut blank = None;
    for (at, ch) in text.char_indices() {
        let is_space = ch == ' ';
        if blank.is_some_and(|b| b != is_space) {
            out.push(&text[start..at]);
            start = at;
        }
        blank = Some(is_space);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// A line of spans wrapped to `cells`: at spaces where it can, inside a word longer than a line.
/// Continuation lines start with `indent` spaces. Nothing is cut.
pub fn wrap_line(line: &[Span], cells: usize, indent: usize) -> Vec<Line> {
    let room = cells.max(1);
    let lead = indent.min(room - 1);
    let mut out: Vec<Line> = Vec::new();
    let mut current: Line = Vec::new();
    let mut used = 0usize;
    // Whether nothing but the indent is on the current line.
    fn at_start(out: &[Line], used: usize, lead: usize) -> bool {
        used == if out.is_empty() { 0 } else { lead }
    }
    fn flush(out: &mut Vec<Line>, current: &mut Line, used: &mut usize, lead: usize) {
        while current.last().is_some_and(|last| is_blank(&last.text)) {
            current.pop();
        }
        out.push(std::mem::take(current));
        if lead > 0 {
            current.push(Span {
                text: " ".repeat(lead),
                style: String::new(),
            });
        }
        *used = lead;
    }
    for span in line {
        for text in tokens(&span.text) {
            let w = width(text);
            if is_blank(text) {
                if !out.is_empty() && at_start(&out, used, lead) {
                    continue;
                }
                if used + w > room {
                    flush(&mut out, &mut current, &mut used, lead);
                    continue;
                }
                current.push(raw(text, &span.style));
                used += w;
                continue;
            }
            if used + w > room && !at_start(&out, used, lead) && w <= room - lead {
                flush(&mut out, &mut current, &mut used, lead);
            }
            if used + w <= room {
                current.push(raw(text, &span.style));
                used += w;
                continue;
            }
            let mut rest: Vec<char> = text.chars().collect();
            while !rest.is_empty() {
                let mut taken = 0usize;
                let mut fits = 0usize;
                let mut mark = 0usize;
                for ch in &rest {
                    let cw = char_width(*ch);
                    if used + taken + cw > room {
                        break;
                    }
                    taken += cw;
                    fits += 1;
                    if is_break(*ch) {
                        mark = fits;
                    }
                }
                if fits == rest.len() {
                    current.push(raw(rest.iter().collect::<String>(), &span.style));
                    used += taken;
                    break;
                }
                if mark == 0 && !at_start(&out, used, lead) {
                    flush(&mut out, &mut current, &mut used, lead);
                    continue;
                }
                let take = if mark > 0 { mark } else { fits.max(1) };
                let piece: String = rest[..take].iter().collect();
                used += width(&piece);
                current.push(raw(piece, &span.style));
                rest.drain(..take);
                flush(&mut out, &mut current, &mut used, lead);
            }
        }
    }
    if !current.is_empty() && !(!out.is_empty() && at_start(&out, used, lead)) {
        flush(&mut out, &mut current, &mut used, lead);
    }
    if out.is_empty() { vec![Vec::new()] } else { out }
}

/// How facts are laid out by [`wrap_facts`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    /// What goes between two facts on a line.
    pub sep: Span,
    /// The spaces continuation lines start with.
    pub indent: usize,
    /// What the first line starts with.
    pub lead: Line,
}

impl Default for Facts {
    fn default() -> Facts {
        Facts {
            sep: raw(" · ", "muted"),
            indent: 0,
            lead: Vec::new(),
        }
    }
}

/// Facts (each a list of spans) on lines `cells` wide with a separator; a line breaks between facts.
pub fn wrap_facts(facts: &[Line], cells: usize, layout: &Facts) -> Vec<Line> {
    let room = cells.max(1);
    let sep_width = width(&layout.sep.text);
    let mut out = Vec::new();
    let mut line = layout.lead.clone();
    let mut used = line_width(&layout.lead);
    let mut filled = false;
    for fact in facts {
        if fact.is_empty() {
            continue;
        }
        let w = line_width(fact);
        if filled && used + sep_width + w > room {
            out.push(std::mem::take(&mut line));
            if layout.indent > 0 {
                line.push(raw(" ".repeat(layout.indent), ""));
            }
            used = layout.indent;
        } else if filled {
            line.push(layout.sep.clone());
            used += sep_width;
        }
        if used + w > room {
            line.extend(fact.iter().cloned());
            let mut parts = wrap_line(&line, room, layout.indent);
            line = parts.pop().unwrap_or_default();
            out.extend(parts);
            used = line_width(&line);
        } else {
            line.extend(fact.iter().cloned());
            used += w;
        }
        filled = true;
    }
    out.push(line);
    out
}

/// Every line wrapped to `cells`.
pub fn wrap_lines(lines: &[Line], cells: usize, indent: usize) -> Vec<Line> {
    lines
        .iter()
        .flat_map(|line| wrap_line(line, cells, indent))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|line| plain(line)).collect()
    }

    #[test]
    fn spans_are_clean_and_one_line() {
        assert_eq!(
            s("a\nb\x1b[1m", "bold"),
            Span {
                text: "a b".into(),
                style: "bold".into()
            }
        );
        assert_eq!(p("x").style, "");
        assert_eq!(line_width(&[p("ab"), p("日本")]), 6);
    }

    #[test]
    fn fitting_a_line() {
        assert_eq!(fit_line(&[p("ab")], 5, "dim"), vec![p("ab"), s("   ", "dim")]);
        assert_eq!(fit_line(&[p("abcdef"), p("gh")], 4, ""), vec![p("abcd")]);
        assert_eq!(plain(&fit_line(&[p("ab"), p("cdef")], 4, "")), "abcd");
        // A wide character that would straddle the edge is left out and the cell padded.
        assert_eq!(plain(&fit_line(&[p("a日本")], 4, "")), "a日 ");
        assert_eq!(fit_line(&[], 0, ""), Vec::<Span>::new());
    }

    #[test]
    fn left_and_right() {
        assert_eq!(
            plain(&spread(&[p("left")], &[p("right")], 12, "")),
            "left   right"
        );
        assert_eq!(
            plain(&spread(&[p("left")], &[p("right")], 9, "")),
            "left     ",
            "no room: the right part goes"
        );
        assert_eq!(plain(&spread(&[p("left")], &[], 6, "")), "left  ");
    }

    #[test]
    fn restyling_keeps_bold_and_spin() {
        let line = vec![s("a", "ok bold"), s("b", "muted"), s("c", "spin err")];
        assert_eq!(
            restyle(&line, "selected"),
            vec![
                s("a", "selected bold"),
                s("b", "selected"),
                s("c", "selected spin")
            ]
        );
        assert_eq!(restyle(&[s("a", "bold")], "dim bold"), vec![s("a", "dim bold")]);
    }

    #[test]
    fn wrapping_a_line_of_spans() {
        let line = vec![s("one two ", "bold"), s("three four", "")];
        assert_eq!(texts(&wrap_line(&line, 9, 0)), ["one two", "three", "four"]);
        assert_eq!(texts(&wrap_line(&line, 9, 2)), ["one two", "  three", "  four"]);
        assert_eq!(
            wrap_line(&line, 9, 0)[0],
            vec![s("one", "bold"), s(" ", "bold"), s("two", "bold")]
        );
        // A long word breaks after a separator when it can, anywhere when it cannot.
        assert_eq!(
            texts(&wrap_line(&[p("C:/Users/someone/project")], 10, 0)),
            ["C:/Users/", "someone/", "project"]
        );
        assert_eq!(
            texts(&wrap_line(&[p("abcdefghijkl")], 5, 0)),
            ["abcde", "fghij", "kl"]
        );
        assert_eq!(
            texts(&wrap_line(&[p("ab abcdefghijkl")], 5, 1)),
            ["ab", " abcd", " efgh", " ijkl"]
        );
        assert_eq!(wrap_line(&[], 5, 0), vec![Vec::<Span>::new()]);
        assert_eq!(texts(&wrap_line(&[p("fits")], 10, 4)), ["fits"]);
    }

    #[test]
    fn facts_break_between_facts() {
        let facts = vec![vec![p("alpha")], vec![p("beta")], vec![], vec![p("gamma")]];
        assert_eq!(
            texts(&wrap_facts(&facts, 30, &Facts::default())),
            ["alpha · beta · gamma"]
        );
        assert_eq!(
            texts(&wrap_facts(&facts, 13, &Facts::default())),
            ["alpha · beta", "gamma"]
        );
        let indented = Facts {
            indent: 2,
            lead: vec![p("> ")],
            ..Facts::default()
        };
        assert_eq!(
            texts(&wrap_facts(&facts, 14, &indented)),
            ["> alpha · beta", "  gamma"]
        );
        // A fact longer than a line wraps like any line.
        let long = vec![vec![p("a")], vec![p("one two three four five")]];
        assert_eq!(
            texts(&wrap_facts(&long, 12, &Facts::default())),
            ["a", "one two", "three four", "five"]
        );
        assert_eq!(wrap_facts(&[], 10, &Facts::default()), vec![Vec::<Span>::new()]);
    }

    #[test]
    fn many_lines() {
        let lines = vec![vec![p("one two")], vec![p("three")]];
        assert_eq!(texts(&wrap_lines(&lines, 4, 0)), ["one", "two", "thre", "e"]);
    }
}
