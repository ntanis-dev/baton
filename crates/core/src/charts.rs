//! Graphs as spans: sparklines, bar charts, and the usage bar with its threshold.

use crate::lines::{Line, Span, s};
use crate::text::{round, width};
use crate::theme::Glyphs;

/// Characters with their styles as spans, neighbours of one style joined.
fn runs(chars: Vec<(&str, &str)>) -> Line {
    let mut spans: Line = Vec::new();
    let mut text = String::new();
    let mut style: Option<&str> = None;
    for (ch, st) in chars {
        if style != Some(st) && !text.is_empty() {
            spans.push(s(&text, style.unwrap_or_default()));
            text.clear();
        }
        style = Some(st);
        text.push_str(ch);
    }
    if !text.is_empty() {
        spans.push(s(&text, style.unwrap_or_default()));
    }
    spans
}

/// One cell per value (None = no reading: a dim dot), scaled to `max`.
pub fn spark(values: &[Option<f64>], g: &Glyphs, style: &str, max: f64) -> Line {
    let top = g.ticks.len() - 1;
    runs(
        values
            .iter()
            .map(|value| {
                let Some(v) = *value else {
                    return (g.normal, "muted");
                };
                let level = if v > 0.0 {
                    (round(v / max * top as f64) as usize).clamp(1, top)
                } else {
                    0
                };
                (g.ticks[level], if level > 0 { style } else { "muted" })
            })
            .collect(),
    )
}

/// A bar chart `height` rows tall, one column per value, the scale on the left: lines, the top row
/// first. `label` writes the scale's top value.
pub fn chart(
    values: &[f64],
    height: usize,
    g: &Glyphs,
    style: &str,
    label: &dyn Fn(f64) -> String,
    column_width: usize,
) -> Vec<Line> {
    let top = values.iter().copied().fold(0.0, f64::max);
    let axis = width(&label(top)).max(1) + 1;
    let steps = g.bars.len();
    let units = (height * steps) as f64;
    let levels: Vec<usize> = values
        .iter()
        .map(|v| {
            if *v > 0.0 && top > 0.0 {
                (round(v / top * units) as usize).max(1)
            } else {
                0
            }
        })
        .collect();
    let mut lines = Vec::new();
    for row in (0..height).rev() {
        let tag = if row == height - 1 {
            label(top)
        } else if row == 0 {
            "0".to_owned()
        } else {
            String::new()
        };
        let mut chars = Vec::new();
        for n in &levels {
            let filled = n.saturating_sub(row * steps);
            let cell = if filled >= steps {
                (g.bars[steps - 1], style)
            } else if filled > 0 {
                (g.bars[filled - 1], style)
            } else {
                (if row == 0 { g.ticks[0] } else { " " }, "muted")
            };
            for _ in 0..column_width {
                chars.push(cell);
            }
        }
        let pad = (axis - 1).saturating_sub(tag.chars().count());
        let mut line: Line = vec![s(format!("{}{tag}", " ".repeat(pad)), "muted"), s(" ", "muted")];
        line.extend(runs(chars));
        lines.push(line);
    }
    lines
}

/// How full a window is, in a colour: green with room, yellow within 15 points of the threshold, red
/// at or past it.
pub fn usage_tone(used: Option<f64>, threshold: f64) -> &'static str {
    match used {
        None => "muted",
        Some(used) if used >= threshold => "err",
        Some(used) if used >= threshold - 15.0 => "warn",
        Some(_) => "ok",
    }
}

/// used % as a bar `cells` wide with the threshold marked on it.
pub fn usage_bar(used: Option<f64>, threshold: f64, cells: usize, g: &Glyphs) -> Vec<Span> {
    let w = cells.max(4);
    let at = ((round(threshold / 100.0 * w as f64) as i64 - 1).max(0) as usize).min(w - 1);
    let filled = match used {
        Some(used) => (round(used.max(0.0) / 100.0 * w as f64) as usize).min(w),
        None => 0,
    };
    let tone = usage_tone(used, threshold);
    let over = used.is_some_and(|used| used >= threshold);
    let chars = (0..w)
        .map(|i| {
            if i == at {
                (g.limit, if over { "err bold" } else { "warn" })
            } else if i < filled {
                (g.full, tone)
            } else {
                (g.empty, "muted")
            }
        })
        .collect();
    runs(chars)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::plain;
    use crate::theme::{PLAIN, RICH};

    #[test]
    fn sparklines() {
        let line = spark(
            &[None, Some(0.0), Some(1.0), Some(50.0), Some(100.0), Some(250.0)],
            &RICH,
            "accent",
            100.0,
        );
        assert_eq!(plain(&line), "·▁▂▅██");
        assert_eq!(line[0].style, "muted");
        assert_eq!(line[1].style, "accent");
        assert_eq!(plain(&spark(&[Some(10.0), Some(20.0)], &PLAIN, "ok", 20.0)), "=#");
        assert!(spark(&[], &RICH, "accent", 100.0).is_empty());
    }

    #[test]
    fn bar_charts() {
        let whole = |n: f64| format!("{}", round(n) as i64);
        let lines = chart(&[0.0, 5.0, 10.0], 2, &RICH, "accent", &whole, 1);
        assert_eq!(
            lines.iter().map(|l| plain(l)).collect::<Vec<_>>(),
            ["10   █", " 0 ▁██"]
        );
        let wide = chart(&[4.0], 1, &PLAIN, "accent", &whole, 3);
        assert_eq!(plain(&wide[0]), "4 ███");
        let empty = chart(&[], 2, &RICH, "accent", &whole, 1);
        assert_eq!(empty.iter().map(|l| plain(l)).collect::<Vec<_>>(), ["0 ", "0 "]);
    }

    #[test]
    fn usage_bars() {
        assert_eq!(usage_tone(None, 90.0), "muted");
        assert_eq!(usage_tone(Some(50.0), 90.0), "ok");
        assert_eq!(usage_tone(Some(75.0), 90.0), "warn");
        assert_eq!(usage_tone(Some(90.0), 90.0), "err");
        let bar = usage_bar(Some(50.0), 90.0, 10, &RICH);
        assert_eq!(plain(&bar), "━━━━━───┃─");
        assert_eq!(bar[0].style, "ok");
        assert_eq!(bar[2].style, "warn");
        let over = usage_bar(Some(100.0), 90.0, 10, &RICH);
        assert_eq!(plain(&over), "━━━━━━━━┃━");
        assert_eq!(over[1].style, "err bold");
        assert_eq!(plain(&usage_bar(None, 90.0, 2, &PLAIN)), "░░░|");
    }
}
