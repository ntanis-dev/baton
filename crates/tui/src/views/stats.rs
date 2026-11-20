//! The last 7 days: per account its 5-hour peak and weekly use per day (a bar and the number in each
//! day's column), then hops, refusals and sessions per day with their totals, and the weekly allowance
//! left unused when a window reset.
//!
//! The labels sit in one narrow column whatever the accounts are called, so the table fits the menu at
//! its smallest; a day with nothing to show is a quiet dot, and a row with no day at all says so once.

use std::collections::BTreeMap;

use baton_core::charts::usage_tone;
use baton_core::clock::parse_ms;
use baton_core::lines::{Facts, Line, Span, p, s, wrap_facts};
use baton_core::text::{day_label, round, short_date};
use baton_core::theme::Glyphs;

use super::common::{pad_end, pad_start, section_rule};
use crate::pane::{ViewModel, body};
use crate::state::State;
use crate::tree::mark_icon;

/// The cells of a day's column: a bar, the number in three, a gap.
const COLUMN: usize = 5;

/// The cells of the label column: two of indent, the longest label, a gap.
const LABELS: usize = 11;

/// The cells of the totals column.
const TOTAL: usize = 7;

/// The tick for a percentage.
fn level(g: &Glyphs, v: f64) -> &'static str {
    let top = g.ticks.len() - 1;
    g.ticks[(round(v / 100.0 * top as f64).max(0.0) as usize).min(top)]
}

/// A day with nothing to show: a dot under the last digit.
fn quiet(g: &Glyphs) -> [Span; 2] {
    [s(pad_start(g.normal, COLUMN - 1), "muted"), p(" ")]
}

/// A row's label, in its column.
fn label(text: &str) -> Span {
    s(pad_end(&format!("  {text}"), LABELS), "muted")
}

/// One window of one account over the days: a bar and the percentage per day, toned by how close the
/// day came to the threshold. A window with no reading on any day says so once.
fn usage_row(name: &str, values: &[Option<f64>], threshold: Option<f64>, g: &Glyphs) -> Line {
    let mut line = vec![label(name)];
    if values.iter().all(Option::is_none) {
        line.push(s("no readings", "muted"));
        return line;
    }
    for value in values {
        let Some(v) = *value else {
            line.extend(quiet(g));
            continue;
        };
        let tone = threshold.map_or("ok", |threshold| usage_tone(Some(v), threshold));
        line.push(s(level(g, v), tone));
        line.push(s(pad_start(&format!("{}", round(v) as i64), COLUMN - 2), tone));
        line.push(p(" "));
    }
    line
}

/// A count per day, and the sum when there is room for it.
fn count_row(name: &str, values: &[usize], tone: &str, total: bool, g: &Glyphs) -> Line {
    let mut line = vec![label(name)];
    for value in values {
        if *value == 0 {
            line.extend(quiet(g));
        } else {
            line.push(s(pad_start(&value.to_string(), COLUMN - 1), tone));
            line.push(p(" "));
        }
    }
    if total {
        let sum: usize = values.iter().sum();
        let style = if sum == 0 { "muted" } else { tone };
        line.push(s(pad_start(&sum.to_string(), TOTAL), style));
    }
    line
}

/// The days as a header over the columns, today's (the last) in bold.
fn days_row(days: &[String], total: bool) -> Line {
    let mut line = vec![p(" ".repeat(LABELS))];
    for (at, day) in days.iter().enumerate() {
        let today = at + 1 == days.len();
        line.push(s(
            pad_start(day_label(day), COLUMN - 1),
            if today { "bold" } else { "muted" },
        ));
        line.push(p(" "));
    }
    if total {
        line.push(s(pad_start("Total", TOTAL), "muted"));
    }
    line
}

/// The Stats' body.
pub fn stats_body(state: &State, width: usize) -> Vec<Line> {
    let g = state.glyphs.glyphs();
    let Some(data) = state.data.as_deref() else {
        return Vec::new();
    };
    let stats = &data.stats;
    let table_width = LABELS + COLUMN * stats.days.len();
    let fits = table_width <= width;
    let total = table_width + TOTAL <= width;
    let date = |at: &str| parse_ms(at).map(short_date).unwrap_or_default();
    // "last 7 days" is in the title row already.
    let mut body: Vec<Line> = vec![
        vec![s(
            format!("{} to {}", date(&stats.since), date(&stats.until)),
            "muted",
        )],
        Vec::new(),
        section_rule("USAGE BY DAY", width, g),
    ];
    // In a pane too narrow for the columns the days run as sparklines instead.
    if fits {
        body.push(days_row(&stats.days, false));
    }
    let ticks = |values: &[Option<f64>], threshold: Option<f64>| -> Vec<Span> {
        values
            .iter()
            .map(|v| match (*v, threshold) {
                (None, _) => s(g.normal, "muted"),
                (Some(v), Some(threshold)) => s(level(g, v), usage_tone(Some(v), threshold)),
                (Some(v), None) => s(level(g, v), "ok"),
            })
            .collect()
    };
    let any_five = stats
        .accounts
        .iter()
        .any(|a| a.daily.iter().any(|d| d.five_hour_max.is_some()));
    for a in &stats.accounts {
        let own = data.states.iter().find(|x| x.id == a.id);
        let t5 = own.map(|x| x.windows.five_hour.threshold);
        let tw = own.map(|x| x.windows.seven_day.threshold);
        let five: Vec<Option<f64>> = a.daily.iter().map(|d| d.five_hour_max).collect();
        let week: Vec<Option<f64>> = a.daily.iter().map(|d| d.weekly_last).collect();
        body.push(vec![mark_icon(a.mark, g), p(" "), s(&a.name, "bold")]);
        // A window nobody had a reading of all week takes no row at all (it is said once, below).
        let rows = [("5h peak", &five, t5, any_five), ("weekly", &week, tw, true)];
        if fits {
            for (name, values, threshold, shown) in rows {
                if shown {
                    body.push(usage_row(name, values, threshold, g));
                }
            }
        } else {
            for (name, values, threshold, _) in rows.into_iter().filter(|row| row.3) {
                let mut line = vec![label(name)];
                line.extend(ticks(values, threshold));
                body.push(line);
            }
        }
    }
    if !any_five && !stats.accounts.is_empty() {
        body.push(vec![s("  no 5-hour readings in these days", "muted")]);
    }
    body.push(Vec::new());
    body.push(section_rule("ACTIVITY", width, g));
    if fits {
        body.push(days_row(&stats.days, total));
    }
    let row = |name: &str, per_day: &BTreeMap<String, usize>, tone: &str| -> Line {
        let values: Vec<usize> = stats
            .days
            .iter()
            .map(|d| per_day.get(d).copied().unwrap_or(0))
            .collect();
        if fits {
            return count_row(name, &values, tone, total, g);
        }
        let joined = values.iter().map(usize::to_string).collect::<Vec<_>>().join(" ");
        vec![label(name), s(joined, tone)]
    };
    body.push(row("Hops", &stats.totals.hops, "bold"));
    body.push(row("Refusals", &stats.totals.refusals, "warn"));
    body.push(row("Sessions", &stats.totals.sessions, "bold"));
    if !stats.hop_reasons.is_empty() {
        let reasons: Vec<Line> = stats
            .hop_reasons
            .iter()
            .map(|(r, n)| vec![s(format!("{n} "), "bold"), s(r, "muted")])
            .collect();
        body.extend(wrap_facts(
            &reasons,
            width,
            &Facts {
                lead: vec![label("Why hops")],
                indent: LABELS,
                ..Facts::default()
            },
        ));
    }
    body.push(Vec::new());
    body.push(section_rule("WEEKLY ALLOWANCE LOST AT RESETS", width, g));
    let mut lost = false;
    let name_width = stats
        .accounts
        .iter()
        .filter(|a| !a.wasted.is_empty())
        .map(|a| a.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(32);
    for a in &stats.accounts {
        for w in &a.wasted {
            lost = true;
            body.extend(wrap_facts(
                &[
                    vec![s(pad_end(&date(&w.reset_at), 6), "muted")],
                    vec![s(
                        pad_start(&format!("{}% unused", round(w.unused) as i64), 11),
                        if w.unused >= 10.0 { "warn bold" } else { "bold" },
                    )],
                    vec![s(
                        format!("{}% under its threshold", round(w.headroom_left) as i64),
                        "muted",
                    )],
                ],
                width,
                &Facts {
                    lead: vec![s(format!("  {}  ", pad_end(&a.name, name_width)), "bold")],
                    indent: 4,
                    sep: p("  "),
                },
            ));
        }
    }
    if !lost {
        // The line belongs right under the heading, before any entry.
        body.push(vec![s("  no weekly reset in these 7 days", "muted")]);
    }
    body
}

/// The Stats' pane.
pub fn model(state: &State, width: usize) -> ViewModel {
    ViewModel {
        title: vec![s("Stats", "bold")],
        right: vec![s("last 7 days", "muted")],
        body: body(stats_body(state, width)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::lines::plain;
    use baton_core::theme::RICH;

    #[test]
    fn a_window_over_the_days() {
        let days = [None, Some(6.4), Some(100.0), None];
        let row = usage_row("weekly", &days, Some(95.0), &RICH);
        assert_eq!(plain(&row), "  weekly      · ▁  6 █100    · ");
        // Toned by how close the day came to the threshold.
        assert_eq!((row[3].style.as_str(), row[6].style.as_str()), ("ok", "err"));
        assert_eq!(
            plain(&usage_row("5h peak", &[None, None], Some(90.0), &RICH)),
            "  5h peak  no readings"
        );
    }

    #[test]
    fn counts_over_the_days_with_their_sum() {
        let row = count_row("Refusals", &[0, 4, 13], "warn", true, &RICH);
        assert_eq!(plain(&row), "  Refusals    ·    4   13      17");
        assert_eq!(
            plain(&count_row("Hops", &[0, 0], "bold", true, &RICH)),
            "  Hops        ·    ·       0"
        );
        assert_eq!(
            plain(&count_row("Hops", &[2], "bold", false, &RICH)),
            "  Hops        2 "
        );
    }

    #[test]
    fn the_days_sit_over_the_numbers_and_today_is_bold() {
        let days = ["2026-03-09".to_owned(), "2026-03-10".to_owned()];
        let row = days_row(&days, true);
        assert_eq!(plain(&row), "            Mon  Tue   Total");
        assert_eq!((row[1].style.as_str(), row[3].style.as_str()), ("muted", "bold"));
        // The table fits the menu at its smallest: 48 cells of pane for a week.
        const { assert!(LABELS + COLUMN * 7 <= 48) };
    }
}
