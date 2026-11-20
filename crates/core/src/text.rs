//! Text for the terminal: cleaned of control sequences, measured in terminal cells (wide CJK and emoji
//! take two, combining marks none) and wrapped to fit. Nothing is ever cut with an ellipsis: what does
//! not fit wraps, or a view leaves a less important part out.

use std::sync::LazyLock;

use regex::Regex;

use crate::clock::{DAY, Ms, local, same_local_day, weekday_of};

/// Terminal escape sequences: CSI, OSC and the two-character ones.
static ESCAPES: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\)?|[@-Z\\-_])").ok()
});

/// Characters that take no cell: combining marks and format characters.
static ZERO_WIDTH: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"^[\p{Mn}\p{Me}\p{Cf}]$").ok());

/// Whether a character is a control character that is never shown.
fn is_control(c: char) -> bool {
    matches!(c, '\u{00}'..='\u{08}' | '\u{0B}'..='\u{1F}' | '\u{7F}'..='\u{9F}')
}

/// Text made safe to show: line ends as `\n`, escape sequences and control characters removed, a tab
/// as two spaces.
pub fn clean(text: &str) -> String {
    let unix = text.replace("\r\n", "\n").replace('\r', "\n");
    let bare = match ESCAPES.as_ref() {
        Some(escapes) => escapes.replace_all(&unix, "").into_owned(),
        None => unix,
    };
    bare.replace('\t', "  ")
        .chars()
        .filter(|c| !is_control(*c))
        .collect()
}

/// Whether a code point is drawn two cells wide.
fn wide(cp: u32) -> bool {
    (0x1100..=0x115f).contains(&cp)
        || ((0x2e80..=0xa4cf).contains(&cp) && cp != 0x303f)
        || (0xac00..=0xd7a3).contains(&cp)
        || (0xf900..=0xfaff).contains(&cp)
        || (0xfe30..=0xfe4f).contains(&cp)
        || (0xff00..=0xff60).contains(&cp)
        || (0xffe0..=0xffe6).contains(&cp)
        || (0x1f300..=0x1f64f).contains(&cp)
        || (0x1f680..=0x1f6ff).contains(&cp)
        || (0x1f900..=0x1f9ff).contains(&cp)
        || (0x1fa70..=0x1faff).contains(&cp)
        || (0x20000..=0x3fffd).contains(&cp)
}

/// The cells a character takes: 0, 1 or 2.
pub fn char_width(ch: char) -> usize {
    let cp = ch as u32;
    if cp < 0x20 || (0x7f..0xa0).contains(&cp) {
        return 0;
    }
    // Nothing below the soft hyphen is a mark or a format character.
    if cp < 0xad {
        return 1;
    }
    let mut buffer = [0u8; 4];
    if ZERO_WIDTH
        .as_ref()
        .is_some_and(|zero| zero.is_match(ch.encode_utf8(&mut buffer)))
    {
        return 0;
    }
    if wide(cp) { 2 } else { 1 }
}

/// The cells a text takes.
pub fn width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// Word wrap to `cells`, keeping blank lines; a word longer than a line is broken. `indent` prefixes
/// continuation lines.
pub fn wrap(text: &str, cells: usize, indent: &str) -> Vec<String> {
    let mut out = Vec::new();
    let room = cells.max(1);
    let indent_width = width(indent);
    for paragraph in clean(text).split('\n') {
        if paragraph.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        let mut used = 0usize;
        let mut first = true;
        let limit = |first: bool| room.saturating_sub(if first { 0 } else { indent_width }).max(1);
        let flush = |out: &mut Vec<String>, line: &mut String, used: &mut usize, first: &mut bool| {
            out.push(format!("{}{line}", if *first { "" } else { indent }));
            *first = false;
            line.clear();
            *used = 0;
        };
        for word in paragraph.trim().split(' ').filter(|word| !word.is_empty()) {
            let w = width(word);
            if used > 0 && used + 1 + w > limit(first) {
                flush(&mut out, &mut line, &mut used, &mut first);
            }
            if w > limit(first) {
                for ch in word.chars() {
                    let cw = char_width(ch);
                    if used + cw > limit(first) {
                        flush(&mut out, &mut line, &mut used, &mut first);
                    }
                    line.push(ch);
                    used += cw;
                }
                continue;
            }
            if used > 0 {
                line.push(' ');
                used += 1;
            }
            line.push_str(word);
            used += w;
        }
        if !line.is_empty() {
            flush(&mut out, &mut line, &mut used, &mut first);
        }
    }
    out
}

/// A number rounded the way the screen always has: halves go up.
pub fn round(n: f64) -> f64 {
    (n + 0.5).floor()
}

/// A time span: 45s, 42m, 2h 5m, 3d 4h. Every component carries its unit.
pub fn span(ms: Option<Ms>) -> String {
    let Some(ms) = ms else {
        return "?".to_owned();
    };
    let s = (round(ms as f64 / 1000.0) as i64).max(0);
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m");
    }
    let h = m / 60;
    if h < 24 {
        return if m % 60 != 0 {
            format!("{h}h {}m", m % 60)
        } else {
            format!("{h}h")
        };
    }
    let d = h / 24;
    if h % 24 != 0 {
        format!("{d}d {}h", h % 24)
    } else {
        format!("{d}d")
    }
}

/// [`span`] of a span that may not be a number at all.
pub fn span_of(ms: f64) -> String {
    span(ms.is_finite().then_some(ms as Ms))
}

/// The short names of the days, Sunday first.
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// The short names of the months.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Local wall-clock time: 15:20.
pub fn hhmm(ms: Ms) -> String {
    let at = local(ms);
    format!("{:02}:{:02}", at.hour, at.minute)
}

/// Local wall-clock time: 15:20, or "Sat 09:00" when not today, or "Oct 30" when more than a week
/// away.
pub fn when(ms: Option<Ms>, now: Ms) -> String {
    let Some(ms) = ms else {
        return "?".to_owned();
    };
    if same_local_day(ms, now) {
        return hhmm(ms);
    }
    let at = local(ms);
    if ((ms - now).abs() as f64) < 6.5 * DAY as f64 {
        return format!("{} {}", DAYS[at.weekday as usize % 7], hhmm(ms));
    }
    format!("{} {}", MONTHS[(at.month as usize + 11) % 12], at.day)
}

/// The day of the week of a date written `2026-11-15`: `Sun`.
pub fn day_label(date: &str) -> &'static str {
    let mut parts = date.split('-').map(|part| part.parse::<i64>().ok());
    let (Some(Some(y)), Some(Some(m)), Some(Some(d))) = (parts.next(), parts.next(), parts.next()) else {
        return "?";
    };
    match weekday_of(y as i32, m as u32, d as u32) {
        Some(day) => DAYS[day as usize % 7],
        None => "?",
    }
}

/// A moment's local date: `Oct 30`.
pub fn short_date(ms: Ms) -> String {
    let at = local(ms);
    format!("{} {}", MONTHS[(at.month as usize + 11) % 12], at.day)
}

/// A percentage, whole: `42%`, or `?`.
pub fn pct(n: Option<f64>) -> String {
    match n {
        Some(n) => format!("{}%", round(n) as i64),
        None => "?".to_owned(),
    }
}

/// A count with its noun: `1 account`, `2 accounts`.
pub fn plural(n: usize, word: &str) -> String {
    plural_as(n, word, &format!("{word}s"))
}

/// A count with its noun and the noun's own plural: `2 entries`.
pub fn plural_as(n: usize, word: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { word } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{HOUR, MINUTE, local_ms};

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(clean("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(clean("\x1b]0;title\x07x"), "x");
        assert_eq!(clean("a\tb"), "a  b");
        assert_eq!(clean("a\x07b\u{9b}c"), "abc");
        assert_eq!(clean("\x1bMx"), "x");
    }

    #[test]
    fn cells() {
        assert_eq!(width("abc"), 3);
        assert_eq!(width("日本"), 4);
        assert_eq!(width("e\u{0301}"), 1);
        assert_eq!(width("a\u{200d}b"), 2);
        assert_eq!(width("\u{1F600}"), 2);
        assert_eq!(width("╺━╸"), 3);
        assert_eq!(char_width('\u{00AD}'), 0);
        assert_eq!(char_width('\n'), 0);
    }

    #[test]
    fn wrapping_words() {
        assert_eq!(wrap("one two three four", 9, ""), ["one two", "three", "four"]);
        assert_eq!(
            wrap("one two three four", 9, "  "),
            ["one two", "  three", "  four"]
        );
        assert_eq!(wrap("abcdefghij", 4, ""), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("a\n\nb", 10, ""), ["a", "", "b"]);
        assert_eq!(wrap("  spaced   out  ", 20, ""), ["spaced out"]);
        assert_eq!(wrap("x", 0, ""), ["x"]);
    }

    #[test]
    fn spans_of_time() {
        assert_eq!(span(None), "?");
        assert_eq!(span(Some(-10)), "0s");
        assert_eq!(span(Some(45_000)), "45s");
        assert_eq!(span(Some(42 * MINUTE + 59_000)), "42m");
        assert_eq!(span(Some(2 * HOUR + 5 * MINUTE)), "2h 5m");
        assert_eq!(span(Some(2 * HOUR)), "2h");
        assert_eq!(span(Some(3 * DAY + 4 * HOUR)), "3d 4h");
        assert_eq!(span(Some(3 * DAY)), "3d");
        assert_eq!(span_of(f64::INFINITY), "?");
        assert_eq!(span_of(90_000.0), "1m");
    }

    #[test]
    fn moments() {
        let noon = local_ms(2026, 3, 10, 12, 0).unwrap();
        assert_eq!(when(None, noon), "?");
        assert_eq!(when(Some(noon + 3 * HOUR + 20 * MINUTE), noon), "15:20");
        assert_eq!(
            when(Some(local_ms(2026, 3, 14, 9, 0).unwrap()), noon),
            "Sat 09:00"
        );
        assert_eq!(when(Some(local_ms(2026, 10, 30, 9, 0).unwrap()), noon), "Oct 30");
        assert_eq!(short_date(noon), "Mar 10");
        assert_eq!(day_label("2026-03-10"), "Tue");
        assert_eq!(day_label("nonsense"), "?");
    }

    #[test]
    fn amounts() {
        assert_eq!(pct(None), "?");
        assert_eq!(pct(Some(41.5)), "42%");
        assert_eq!(pct(Some(41.4)), "41%");
        assert_eq!(plural(1, "account"), "1 account");
        assert_eq!(plural(0, "account"), "0 accounts");
        assert_eq!(plural_as(2, "entry", "entries"), "2 entries");
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
    }
}
