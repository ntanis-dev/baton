//! What reaches the menu from the terminal, as actions.
//!
//! A terminal sends keys, pastes, and sometimes its own replies: focus in and out, cursor position
//! reports, device attributes and status, mode and window reports, mouse reports, and string replies
//! (OSC, DCS, APC, PM, SOS) ended by BEL or ST. None of the replies is a key: [`ReplyFilter`] takes them
//! out (the mouse reports become mouse events), and what is left is split into keys and text.
//!
//! This module works on the bytes a terminal sends; `terminal` maps the events of a console that is
//! read through crossterm onto the same actions.

use std::sync::LazyLock;
use std::time::Duration;

use regex::{Captures, Regex};

use crate::keys::Key;
use crate::state::{Action, MouseEvent, MouseKind, WHEEL_STEP};

/// How long a lone escape (or an unfinished escape sequence) is held for the rest of it.
pub const HOLD_ESCAPE_MS: u64 = 25;
/// How long an unfinished string reply is held for its end.
pub const HOLD_STRING_MS: u64 = 150;

/// The start of a bracketed paste.
const PASTE_START: &str = "\x1b[200~";
/// The end of a bracketed paste.
const PASTE_END: &str = "\x1b[201~";

/// A regular expression that is known to compile; None only if this file is edited wrongly.
fn pattern(source: &str) -> Option<Regex> {
    Regex::new(source).ok()
}

/// The replies that are CSI sequences.
static CSI_REPLY: LazyLock<Option<Regex>> = LazyLock::new(|| {
    pattern(
        &[
            r"\x1b\[[IO]",
            r"\x1b\[[0-9]+;[0-9]+R",
            r"\x1b\[[?>=][0-9;]*c",
            r"\x1b\[[0-9]*n",
            r"\x1b\[\??[0-9;]+\$y",
            r"\x1b\[\?[0-9]+u",
            r"\x1b\[[0-9]+(?:;[0-9]+)*t",
            r"\x1b\[<[0-9]+;[0-9]+;[0-9]+[Mm]",
        ]
        .join("|"),
    )
});
/// A whole string reply: its introducer, its text and its end.
static STRING_REPLY: LazyLock<Option<Regex>> =
    LazyLock::new(|| pattern(r"(?s)\x1b[\]P_^X].*?(?:\x07|\x1b\\)"));
/// The introducer of a string reply.
static STRING_START: LazyLock<Option<Regex>> = LazyLock::new(|| pattern(r"\x1b[\]P_^X]"));
/// An escape, or an unfinished CSI sequence, at the end of the text.
static PARTIAL_TAIL: LazyLock<Option<Regex>> =
    LazyLock::new(|| pattern(r"\x1b(?:\[[\x30-\x3f]*[\x20-\x2f]*)?$"));
/// The paste marks and the SGR mouse reports.
static MARKS: LazyLock<Option<Regex>> =
    LazyLock::new(|| pattern(r"\x1b\[(200~|201~|<([0-9]+);([0-9]+);([0-9]+)([Mm]))"));

/// Takes the terminal's replies out of what it sends.
#[derive(Clone, Debug, Default)]
pub struct ReplyFilter {
    /// The end of the last chunk that may be the start of a reply.
    held: String,
    /// Whether a bracketed paste is going on: a mouse report in it is text someone pasted.
    pasting: bool,
}

impl ReplyFilter {
    /// A filter that holds nothing.
    pub fn new() -> ReplyFilter {
        ReplyFilter::default()
    }

    /// Takes the next chunk and gives back what of it (and of what was held) is keys and pastes. Each
    /// mouse report outside a paste goes to `on_mouse`.
    pub fn push(&mut self, chunk: &str, on_mouse: &mut dyn FnMut(MouseEvent)) -> String {
        let mut text = std::mem::take(&mut self.held);
        text.push_str(chunk);
        if let Some(strings) = STRING_REPLY.as_ref() {
            text = strings.replace_all(&text, "").into_owned();
        }
        if let Some(marks) = MARKS.as_ref() {
            let pasting = &mut self.pasting;
            text = marks
                .replace_all(&text, |found: &Captures<'_>| {
                    let report = found.get(1).map_or("", |m| m.as_str());
                    if report == "200~" || report == "201~" {
                        *pasting = report == "200~";
                        return found.get(0).map_or("", |m| m.as_str()).to_owned();
                    }
                    if !*pasting {
                        let number = |i: usize| {
                            found
                                .get(i)
                                .and_then(|m| m.as_str().parse::<i64>().ok())
                                .unwrap_or(0)
                        };
                        let bits = number(2);
                        let released = found.get(5).is_some_and(|m| m.as_str() == "m");
                        on_mouse(MouseEvent {
                            x: (number(3) - 1) as i32,
                            y: (number(4) - 1) as i32,
                            button: (bits & 3) as u8,
                            kind: if bits & 64 != 0 {
                                MouseKind::Wheel
                            } else if released {
                                MouseKind::Release
                            } else if bits & 32 != 0 {
                                MouseKind::Move
                            } else {
                                MouseKind::Press
                            },
                            delta: if bits & 64 == 0 {
                                0
                            } else if bits & 1 != 0 {
                                WHEEL_STEP
                            } else {
                                -WHEEL_STEP
                            },
                        });
                    }
                    String::new()
                })
                .into_owned();
        }
        if let Some(replies) = CSI_REPLY.as_ref() {
            text = replies.replace_all(&text, "").into_owned();
        }
        let start = STRING_START
            .as_ref()
            .and_then(|start| start.find(&text))
            .map(|m| m.start());
        let tail = PARTIAL_TAIL
            .as_ref()
            .and_then(|tail| tail.find(&text))
            .map(|m| m.start());
        if let Some(at) = start.or(tail) {
            self.held = text.split_off(at);
        }
        text
    }

    /// Gives up waiting: what was held is keys after all (the introducer of a string reply aside).
    pub fn flush(&mut self) -> String {
        let held = std::mem::take(&mut self.held);
        if self.holds_string(&held) {
            held.chars().skip(2).collect()
        } else {
            held
        }
    }

    /// Whether something is held.
    pub fn pending(&self) -> bool {
        !self.held.is_empty()
    }

    /// How long what is held is waited for.
    pub fn wait(&self) -> Duration {
        Duration::from_millis(if self.holds_string(&self.held) {
            HOLD_STRING_MS
        } else {
            HOLD_ESCAPE_MS
        })
    }

    /// Whether held text starts a string reply.
    fn holds_string(&self, held: &str) -> bool {
        let head: String = held.chars().take(2).collect();
        STRING_START.as_ref().is_some_and(|start| start.is_match(&head))
    }
}

/// A key the terminal sent, with what was held down with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Held {
    /// Shift.
    shift: bool,
    /// Ctrl.
    ctrl: bool,
}

/// The keys that move: the arrows, the page keys, Home and End.
fn moving(name: &str, held: Held) -> Option<Key> {
    let plain = Key::from_name(name);
    Some(if held.ctrl {
        Key::Named(format!("ctrl+{name}"))
    } else if held.shift {
        Key::Named(format!("shift+{name}"))
    } else {
        plain
    })
}

/// What was held, from the modifier parameter of a sequence (1 is nothing; then shift is 1, alt 2,
/// ctrl 4 over it).
fn held_of(modifier: u32) -> Held {
    let bits = modifier.saturating_sub(1);
    Held {
        shift: bits & 1 != 0,
        ctrl: bits & 4 != 0,
    }
}

/// The key of one character that came alone: Enter, Tab, Backspace, Escape, Ctrl with a letter, or
/// the character typed. `meta`: it came after an escape (Alt was held), which makes a plain character
/// no key at all.
fn single(ch: char, meta: bool) -> Option<Key> {
    Some(match ch {
        '\r' => Key::Enter,
        '\n' => return None,
        '\t' => Key::Tab,
        '\u{8}' | '\u{7f}' => Key::Backspace,
        '\u{1b}' => Key::Escape,
        '\0' => Key::Ctrl(' '),
        '\u{1}'..='\u{1a}' => Key::Ctrl(char::from(ch as u8 + 96)),
        '\u{1c}'..='\u{1f}' => Key::Ctrl(char::from(ch as u8 + 64)),
        '\u{80}'..='\u{9f}' => return None,
        _ if meta => return None,
        _ => Key::Char(ch),
    })
}

/// The key of a control sequence: `body` is what follows `ESC [` (or `ESC O` when `ss3`) up to and
/// with its final character. None for a sequence that is no key the menu knows.
fn sequence_key(body: &str, ss3: bool) -> Option<Key> {
    // Legacy function keys start with a second bracket (ESC [ [ A): nothing the menu uses but the
    // page keys of PuTTY.
    let body = body.strip_prefix('[').unwrap_or(body);
    let last = body.chars().next_back()?;
    let params: Vec<u32> = body[..body.len() - last.len_utf8()]
        .split(';')
        .map(|part| part.split(':').next().unwrap_or("").parse::<u32>().unwrap_or(0))
        .collect();
    let first = params.first().copied().unwrap_or(0);
    match last {
        '~' | '^' | '$' => {
            let mut held = held_of(params.get(1).copied().unwrap_or(1));
            held.ctrl |= last == '^';
            held.shift |= last == '$';
            match first {
                1 | 7 => moving("home", held),
                4 | 8 => moving("end", held),
                5 => moving("pageup", held),
                6 => moving("pagedown", held),
                3 => Some(Key::Delete),
                _ => None,
            }
        }
        'u' => None,
        _ => {
            // A letter: `ESC [ A`, or with what was held `ESC [ 1 ; 5 A` (also `ESC [ 5 A`).
            let modifier = if params.len() > 1 { params[1] } else { first.max(1) };
            let mut held = held_of(modifier);
            let name = match last {
                'A' => "up",
                'B' => "down",
                'C' => "right",
                'D' => "left",
                'F' => "end",
                'H' => "home",
                'a' | 'b' | 'c' | 'd' => {
                    // rxvt: lower case letters are the arrows with Shift (or, after ESC O, with Ctrl).
                    if ss3 {
                        held.ctrl = true;
                    } else {
                        held.shift = true;
                    }
                    ["up", "down", "right", "left"][(last as u8 - b'a') as usize]
                }
                'Z' => return Some(Key::Named("shift+tab".to_owned())),
                'M' if ss3 => return Some(Key::Enter),
                _ => return None,
            };
            moving(name, held)
        }
    }
}

/// What follows an escape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sequence {
    /// A whole control sequence, ending before this index.
    Whole(usize),
    /// The start of a control sequence whose end has not arrived.
    Partial,
    /// No control sequence.
    No,
}

/// The control sequence that starts at `at` (the escape). `prefix` is the number of escapes it
/// starts with.
fn sequence_end(text: &str, at: usize, prefix: usize) -> Sequence {
    let bytes = text.as_bytes();
    let Some(kind) = bytes.get(at + prefix).copied() else {
        return Sequence::No;
    };
    let start = at + prefix + 1;
    if kind == b'[' {
        let mut index = start;
        while let Some(byte) = bytes.get(index).copied() {
            if byte == b'[' && index == start {
                index += 1;
                continue;
            }
            // Shifted editing keys of rxvt end in `$`, which is otherwise an intermediate byte.
            let rxvt = byte == b'$' && index == start + 1 && b"235678".contains(&bytes[start]);
            if rxvt || (0x40..=0x7e).contains(&byte) {
                return Sequence::Whole(index + 1);
            }
            if !(0x20..=0x3f).contains(&byte) {
                return Sequence::No;
            }
            index += 1;
        }
        return Sequence::Partial;
    }
    if kind == b'O' {
        let mut index = start;
        while let Some(byte) = bytes.get(index).copied() {
            if (0x40..=0x7e).contains(&byte) {
                return Sequence::Whole(index + 1);
            }
            if byte != b';' && !byte.is_ascii_digit() {
                return Sequence::No;
            }
            index += 1;
        }
        return Sequence::Partial;
    }
    Sequence::No
}

/// A run of text without escapes as actions: Backspace and Ctrl+C are keys of their own wherever they
/// are (a held Backspace arrives as several in one chunk), one other character is a key, more are
/// text that arrived at once.
fn plain_text(text: &str, out: &mut Vec<Action>) {
    let mut run = String::new();
    let end_run = |run: &mut String, out: &mut Vec<Action>| {
        let mut chars = run.chars();
        match (chars.next(), chars.next()) {
            (None, _) => {}
            (Some(only), None) => out.extend(single(only, false).map(Action::Key)),
            _ => out.push(Action::Text(run.clone())),
        }
        run.clear();
    };
    for ch in text.chars() {
        if matches!(ch, '\u{7f}' | '\u{8}' | '\u{3}') {
            end_run(&mut run, out);
            out.extend(single(ch, false).map(Action::Key));
        } else {
            run.push(ch);
        }
    }
    end_run(&mut run, out);
}

/// Splits what the filter let through into keys, text and pastes.
#[derive(Clone, Debug, Default)]
pub struct KeyParser {
    /// A paste whose end has not arrived yet, from its start mark.
    pending: String,
}

impl KeyParser {
    /// A parser that holds nothing.
    pub fn new() -> KeyParser {
        KeyParser::default()
    }

    /// Whether something is held: a paste waiting for its end, or the start of a sequence.
    pub fn pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Whether the start of a sequence is held (and not a paste, which waits as long as it takes).
    fn holds_escape(&self) -> bool {
        self.pending() && !self.pending.starts_with(PASTE_START)
    }

    /// The actions in the next piece of text. `last`: nothing more follows for now (the filter gave up
    /// waiting), so an escape at the end is the Escape key and not the start of a sequence.
    pub fn push(&mut self, chunk: &str, last: bool) -> Vec<Action> {
        let mut text = std::mem::take(&mut self.pending);
        text.push_str(chunk);
        let mut out = Vec::new();
        let mut index = 0;
        while index < text.len() {
            let Some(found) = text[index..].find('\u{1b}') else {
                plain_text(&text[index..], &mut out);
                break;
            };
            let at = index + found;
            plain_text(&text[index..at], &mut out);
            let rest = &text[at..];
            if rest.starts_with(PASTE_START) {
                let body = at + PASTE_START.len();
                let Some(end) = text[body..].find(PASTE_END) else {
                    self.pending = text[at..].to_owned();
                    return out;
                };
                out.push(Action::Paste(text[body..body + end].to_owned()));
                index = body + end + PASTE_END.len();
                continue;
            }
            // One escape, or two (Alt with a key that sends a sequence).
            let prefix = if rest[1..].starts_with('\u{1b}') { 2 } else { 1 };
            match sequence_end(&text, at, prefix) {
                Sequence::Whole(end) => {
                    let ss3 = text.as_bytes()[at + prefix] == b'O';
                    out.extend(sequence_key(&text[at + prefix + 1..end], ss3).map(Action::Key));
                    index = end;
                    continue;
                }
                Sequence::Partial if !last => {
                    self.pending = rest.to_owned();
                    return out;
                }
                _ => {}
            }
            let mut after = rest[prefix..].chars();
            match after.next() {
                // Escape alone (twice with Alt): the Escape key.
                None => {
                    if last || prefix == 2 {
                        out.push(Action::Key(Key::Escape));
                    } else {
                        self.pending = rest.to_owned();
                    }
                    break;
                }
                Some('[' | 'O') => {
                    // A sequence that never ended, or one that is none: no key. What follows its
                    // introducer is read on its own.
                    index = at + prefix + 1;
                }
                Some(ch) if prefix == 2 => {
                    // Two escapes and something that starts no sequence: Escape, then the rest.
                    out.push(Action::Key(Key::Escape));
                    index = at + 2;
                    let _ = ch;
                }
                Some(ch) => {
                    out.extend(single(ch, true).map(Action::Key));
                    index = at + 1 + ch.len_utf8();
                }
            }
        }
        out
    }
}

/// The bytes a terminal sends as actions: decoded, the replies taken out, split into keys, text,
/// pastes and mouse events.
#[derive(Clone, Debug, Default)]
pub struct Decoder {
    /// The bytes of a character that has not arrived whole.
    partial: Vec<u8>,
    /// Takes the replies out.
    filter: ReplyFilter,
    /// Splits the rest.
    keys: KeyParser,
}

impl Decoder {
    /// A decoder that holds nothing.
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// The actions in the next bytes: the mouse events first, then the keys, text and pastes.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Action> {
        self.partial.extend_from_slice(bytes);
        let whole = match std::str::from_utf8(&self.partial) {
            Ok(text) => text.len(),
            // A character cut in two waits for its other half; bytes that are no text are passed on
            // as the replacement character.
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.partial.len(),
        };
        let text = String::from_utf8_lossy(&self.partial[..whole]).into_owned();
        self.partial.drain(..whole);
        let mut out = Vec::new();
        let keys = self
            .filter
            .push(&text, &mut |event| out.push(Action::Mouse(event)));
        out.extend(self.keys.push(&keys, false));
        out
    }

    /// How long to wait for more before [`Decoder::flush`]; None when nothing is held.
    pub fn wait(&self) -> Option<Duration> {
        if self.filter.pending() {
            return Some(self.filter.wait());
        }
        self.keys
            .holds_escape()
            .then(|| Duration::from_millis(HOLD_ESCAPE_MS))
    }

    /// Gives up waiting: what was held is keys after all.
    pub fn flush(&mut self) -> Vec<Action> {
        let rest = self.filter.flush();
        self.keys.push(&rest, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(names: &[&str]) -> Vec<Action> {
        names
            .iter()
            .map(|name| Action::Key(Key::from_name(name)))
            .collect()
    }

    fn decode(bytes: &str) -> Vec<Action> {
        let mut decoder = Decoder::new();
        let mut out = decoder.push(bytes.as_bytes());
        out.extend(decoder.flush());
        out
    }

    #[test]
    fn sgr_reports_survive_split_chunks_and_pasted_reports_never_click() {
        let mut events = Vec::new();
        let mut filter = ReplyFilter::new();
        assert_eq!(filter.push("\x1b[<0;", &mut |event| events.push(event)), "");
        assert!(filter.pending());
        assert_eq!(filter.wait(), Duration::from_millis(HOLD_ESCAPE_MS));
        assert_eq!(filter.push("12;9M", &mut |event| events.push(event)), "");
        assert_eq!(
            events[0],
            MouseEvent {
                x: 11,
                y: 8,
                button: 0,
                kind: MouseKind::Press,
                delta: 0
            }
        );
        filter.push("\x1b[<65;20;12M", &mut |event| events.push(event));
        assert_eq!((events[1].kind, events[1].delta), (MouseKind::Wheel, 1));
        assert_eq!(
            filter.push("\x1b[200~text\x1b[<0;2;3M\x1b[201~", &mut |event| events
                .push(event)),
            "\x1b[200~text\x1b[201~"
        );
        assert_eq!(events.len(), 2);
        assert_eq!(
            filter.push("\x1b[?1;2c\x1b[3;4Rr", &mut |event| events.push(event)),
            "r"
        );
        assert!(!filter.pending());
    }

    #[test]
    fn the_kinds_of_mouse_report() {
        let mut events = Vec::new();
        let mut filter = ReplyFilter::new();
        let all = "\x1b[<0;5;5m\x1b[<32;6;5M\x1b[<64;1;1M\x1b[<2;3;4M";
        assert_eq!(filter.push(all, &mut |event| events.push(event)), "");
        let seen: Vec<(MouseKind, u8, i32)> = events.iter().map(|e| (e.kind, e.button, e.delta)).collect();
        assert_eq!(
            seen,
            [
                (MouseKind::Release, 0, 0),
                (MouseKind::Move, 0, 0),
                (MouseKind::Wheel, 0, -1),
                (MouseKind::Press, 2, 0)
            ]
        );
    }

    #[test]
    fn string_replies_are_held_for_their_end_and_then_let_go() {
        let mut none = |_: MouseEvent| {};
        let mut filter = ReplyFilter::new();
        assert_eq!(filter.push("a\x1b]11;rgb:00/00", &mut none), "a");
        assert_eq!(filter.wait(), Duration::from_millis(HOLD_STRING_MS));
        assert_eq!(filter.push("/00\x07b", &mut none), "b");
        assert!(!filter.pending());
        // One that never ends is keys after all, its introducer aside.
        assert_eq!(filter.push("\x1b]xyz", &mut none), "");
        assert_eq!(filter.flush(), "xyz");
        // An escape alone is held a moment, then it is the Escape key.
        assert_eq!(filter.push("q\x1b", &mut none), "q");
        assert_eq!(filter.flush(), "\x1b");
        assert_eq!(
            filter.push(
                "\x1bP1$r0m\x1b\\\x1b[I\x1b[O\x1b[0n\x1b[?2026;2$y\x1b[?1u\x1b[8;30;120t",
                &mut none
            ),
            ""
        );
    }

    #[test]
    fn keys_as_the_terminal_sends_them() {
        assert_eq!(
            decode("\x1b[A\x1b[B\x1b[C\x1b[D"),
            keys(&["up", "down", "right", "left"])
        );
        assert_eq!(decode("\x1bOA\x1bOF\x1bOH"), keys(&["up", "end", "home"]));
        assert_eq!(
            decode("\x1b[H\x1b[F\x1b[1~\x1b[4~\x1b[7~\x1b[8~"),
            keys(&["home", "end", "home", "end", "home", "end"])
        );
        assert_eq!(
            decode("\x1b[5~\x1b[6~\x1b[3~"),
            keys(&["pageup", "pagedown", "delete"])
        );
        assert_eq!(decode("\x1b[1;2A"), keys(&["shift+up"]));
        assert_eq!(decode("\x1b[1;5C"), keys(&["ctrl+right"]));
        assert_eq!(
            decode("\x1b[1;3D"),
            keys(&["left"]),
            "Alt does not change an arrow"
        );
        assert_eq!(decode("\x1b[5;5~"), keys(&["ctrl+pageup"]));
        assert_eq!(decode("\x1b[Z"), keys(&["shift+tab"]));
        assert_eq!(decode("\x1b[a\x1bOb"), keys(&["shift+up", "ctrl+down"]));
        assert_eq!(decode("\r"), keys(&["enter"]));
        assert_eq!(decode("\x1bOM"), keys(&["enter"]));
        assert_eq!(decode("\t"), keys(&["tab"]));
        assert_eq!(decode("\x7f"), keys(&["backspace"]));
        assert_eq!(decode("\x08"), keys(&["backspace"]));
        assert_eq!(decode("\x1b"), keys(&["escape"]));
        assert_eq!(decode("\x1b\x1b"), keys(&["escape"]));
        assert_eq!(decode("\x03"), keys(&["ctrl+c"]));
        assert_eq!(decode("\x15"), keys(&["ctrl+u"]));
        assert_eq!(decode("\x0b"), keys(&["ctrl+k"]));
        assert_eq!(decode("\0"), keys(&["ctrl+space"]));
        assert_eq!(decode("J"), keys(&["J"]));
        assert_eq!(decode(" "), keys(&[" "]));
        assert_eq!(decode("/"), keys(&["/"]));
        assert_eq!(decode("é"), vec![Action::Key(Key::Char('é'))]);
        // Nothing the menu uses: a line feed alone, Alt with a key, function keys, Insert, replies it
        // does not know.
        assert_eq!(decode("\n"), vec![]);
        assert_eq!(decode("\x1ba"), vec![]);
        assert_eq!(decode("\x1bOP\x1b[15~\x1b[2~\x1b[97;5u\x1b[E"), vec![]);
        // Alt with Enter or Backspace is still that key.
        assert_eq!(decode("\x1b\r\x1b\x7f"), keys(&["enter", "backspace"]));
    }

    #[test]
    fn text_that_arrives_at_once_and_pastes() {
        assert_eq!(decode("acct5"), vec![Action::Text("acct5".to_owned())]);
        // A held Backspace, and Ctrl+C, are keys wherever they are.
        assert_eq!(
            decode("ab\x7f\x7f\x03c"),
            vec![
                Action::Text("ab".to_owned()),
                Action::Key(Key::Backspace),
                Action::Key(Key::Backspace),
                Action::Key(Key::Ctrl('c')),
                Action::Key(Key::Char('c')),
            ]
        );
        assert_eq!(
            decode("x\x1b[200~two\nlines\x1b[201~\r"),
            vec![
                Action::Key(Key::Char('x')),
                Action::Paste("two\nlines".to_owned()),
                Action::Key(Key::Enter),
            ]
        );
        // A paste in pieces waits for its end; a mouse report in it is pasted text.
        let mut decoder = Decoder::new();
        assert_eq!(decoder.push(b"\x1b[200~one "), vec![]);
        assert_eq!(decoder.wait(), None);
        assert_eq!(
            decoder.push(b"\x1b[<0;2;3Mtwo\x1b[201~q"),
            vec![Action::Paste("one two".to_owned()), Action::Key(Key::Char('q'))]
        );
    }

    #[test]
    fn mouse_reports_come_first_and_characters_cut_in_two_wait() {
        let mut decoder = Decoder::new();
        assert_eq!(
            decoder.push(b"q\x1b[<0;8;8M"),
            vec![
                Action::Mouse(MouseEvent::press(7, 7)),
                Action::Key(Key::Char('q'))
            ]
        );
        let bytes = "é".as_bytes();
        assert_eq!(decoder.push(&bytes[..1]), vec![]);
        assert_eq!(decoder.push(&bytes[1..]), vec![Action::Key(Key::Char('é'))]);
        // An escape waits a moment for the rest of a sequence.
        assert_eq!(decoder.push(b"\x1b"), vec![]);
        assert_eq!(decoder.wait(), Some(Duration::from_millis(HOLD_ESCAPE_MS)));
        assert_eq!(decoder.push(b"[B"), keys(&["down"]));
        assert_eq!(decoder.push(b"\x1b"), vec![]);
        assert_eq!(decoder.flush(), keys(&["escape"]));
        assert_eq!(decoder.wait(), None);
    }
}
