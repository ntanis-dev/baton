//! What the plain commands print (baton install, status, doctor, ...) is built like the menu's panes:
//! lines of spans (`lines`) in the menu's palette and glyphs, measured in terminal cells and wrapped to
//! the terminal. This module turns them into ANSI text for a stream, or into plain text when it is not
//! a terminal (a pipe, a test, the /hop command's answer), `NO_COLOR` is set, or the terminal has no
//! colour.

use crate::art::branding;
use crate::env::Env;
use crate::lines::{Line, Span, line_width, p, s, wrap_line};
use crate::theme::{GlyphSet, Glyphs, TextProps, Theme, make_theme, pick_glyphs};

/// The ANSI code of a colour's name.
fn named(color: &str) -> Option<u32> {
    Some(match color {
        "black" => 30,
        "red" => 31,
        "green" => 32,
        "yellow" => 33,
        "blue" => 34,
        "magenta" => 35,
        "cyan" => 36,
        "white" => 37,
        "gray" | "grey" => 90,
        "redBright" => 91,
        "greenBright" => 92,
        "yellowBright" => 93,
        "blueBright" => 94,
        "magentaBright" => 95,
        "cyanBright" => 96,
        "whiteBright" => 97,
        _ => return None,
    })
}

/// The sixteen basic colours, for terminals with no more.
const BASIC: [[i32; 3]; 16] = [
    [0, 0, 0],
    [205, 49, 49],
    [13, 188, 121],
    [229, 229, 16],
    [36, 114, 200],
    [188, 63, 188],
    [17, 168, 205],
    [229, 229, 229],
    [102, 102, 102],
    [241, 76, 76],
    [35, 209, 139],
    [245, 245, 67],
    [59, 142, 234],
    [214, 112, 214],
    [41, 184, 219],
    [255, 255, 255],
];

/// `#rrggbb` as its three parts.
fn rgb(hex: &str) -> [i32; 3] {
    let part = |at: usize| {
        hex.get(at..at + 2)
            .and_then(|h| i32::from_str_radix(h, 16).ok())
            .unwrap_or(0)
    };
    [part(1), part(3), part(5)]
}

/// A number rounded with halves going up.
fn round(n: f64) -> i32 {
    (n + 0.5).floor() as i32
}

/// The codes that set a colour (a name or `#rrggbb`) at a colour depth (24, 8 or 4 bits).
pub fn color_codes(color: &str, background: bool, depth: u8) -> Vec<u32> {
    let base = if background { 10 } else { 0 };
    if let Some(code) = named(color) {
        return vec![code + base];
    }
    let [r, g, b] = rgb(color);
    let set = if background { 48 } else { 38 };
    if depth >= 24 {
        return vec![set, 2, r as u32, g as u32, b as u32];
    }
    if depth >= 8 {
        let gray = (r - g).abs() < 8 && (g - b).abs() < 8;
        let level = |v: i32| round(f64::from(v) / 255.0 * 5.0);
        let index = if gray {
            232 + round(f64::from(r - 8) / 247.0 * 24.0).min(23)
        } else {
            16 + 36 * level(r) + 6 * level(g) + level(b)
        };
        return vec![set, 5, index.max(0) as u32];
    }
    let mut best = 0;
    let mut distance = i32::MAX;
    for (i, [cr, cg, cb]) in BASIC.iter().enumerate() {
        let d = (cr - r).pow(2) + (cg - g).pow(2) + (cb - b).pow(2);
        if d < distance {
            best = i as u32;
            distance = d;
        }
    }
    vec![(if best < 8 { 30 + best } else { 82 + best }) + base]
}

/// The escape sequence that opens a style at a colour depth; empty when the style draws nothing. What
/// it opens is closed by `[0m`.
pub fn escape(props: &TextProps, depth: u8) -> String {
    let mut codes: Vec<u32> = Vec::new();
    if props.bold {
        codes.push(1);
    }
    if props.dim {
        codes.push(2);
    }
    if props.italic {
        codes.push(3);
    }
    if props.underline {
        codes.push(4);
    }
    if props.inverse {
        codes.push(7);
    }
    if let Some(color) = &props.color {
        codes.extend(color_codes(color, false, depth));
    }
    if let Some(background) = &props.background {
        codes.extend(color_codes(background, true, depth));
    }
    if codes.is_empty() {
        return String::new();
    }
    format!(
        "\x1b[{}m",
        codes.iter().map(u32::to_string).collect::<Vec<_>>().join(";")
    )
}

/// The terminal a command writes to: whether it styles, how wide it is, which glyph set it can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminal {
    /// Whether it is a terminal at all.
    pub tty: bool,
    /// Its colour depth in bits: 1 (none), 4, 8 or 24.
    pub depth: u8,
    /// Whether styles are written.
    pub styled: bool,
    /// Which glyph set it shows.
    pub glyphs: GlyphSet,
    /// Its look.
    pub theme: Theme,
    /// Its glyphs.
    pub g: &'static Glyphs,
    /// The width to lay out for.
    pub cols: usize,
}

/// The colour depth of a terminal, from its environment: none under `NO_COLOR` or a dumb terminal,
/// what `FORCE_COLOR` says, else what the terminal says of itself.
pub fn color_depth(env: &Env) -> u8 {
    if let Some(force) = env.get("FORCE_COLOR") {
        return match force {
            "" | "1" | "true" => 4,
            "2" => 8,
            "3" => 24,
            _ => 1,
        };
    }
    if env.has("NODE_DISABLE_COLORS") || env.has("NO_COLOR") || env.get("TERM") == Some("dumb") {
        return 1;
    }
    if cfg!(windows) {
        // Every console of Windows 10 and later draws 24-bit colour.
        return 24;
    }
    if env.has("TMUX") {
        return 24;
    }
    let colorterm = env.get("COLORTERM").unwrap_or_default();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return 24;
    }
    match env.get("TERM_PROGRAM").unwrap_or_default() {
        "iTerm.app" | "HyperTerm" | "MacTerm" | "vscode" | "WezTerm" | "ghostty" => return 24,
        "Apple_Terminal" => return 8,
        _ => {}
    }
    let term = env.get("TERM").unwrap_or_default().to_lowercase();
    if term.contains("256") {
        return 8;
    }
    let known = [
        "xterm", "screen", "vt100", "vt220", "rxvt", "color", "ansi", "cygwin", "linux", "konsole", "kitty",
    ];
    if known.iter().any(|name| term.contains(name)) || !colorterm.is_empty() {
        return 4;
    }
    1
}

/// The terminal for a stream: `tty` says whether it is one and `columns` how wide. A stream that is
/// not a terminal gets plain text (set `FORCE_COLOR` to style it anyway); `NO_COLOR` drops every style.
pub fn terminal_for(tty: bool, columns: Option<usize>, env: &Env) -> Terminal {
    let forced = env
        .get("FORCE_COLOR")
        .is_some_and(|force| !force.is_empty() && force != "0");
    let depth = if tty {
        color_depth(env)
    } else if forced && !env.has("NO_COLOR") {
        8
    } else {
        1
    };
    // Layout width: the terminal's, kept readable on a wide one.
    let cols = columns.filter(|c| *c > 0).unwrap_or(100).clamp(60, 110);
    make_terminal(tty, depth, pick_glyphs(env), cols)
}

/// A terminal with these properties.
pub fn make_terminal(tty: bool, depth: u8, glyphs: GlyphSet, cols: usize) -> Terminal {
    Terminal {
        tty,
        depth,
        styled: depth >= 4,
        glyphs,
        theme: make_theme(depth >= 4, glyphs),
        g: glyphs.glyphs(),
        cols,
    }
}

/// The colour depth to assume for output that goes through a pipe to a program that draws it (the
/// status line Claude Code shows): from the environment it inherits, since the pipe says nothing.
/// `NO_COLOR`: none.
pub fn inherited_depth(env: &Env) -> u8 {
    if env.has("NO_COLOR") {
        return 1;
    }
    let colorterm = env.get("COLORTERM").unwrap_or_default().to_lowercase();
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return 24;
    }
    if env.has("WT_SESSION") || env.has("TERM_PROGRAM") {
        return 24;
    }
    if env
        .get("TERM")
        .unwrap_or_default()
        .to_lowercase()
        .contains("256color")
    {
        return 8;
    }
    4
}

/// Lines of spans to text, one line per row. Trailing blanks are trimmed in plain text.
pub fn render(lines: &[Line], term: &Terminal) -> String {
    lines
        .iter()
        .map(|line| {
            let mut out = String::new();
            // Neighbouring spans of one style go out under one escape.
            let mut runs: Vec<Span> = Vec::new();
            for span in line {
                match runs.last_mut() {
                    Some(last) if last.style == span.style => last.text.push_str(&span.text),
                    _ => runs.push(span.clone()),
                }
            }
            for run in runs {
                let open = if term.styled {
                    escape(&term.theme.props(&run.style), term.depth)
                } else {
                    String::new()
                };
                if open.is_empty() {
                    out.push_str(&run.text);
                } else {
                    out.push_str(&open);
                    out.push_str(&run.text);
                    out.push_str("\x1b[0m");
                }
            }
            if term.styled {
                out
            } else {
                out.trim_end_matches(' ').to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The spaces every line of a document starts with.
pub const INDENT: usize = 2;

/// A document: every line indented, and in a terminal a blank line above and below.
pub fn document(lines: &[Line], term: &Terminal) -> Vec<Line> {
    let mut body: Vec<Line> = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                return Vec::new();
            }
            let mut indented = vec![p(" ".repeat(INDENT))];
            indented.extend(line.iter().cloned());
            indented
        })
        .collect();
    if term.tty {
        body.insert(0, Vec::new());
        body.push(Vec::new());
    }
    body
}

/// An error for stderr: "✗ message" in a terminal, "baton: message" otherwise.
pub fn error_text(message: &str, term: &Terminal) -> String {
    if !term.styled {
        return format!("baton: {message}");
    }
    render(
        &[vec![s(term.g.cross, "err bold"), p(" "), s(message, "text")]],
        term,
    )
}

/// The width of what a document's lines may use.
pub fn room(term: &Terminal) -> usize {
    term.cols.saturating_sub(INDENT * 2)
}

/// The Baton mark: the wordmark and slogan (`full`), or the mark and name on one line. Nothing when
/// the output is not a terminal.
pub fn banner(term: &Terminal, full: bool) -> Vec<Line> {
    if !term.tty {
        return Vec::new();
    }
    if full {
        let mut out = branding("");
        out.push(Vec::new());
        return out;
    }
    vec![
        vec![s(term.g.baton, "brand"), s(" BATON", "brand bold")],
        Vec::new(),
    ]
}

/// "TITLE ──────── note": a section heading with its rule, the menu's.
pub fn heading(title: &str, width: usize, term: &Terminal, note: &str) -> Line {
    let tail = if note.is_empty() {
        Vec::new()
    } else {
        vec![s(note, "muted")]
    };
    let taken = line_width(&[p(title)]) + 2 + if note.is_empty() { 0 } else { line_width(&tail) + 2 };
    let rule = width.saturating_sub(taken);
    let mut out = vec![
        s(title, "muted bold"),
        s(format!("  {}", term.g.corners.h.repeat(rule)), "divider"),
    ];
    if !note.is_empty() {
        out.push(p("  "));
        out.extend(tail);
    }
    out
}

/// `text` padded with spaces to `width` characters.
fn pad_end(text: &str, width: usize) -> String {
    let have = text.chars().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(have)))
}

/// "label   value" with the labels in a column and a long value wrapped under itself; `value` is
/// spans.
pub fn fact(label: &str, value: &[Span], width: usize, label_width: usize, style: &str) -> Vec<Line> {
    let mut line = vec![s(pad_end(label, label_width), style)];
    line.extend(value.iter().cloned());
    wrap_line(&line, width, label_width)
}

/// A list entry: a mark, a name column and what follows, wrapped under itself.
pub fn entry(mark: &[Span], name: &str, value: &[Span], width: usize, name_width: usize) -> Vec<Line> {
    let mut head: Line = mark.to_vec();
    head.push(p(" "));
    head.push(s(pad_end(name, name_width), "bold"));
    let lead = line_width(&head) + 1;
    head.push(p(" "));
    head.extend(value.iter().cloned());
    wrap_line(&head, width, lead)
}

/// A paragraph of one style.
pub fn paragraph(text: &str, width: usize, style: &str) -> Vec<Line> {
    wrap_line(&[s(text, style)], width, 0)
}

/// The mark of an outcome, in the terminal's glyphs: `ok`, `done`, `same`, `fail`, `warn`, `info`,
/// `skip`.
pub fn mark(g: &Glyphs, kind: &str) -> Line {
    vec![match kind {
        "ok" | "done" => s(g.check, "ok bold"),
        "same" => s(g.check, "muted"),
        "fail" => s(g.cross, "err bold"),
        "warn" => s("!", "warn bold"),
        "skip" => s(g.ring, "muted"),
        _ => s(g.dot, "muted"),
    }]
}

/// The one-line answer to a command that did something: "✓ Added acct2".
pub fn outcome(term: &Terminal, text: &str, kind: &str) -> Line {
    let mut out = mark(term.g, kind);
    out.push(p(" "));
    out.push(s(text, if kind == "fail" { "err bold" } else { "bold" }));
    out
}

/// A command's next step, in the accent colour.
pub fn next(text: &str, width: usize, label_width: usize) -> Vec<Line> {
    fact("Next", &[s(text, "accent bold")], width, label_width, "muted")
}

/// Shown paths: the home folder as ~ and forward slashes.
pub fn tildify(path: &str, home: &str) -> String {
    let a = path.replace('\\', "/");
    let h = home.replace('\\', "/");
    let h = h.strip_suffix('/').unwrap_or(&h);
    let (a_low, h_low) = (a.to_lowercase(), h.to_lowercase());
    if a_low == h_low {
        return "~".to_owned();
    }
    if a_low.starts_with(&format!("{h_low}/")) && a.is_char_boundary(h.len()) {
        return format!("~{}", &a[h.len()..]);
    }
    a
}

/// The keys a notice's hints name.
const KEYS: [&str; 4] = ["Enter", "Ctrl+C", "any other key", "any key"];

/// A hint split into what is a key's name and what is not, in order.
fn hint_parts(hint: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut rest = hint;
    loop {
        let found = KEYS
            .iter()
            .filter_map(|key| rest.find(key).map(|at| (at, *key)))
            .min_by_key(|(at, key)| (*at, std::cmp::Reverse(key.len())));
        let Some((at, key)) = found else {
            out.push((rest, false));
            return out;
        };
        out.push((&rest[..at], false));
        out.push((key, true));
        rest = &rest[at + key.len()..];
    }
}

/// A notice between Claude Code's own output (a baton session): "baton: text" with the mark in front,
/// the key hints after the first " · " dimmed.
pub fn notice(line: &str, term: &Terminal) -> String {
    let Some(rest) = line.strip_prefix("baton: ").filter(|_| term.styled) else {
        return line.to_owned();
    };
    let mut parts = rest.split(" · ");
    let head = parts.next().unwrap_or_default();
    let mut body = vec![s(term.g.baton, "brand"), p(" "), s(head, "text")];
    for hint in parts {
        body.push(s(" · ", "divider"));
        for (part, key) in hint_parts(hint) {
            body.push(s(part, if key { "accent bold" } else { "muted" }));
        }
    }
    render(&[body], term)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::plain;

    fn plain_term() -> Terminal {
        make_terminal(false, 1, GlyphSet::Rich, 80)
    }

    #[test]
    fn colours_at_each_depth() {
        assert_eq!(color_codes("red", false, 24), [31]);
        assert_eq!(color_codes("whiteBright", true, 4), [107]);
        assert_eq!(color_codes("#d7ac78", false, 24), [38, 2, 215, 172, 120]);
        assert_eq!(color_codes("#d7ac78", true, 24), [48, 2, 215, 172, 120]);
        assert_eq!(color_codes("#d7ac78", false, 8), [38, 5, 180]);
        assert_eq!(color_codes("#7e7e7e", false, 8), [38, 5, 243]);
        assert_eq!(color_codes("#cf8b82", false, 4), [95]);
        assert_eq!(color_codes("#121519", true, 4), [40]);
    }

    #[test]
    fn rendering() {
        let lines = vec![vec![s("a", "bold"), s("b", "bold"), p(" c  ")], vec![]];
        assert_eq!(render(&lines, &plain_term()), "ab c\n");
        let styled = make_terminal(true, 24, GlyphSet::Rich, 80);
        assert_eq!(render(&lines, &styled), "\x1b[1mab\x1b[0m c  \n");
        assert_eq!(
            render(&[vec![s("x", "ok")]], &styled),
            "\x1b[38;2;146;174;153mx\x1b[0m"
        );
        assert_eq!(
            render(
                &[vec![s("x", "dim underline")]],
                &make_terminal(true, 4, GlyphSet::Rich, 80)
            ),
            "\x1b[4;90mx\x1b[0m"
        );
    }

    #[test]
    fn which_terminal() {
        let env = Env::new();
        let piped = terminal_for(false, None, &env);
        assert_eq!((piped.styled, piped.cols, piped.depth), (false, 100, 1));
        assert_eq!(terminal_for(false, Some(300), &env).cols, 110);
        assert_eq!(terminal_for(false, Some(20), &env).cols, 60);
        let forced = Env::from_pairs([("FORCE_COLOR", "1")]);
        assert_eq!(terminal_for(false, None, &forced).depth, 8);
        let both = Env::from_pairs([("FORCE_COLOR", "1"), ("NO_COLOR", "1")]);
        assert_eq!(terminal_for(false, None, &both).depth, 1);
        assert!(!terminal_for(true, Some(90), &Env::from_pairs([("NO_COLOR", "1")])).styled);
        assert_eq!(color_depth(&Env::from_pairs([("FORCE_COLOR", "3")])), 24);
        assert_eq!(color_depth(&Env::from_pairs([("TERM", "dumb")])), 1);
    }

    #[test]
    fn the_depth_a_pipe_inherits() {
        assert_eq!(inherited_depth(&Env::new()), 4);
        assert_eq!(
            inherited_depth(&Env::from_pairs([("NO_COLOR", "1"), ("COLORTERM", "truecolor")])),
            1
        );
        assert_eq!(
            inherited_depth(&Env::from_pairs([("COLORTERM", "TrueColor")])),
            24
        );
        assert_eq!(inherited_depth(&Env::from_pairs([("WT_SESSION", "x")])), 24);
        assert_eq!(inherited_depth(&Env::from_pairs([("TERM", "xterm-256color")])), 8);
    }

    #[test]
    fn documents_and_their_pieces() {
        let term = plain_term();
        let lines = vec![vec![p("one")], vec![], vec![p("two")]];
        let doc = document(&lines, &term);
        assert_eq!(render(&doc, &term), "  one\n\n  two");
        let tty = make_terminal(true, 1, GlyphSet::Rich, 80);
        assert_eq!(document(&lines, &tty).len(), 5);
        assert_eq!(room(&term), 76);
        assert!(banner(&term, true).is_empty());
        assert_eq!(plain(&banner(&tty, false)[0]), "╺━╸ BATON");
        assert_eq!(banner(&tty, true).len(), 10);
        assert_eq!(plain(&heading("ACCOUNTS", 20, &term, "")), "ACCOUNTS  ──────────");
        assert_eq!(
            plain(&heading("ACCOUNTS", 20, &term, "2")),
            "ACCOUNTS  ───────  2"
        );
        let fact_lines = fact("Profile", &[p("a long value that wraps")], 24, 10, "muted");
        assert_eq!(
            fact_lines.iter().map(|l| plain(l)).collect::<Vec<_>>(),
            ["Profile   a long value", "          that wraps"]
        );
        let entry_lines = entry(&mark(term.g, "ok"), "Work", &[p("ready to go now")], 20, 6);
        assert_eq!(
            entry_lines.iter().map(|l| plain(l)).collect::<Vec<_>>(),
            ["✓ Work   ready to go", "         now"]
        );
        assert_eq!(plain(&paragraph("some words", 5, "muted")[1]), "words");
        assert_eq!(plain(&outcome(&term, "Added Work", "done")), "✓ Added Work");
        assert_eq!(outcome(&term, "No", "fail")[2].style, "err bold");
        assert_eq!(plain(&mark(term.g, "warn")), "!");
        assert_eq!(plain(&mark(term.g, "skip")), "○");
        assert_eq!(plain(&mark(term.g, "info")), "●");
        assert_eq!(
            plain(&next("baton login Work", 40, 10)[0]),
            "Next      baton login Work"
        );
    }

    #[test]
    fn errors_and_paths() {
        assert_eq!(error_text("nope", &plain_term()), "baton: nope");
        let styled = make_terminal(true, 4, GlyphSet::Plain, 80);
        assert!(
            error_text("nope", &styled).contains("x\x1b[0m nope")
                || error_text("nope", &styled).contains("nope")
        );
        assert_eq!(tildify("C:\\Users\\op\\.baton\\x", "C:\\Users\\op"), "~/.baton/x");
        assert_eq!(tildify("c:/users/OP", "C:\\Users\\op\\"), "~");
        assert_eq!(tildify("/home/other/x", "/home/op"), "/home/other/x");
        assert_eq!(tildify("/home/operator", "/home/op"), "/home/operator");
    }

    #[test]
    fn notices() {
        let line = "baton: moving to Spare in 10 s · Enter: now · any other key: stay on Work · Ctrl+C: quit";
        assert_eq!(notice(line, &plain_term()), line);
        let styled = make_terminal(true, 4, GlyphSet::Rich, 80);
        assert_eq!(notice("something else", &styled), "something else");
        let drawn = notice(line, &styled);
        let bare = crate::text::clean(&drawn);
        assert_eq!(
            bare,
            "╺━╸ moving to Spare in 10 s · Enter: now · any other key: stay on Work · Ctrl+C: quit"
        );
        assert_eq!(
            hint_parts("any other key: stay"),
            vec![("", false), ("any other key", true), (": stay", false)]
        );
        assert_eq!(hint_parts("no keys"), vec![("no keys", false)]);
    }
}
