//! One account: both bars with the threshold marks and the reset countdowns (and a bar for each
//! model's own weekly limit), blocked-by and when it is back, the weekly urgency in words, its mark, order and thresholds, its sign-in (method only), its
//! sessions, 7 days of use per window and its hops.

use baton_core::charts::spark;
use baton_core::clock::{Ms, parse_ms};
use baton_core::config::Mark;
use baton_core::format::{TimeWords, other_limits, poll_words, read_words};
use baton_core::json::show;
use baton_core::lines::{Facts, Line, Span, p, s, wrap_facts, wrap_line};
use baton_core::policy::{AccountState, WindowState};
use baton_core::text::{day_label, plural, round, span, when};
use baton_core::usage::is_weekly_meter;
use baton_core::window::WINDOWS;

use super::common::{field, window_line};
use crate::pane::{ViewModel, body};
use crate::state::{State, account_by_id, accounts, config_of, sessions_on};
use crate::tree::mark_icon;

/// The weekly urgency in words: what is lost if it goes unused. Of the week every model draws on:
/// a model's own limit is said beside it.
pub fn urgency_words(a: &AccountState, now: Ms) -> String {
    let week = &a.weekly_all;
    let (Some(used), Some(resets)) = (week.used, week.resets_at.filter(|at| *at != 0)) else {
        return "no weekly reading yet".to_owned();
    };
    let left = (week.threshold - used).max(0.0);
    if left == 0.0 {
        return format!("weekly limit reached; resets in {}", span(Some(resets - now)));
    }
    format!(
        "loses {}% in {} if unused",
        round(left) as i64,
        span(Some(resets - now))
    )
}

/// An account's sign-in, as far as it is known.
fn sign_in_words(state: &State, id: &str) -> Line {
    let Some(auth) = state.auth.get(id).filter(|auth| auth.error.is_none()) else {
        return vec![s("sign-in unknown", "muted")];
    };
    if auth.logged_in == Some(true) {
        let method = auth.auth_method.as_deref().filter(|m| !m.is_empty());
        return vec![
            s("signed in", "ok"),
            s(method.map(|m| format!(" ({m})")).unwrap_or_default(), "muted"),
        ];
    }
    vec![s("signed out", "warn bold")]
}

/// An amount with two decimals, a half rounding up: `12.50`.
fn fixed2(amount: f64) -> String {
    let cents = round(amount.abs() * 100.0) as i64;
    let sign = if amount < 0.0 && cents != 0 { "-" } else { "" };
    format!("{sign}{}.{:02}", cents / 100, cents % 100)
}

/// The poll outcomes that are trouble.
const POLL_TROUBLE: [&str; 4] = ["unavailable", "rate-limited", "timeout", "failed"];

/// How the menu writes times: 24-hour moments and "12m".
pub const MENU_TIME: TimeWords = TimeWords { time: when, span };

/// Where each window's reading came from and how old it is, the plan's other limits (the weekly ones
/// have bars of their own) and extra usage from the last poll, and the poller's last word on the
/// account.
fn usage_lines(state: &State, a: &AccountState, width: usize) -> Vec<Line> {
    let now = state.now;
    let Some(data) = state.data.as_deref() else {
        return Vec::new();
    };
    let reading = data.readings.get(&a.id);
    let words = |w: &WindowState| {
        w.read_at
            .map(|read_at| read_words(w.source.as_deref(), Some(read_at), now, MENU_TIME))
    };
    let five = words(&a.windows.five_hour);
    let week = words(&a.windows.seven_day);
    let mut out = Vec::new();
    if five.is_some() || week.is_some() {
        let mut value: Line = Vec::new();
        match (&five, &week) {
            (Some(five), Some(week)) if five == week => value.push(p(five)),
            _ => {
                if let Some(five) = &five {
                    value.push(s("5-hour ", "muted"));
                    value.push(p(five));
                }
                if five.is_some() && week.is_some() {
                    value.push(s(" · ", "muted"));
                }
                if let Some(week) = &week {
                    value.push(s("weekly ", "muted"));
                    value.push(p(week));
                }
            }
        }
        out.extend(field("Read", &value, width, 12));
    }
    let others = other_limits(reading, now)
        .into_iter()
        .filter(|r| !is_weekly_meter(r));
    for (i, r) in others.enumerate() {
        let group = r.group.as_deref().filter(|g| !g.is_empty() && *g != "session");
        let name = format!(
            "{}{}",
            r.label.as_deref().unwrap_or(&r.kind),
            group.map(|g| format!(" {g}")).unwrap_or_default()
        );
        let mut value = vec![
            s(name, "bold"),
            s(
                format!(" {}%", round(r.used) as i64),
                if r.used >= a.windows.seven_day.threshold {
                    "err bold"
                } else {
                    ""
                },
            ),
        ];
        if let Some(resets) = r.resets_at.as_deref().and_then(parse_ms) {
            value.push(s(
                format!(
                    "  resets {} ({})",
                    when(Some(resets), now),
                    span(Some(resets - now))
                ),
                "muted",
            ));
        }
        out.extend(field(if i > 0 { "" } else { "Limits" }, &value, width, 12));
    }
    if let Some(extra) = reading.and_then(|r| r.extra_usage.as_ref()) {
        let currency = extra.currency.as_deref().filter(|c| !c.is_empty());
        let money = |v: f64| {
            format!(
                "{}{}",
                fixed2(v / 100.0),
                currency.map(|c| format!(" {c}")).unwrap_or_default()
            )
        };
        let mut value = vec![s(
            if extra.enabled { "on" } else { "off" },
            if extra.enabled { "" } else { "muted" },
        )];
        if let Some(used) = extra.used_credits {
            value.push(p(format!(", {} used", money(used))));
        }
        if let Some(limit) = extra.monthly_limit {
            value.push(s(format!(" of {} this month", money(limit)), "muted"));
        }
        out.extend(field("Extra usage", &value, width, 12));
    }
    if let Some(poll) = &data.poll {
        let entry = poll.state.accounts.get(&a.id);
        let polling = poll
            .state
            .running
            .as_ref()
            .is_some_and(|running| running.account == a.id);
        let said = if polling {
            "polling now".to_owned()
        } else {
            poll_words(entry, now, MENU_TIME)
        };
        let trouble = entry
            .and_then(|e| e.status.as_deref())
            .is_some_and(|status| POLL_TROUBLE.contains(&status));
        out.extend(field(
            "Usage poll",
            &[s(said, if trouble { "warn" } else { "muted" })],
            width,
            12,
        ));
    }
    out
}

/// One account's body.
pub fn account_body(state: &State, id: &str, width: usize) -> Vec<Line> {
    let (Some(a), Some(data)) = (account_by_id(state, id), state.data.as_deref()) else {
        return vec![vec![s("This account is gone.", "muted")]];
    };
    let g = state.glyphs.glyphs();
    let now = state.now;
    let all = accounts(state);
    let config = config_of(state, id);
    let own_five = config.is_some_and(|c| c.five_hour.is_some());
    let own_week = config.is_some_and(|c| c.weekly.is_some());
    let mut body: Vec<Line> = Vec::new();
    let mut facts: Vec<Line> = vec![
        vec![s(format!("#{} of {}", a.index + 1, all.len()), "muted")],
        vec![s(
            a.mark.as_str(),
            match a.mark {
                Mark::Preferred => "baton bold",
                Mark::LastResort => "muted",
                Mark::Normal => "",
            },
        )],
        sign_in_words(state, id),
    ];
    if data.next.account.as_deref() == Some(id) {
        facts.push(vec![s(format!("{} next work goes here", g.baton), "baton")]);
    }
    // The name is the pane's title already.
    let layout = Facts {
        lead: vec![mark_icon(a.mark, g), p(" ")],
        indent: 2,
        ..Facts::default()
    };
    body.extend(wrap_facts(&facts, width, &layout));
    body.extend(field(
        "Thresholds",
        &[
            if a.windows.five_hour.absent {
                s("5-hour none", "muted")
            } else {
                s(
                    format!("5-hour {}%", show(a.windows.five_hour.threshold)),
                    if own_five { "bold" } else { "" },
                )
            },
            s(" · ", "muted"),
            s(
                format!("weekly {}%", show(a.windows.seven_day.threshold)),
                if own_week { "bold" } else { "" },
            ),
            s(
                if !own_five && !own_week {
                    "  (policy defaults)"
                } else {
                    ""
                },
                "muted",
            ),
        ],
        width,
        12,
    ));
    body.push(Vec::new());
    body.push(window_line("5-hour", &a.windows.five_hour, width, g, now, 8));
    // The week every model draws on, then each model's own limit under its name: the two are
    // separate limits, and either one can be the full one.
    body.push(window_line("weekly", &a.weekly_all, width, g, now, 8));
    for own in &a.weekly_own {
        let name: String = own.label.as_deref().unwrap_or("model").chars().take(7).collect();
        body.push(window_line(&name, own, width, g, now, 8));
    }
    body.extend(usage_lines(state, a, width));
    let until = match a.available_at.filter(|_| a.available_known) {
        Some(at) => format!(" until {} (in {})", when(Some(at), now), span(Some(at - now))),
        None => "; its reset time is unknown".to_owned(),
    };
    if !a.limited_models.is_empty() {
        // Only a model's own limit: not the account's. Said by the model's name, in amber, with
        // what still runs here.
        let models = a.limited_models.join(" and ");
        let verb = if a.limited_models.len() == 1 { "is" } else { "are" };
        body.extend(wrap_line(
            &[
                s(format!("{} ", g.blocked), "warn bold"),
                s(format!("{models} {verb} at the weekly limit"), "warn bold"),
                s(until, "warn"),
            ],
            width,
            2,
        ));
        body.extend(wrap_line(
            &[s("  chats on other models still run here", "muted")],
            width,
            2,
        ));
        if a.windows.seven_day.rejected {
            body.extend(wrap_line(
                &[s("  refused by Anthropic, not only over the threshold", "muted")],
                width,
                2,
            ));
        }
    } else if a.blocked {
        let by = a
            .blocked_by
            .iter()
            .map(|limit| limit.label())
            .collect::<Vec<_>>()
            .join(" and the ");
        body.extend(wrap_line(
            &[
                s(format!("{} ", g.blocked), "err bold"),
                s(format!("Blocked by the {by} limit"), "err bold"),
                s(until, "err"),
            ],
            width,
            2,
        ));
        let refused = WINDOWS.iter().any(|w| a.windows[*w].rejected);
        if refused || a.unknown_rejection.is_some() {
            body.extend(wrap_line(
                &[s("  refused by Anthropic, not only over the threshold", "muted")],
                width,
                2,
            ));
        }
    } else {
        body.push(vec![
            s(format!("{} ", g.check), "ok bold"),
            s("Available now", "ok"),
        ]);
    }
    body.extend(field("Weekly", &[p(urgency_words(a, now))], width, 12));
    body.push(Vec::new());
    // 7 days per window.
    if let Some(stats) = data.stats.accounts.iter().find(|x| x.id == id) {
        // A column per day ("Mon" over its bar) where there is room, else a cell.
        let wide = width >= 12 + stats.daily.len() * 4;
        let cells_of = |values: Vec<Option<f64>>, style: &str| -> Line {
            let line = spark(&values, g, style, 100.0);
            if !wide {
                return line;
            }
            line.iter()
                .flat_map(|span: &Span| {
                    span.text
                        .chars()
                        .flat_map(|ch| [s(ch.to_string().repeat(3), &span.style), p(" ")])
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let days = if wide {
            stats
                .daily
                .iter()
                .map(|d| day_label(&d.date))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            stats
                .daily
                .iter()
                .filter_map(|d| day_label(&d.date).chars().next())
                .collect()
        };
        body.extend(field("7 days", &[s(days, "muted")], width, 12));
        body.extend(field(
            "  5h peak",
            &cells_of(stats.daily.iter().map(|d| d.five_hour_max).collect(), "accent"),
            width,
            12,
        ));
        body.extend(field(
            "  weekly",
            &cells_of(stats.daily.iter().map(|d| d.weekly_last).collect(), "info"),
            width,
            12,
        ));
        let hops_in: usize = stats.daily.iter().map(|d| d.hops_in).sum();
        let hops_out: usize = stats.daily.iter().map(|d| d.hops_out).sum();
        body.extend(field(
            "Hops",
            &[
                s(
                    format!("{hops_in} in"),
                    if hops_in > 0 { "bold" } else { "muted" },
                ),
                s(" · ", "muted"),
                s(
                    format!("{hops_out} out"),
                    if hops_out > 0 { "bold" } else { "muted" },
                ),
                s(" · ", "muted"),
                s(
                    plural(stats.refusals, "refusal"),
                    if stats.refusals > 0 { "warn" } else { "muted" },
                ),
            ],
            width,
            12,
        ));
    }
    let live = sessions_on(state, id);
    let running: Line = if live.is_empty() {
        vec![s("none running", "muted")]
    } else {
        let mut names = Vec::new();
        for (i, x) in live.iter().enumerate() {
            if i > 0 {
                names.push(s(", ", "muted"));
            }
            names.push(s(&x.name, "bold"));
        }
        names
    };
    body.extend(field("Sessions", &running, width, 12));
    body
}

/// An account's pane.
pub fn model(state: &State, id: &str, width: usize) -> ViewModel {
    let a = account_by_id(state, id);
    let g = state.glyphs.glyphs();
    ViewModel {
        title: vec![s(a.map_or(id, |a| a.name.as_str()), "bold")],
        right: match a {
            Some(a) if !a.limited_models.is_empty() => vec![s(
                format!("{} {} limit", g.blocked, a.limited_models.join(", ")),
                "warn",
            )],
            Some(a) if a.blocked => vec![s(format!("{} blocked", g.blocked), "err")],
            Some(_) => vec![s("available", "ok")],
            None => Vec::new(),
        },
        body: body(account_body(state, id, width)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_have_two_decimals() {
        assert_eq!(fixed2(12.5), "12.50");
        assert_eq!(fixed2(50.0), "50.00");
        assert_eq!(fixed2(0.125), "0.13");
        assert_eq!(fixed2(0.004), "0.00");
        assert_eq!(fixed2(-3.456), "-3.46");
        assert_eq!(fixed2(-0.001), "0.00");
    }
}
