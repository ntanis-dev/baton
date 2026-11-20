//! What the plain commands print: plain text for a pipe, the menu's palette for a terminal.

use baton_core::env::Env;
use baton_core::lines::{Line, p, s};
use baton_core::term::{Terminal, error_text, make_terminal, notice, render, terminal_for, tildify};
use baton_core::theme::pick_glyphs;

/// A terminal of `depth` colours, `columns` wide, in an empty environment.
fn tty(depth: u8, columns: usize) -> Terminal {
    let sized = terminal_for(true, Some(columns), &Env::new());
    make_terminal(true, depth, pick_glyphs(&Env::new()), sized.cols)
}

/// A stream that is not a terminal, with this environment.
fn pipe(env: &[(&str, &str)]) -> Terminal {
    terminal_for(false, None, &Env::from_pairs(env.iter().copied()))
}

/// Text without its colour escapes.
fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("\x1b[") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        rest = after.split_once('m').map_or("", |(_, tail)| tail);
    }
    out.push_str(rest);
    out
}

#[test]
fn a_pipe_gets_plain_text_and_a_terminal_gets_the_menus_palette_at_its_depth() {
    let line: Vec<Line> = vec![vec![s("Ready", "ok bold"), s(" now", "muted")]];
    let plain = pipe(&[]);
    assert!(!plain.styled);
    assert_eq!(render(&line, &plain), "Ready now");

    // ok is #92ae99, bold first; muted is #7e858e.
    assert_eq!(
        render(&line, &tty(24, 100)),
        "\x1b[1;38;2;146;174;153mReady\x1b[0m\x1b[38;2;126;133;142m now\x1b[0m"
    );
    let eight = render(&line, &tty(8, 100));
    assert!(eight.starts_with("\x1b[1;38;5;"), "{eight:?}");
    let four = render(&line, &tty(4, 100));
    assert!(
        four.starts_with("\x1b[1;3") || four.starts_with("\x1b[1;9"),
        "{four:?}"
    );
    // NO_COLOR (depth 1) and a dumb terminal: no escapes at all.
    assert_eq!(render(&line, &tty(1, 100)), "Ready now");
    let no_color = terminal_for(true, Some(100), &Env::from_pairs([("NO_COLOR", "1")]));
    assert_eq!(render(&line, &no_color), "Ready now");
    let dumb = terminal_for(true, Some(100), &Env::from_pairs([("TERM", "dumb")]));
    assert_eq!(render(&line, &dumb), "Ready now");
}

#[test]
fn force_color_styles_a_pipe_and_no_color_does_not() {
    assert!(pipe(&[("FORCE_COLOR", "1")]).styled);
    assert!(!pipe(&[("FORCE_COLOR", "1"), ("NO_COLOR", "1")]).styled);
    assert!(!pipe(&[("FORCE_COLOR", "0")]).styled);
}

#[test]
fn neighbouring_spans_of_one_style_share_one_escape_and_plain_lines_lose_trailing_blanks() {
    let out = render(
        &[vec![s("a ", "bold"), s("b", "bold"), s(" c", "muted")]],
        &tty(24, 100),
    );
    assert_eq!(out.matches("\x1b[1m").count(), 1);
    assert_eq!(strip(&out), "a b c");
    assert_eq!(render(&[vec![p("text   ")]], &pipe(&[])), "text");
}

#[test]
fn the_layout_is_as_wide_as_the_terminal_within_reason() {
    assert_eq!(tty(24, 200).cols, 110);
    assert_eq!(tty(24, 40).cols, 60);
    assert_eq!(pipe(&[]).cols, 100);
}

#[test]
fn notices_from_a_baton_session_have_the_mark_in_front_and_the_key_hints_after_the_first_dot() {
    let line = "baton: acct1 reached its 5-hour limit; moving this conversation to acct2 in 8 s · Enter: now · any other key: stay on acct1 · Ctrl+C: quit";
    assert_eq!(notice(line, &pipe(&[])), line, "not a terminal: unchanged");
    let term = tty(24, 100);
    let styled = notice(line, &term);
    assert!(styled.contains("\x1b["));
    assert_eq!(
        strip(&styled),
        line.replace("baton: ", &format!("{} ", term.g.baton)),
        "the same words, the mark in place of the name"
    );
    assert_eq!(notice("something else", &term), "something else");
}

#[test]
fn errors_are_baton_message_when_plain_and_a_cross_when_styled() {
    assert_eq!(
        error_text("No account named x.", &pipe(&[])),
        "baton: No account named x."
    );
    let term = tty(24, 100);
    assert_eq!(
        strip(&error_text("No account named x.", &term)),
        format!("{} No account named x.", term.g.cross)
    );
}

#[test]
fn paths_show_the_home_folder_as_a_tilde() {
    assert_eq!(
        tildify("C:\\Users\\a\\.claude\\x.json", "C:\\Users\\a"),
        "~/.claude/x.json"
    );
    assert_eq!(tildify("C:\\Users\\a", "C:\\Users\\a"), "~");
    assert_eq!(tildify("D:\\other\\x", "C:\\Users\\a"), "D:/other/x");
    assert_eq!(tildify("C:\\Users\\ab\\x", "C:\\Users\\a"), "C:/Users/ab/x");
}
