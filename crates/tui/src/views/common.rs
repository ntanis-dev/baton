//! Pieces the panes share.

use baton_core::charts::usage_bar;
use baton_core::clock::Ms;
use baton_core::lines::{Line, Span, line_width, p, s, wrap_line};
use baton_core::policy::WindowState;
use baton_core::text::{pct, span, when};
use baton_core::theme::Glyphs;

/// `text` padded with spaces at its end to `width` characters.
pub fn pad_end(text: &str, width: usize) -> String {
    let have = text.encode_utf16().count();
    format!("{text}{}", " ".repeat(width.saturating_sub(have)))
}

/// `text` padded with spaces at its start to `width` characters.
pub fn pad_start(text: &str, width: usize) -> String {
    let have = text.encode_utf16().count();
    format!("{}{text}", " ".repeat(width.saturating_sub(have)))
}

/// A section's title, with a note after it when there is one.
pub fn section(title: &str, extra: &str) -> Line {
    let mut line = vec![s(title, "bold")];
    if !extra.is_empty() {
        line.push(s(format!("  {extra}"), "muted"));
    }
    line
}

/// A section's title with a rule to the pane's edge.
pub fn section_rule(title: &str, width: usize, g: &Glyphs) -> Line {
    let rule = width.saturating_sub(line_width(&[p(title)]) + 2);
    vec![
        s(title, "muted bold"),
        s(format!("  {}", g.corners.h.repeat(rule)), "divider"),
    ]
}

/// "label  value" rows with the labels in a column; a long value wraps under itself.
pub fn field(label: &str, value: &[Span], width: usize, label_width: usize) -> Vec<Line> {
    let mut line = vec![s(pad_end(label, label_width), "muted")];
    line.extend(value.iter().cloned());
    wrap_line(&line, width, label_width)
}

/// The words for where new work goes, from the policy's reason.
pub fn reason_words(reason: &str) -> &str {
    match reason {
        "preferred" => "preferred, with 5-hour room",
        "urgency" => "the most weekly allowance to lose before its reset",
        "low-reserve" => "the most 5-hour room left",
        "last-resort" => "every other account is blocked",
        "order" => "first open account in order",
        other => other,
    }
}

/// One window of an account as a line: label, bar with the threshold mark, used %, and its reset when
/// there is room (dropped first, never cut).
pub fn window_line(
    label: &str,
    w: &WindowState,
    width: usize,
    g: &Glyphs,
    now: Ms,
    label_width: usize,
) -> Line {
    let head = s(pad_end(label, label_width), "muted");
    // A plan with no such limit has nothing to draw.
    if w.absent {
        return vec![head, s("no such limit on this plan", "muted")];
    }
    let value = match w.used {
        None => s(if w.reset { "reset" } else { "not measured" }, "muted"),
        Some(used) => s(
            pad_start(&format!(" {}", pct(Some(used))), 5),
            if used >= w.threshold { "err bold" } else { "bold" },
        ),
    };
    let resets = match (w.used, w.resets_at.filter(|at| *at != 0)) {
        (Some(_), Some(at)) => vec![
            s(format!("  resets {}", when(Some(at), now)), "muted"),
            s(format!(" ({})", span(Some(at - now))), "muted"),
        ],
        _ => Vec::new(),
    };
    // Every bar of a pane the same width (room kept for the longest reset text); a narrow pane drops the
    // resets instead of shrinking the bar.
    let fixed = label_width + 5 + 1;
    let with_reset = width as i64 - fixed as i64 - 27;
    let bar_width = (if with_reset >= 12 {
        with_reset
    } else {
        width as i64 - fixed as i64
    })
    .clamp(8, 40) as usize;
    let mut line = vec![head];
    line.extend(usage_bar(w.used, w.threshold, bar_width, g));
    line.push(p(" "));
    line.push(value);
    if line_width(&line) + line_width(&resets) <= width {
        line.extend(resets);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::clock::{HOUR, local_ms};
    use baton_core::lines::plain;
    use baton_core::policy::window_state;
    use baton_core::theme::RICH;
    use baton_core::window::Window;

    #[test]
    fn sections_and_fields() {
        assert_eq!(plain(&section("Hops", "")), "Hops");
        assert_eq!(plain(&section("Hops", "none")), "Hops  none");
        assert_eq!(plain(&section_rule("TODAY", 12, &RICH)), "TODAY  ─────");
        assert_eq!(plain(&section_rule("TODAY", 3, &RICH)), "TODAY  ");
        let lines = field("Folder", &[p("a/long/path that wraps")], 22, 10);
        assert_eq!(
            lines.iter().map(|l| plain(l)).collect::<Vec<_>>(),
            ["Folder    a/long/path", "          that wraps"]
        );
        assert_eq!(
            reason_words("urgency"),
            "the most weekly allowance to lose before its reset"
        );
        assert_eq!(reason_words("something-new"), "something-new");
        assert_eq!(pad_start("7", 3), "  7");
        assert_eq!(pad_end("abcd", 2), "abcd");
    }

    #[test]
    fn a_window_as_a_line() {
        let now = local_ms(2026, 3, 10, 12, 0).unwrap();
        let policy = baton_core::config::Policy::default();
        let reading = baton_core::usage::WindowReading {
            used: 42.0,
            resets_at: Some(baton_core::clock::iso(now + 2 * HOUR)),
            read_at: Some(baton_core::clock::iso(now)),
            source: None,
            label: None,
        };
        let w = window_state(Window::FiveHour, Some(&reading), None, 90.0, now, &policy);
        assert_eq!(
            plain(&window_line("5-hour", &w, 68, &RICH, now, 8)),
            "5-hour  ━━━━━━━━━━━────────────┃───   42%  resets 14:00 (2h)"
        );
        // A narrow pane drops the reset, never the bar.
        assert_eq!(
            plain(&window_line("5h", &w, 24, &RICH, now, 4)),
            "5h  ━━━━━━──────┃─   42%"
        );
        let none = window_state(Window::FiveHour, None, None, 90.0, now, &policy);
        assert_eq!(
            plain(&window_line("5h", &none, 24, &RICH, now, 4)),
            "5h  ────────────┃─ not measured"
        );
    }
}
