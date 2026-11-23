//! The Overview: where new work goes, every account's two windows and status, the live sessions and
//! today's moves.

use baton_core::json::{Get, show};
use baton_core::lines::{Line, Span, fit_line, p, s, spread, wrap_line};
use baton_core::policy::WindowState;
use baton_core::text::{plural, round, span, when};
use baton_core::theme::Glyphs;

use super::common::{pad_end, pad_start, reason_words, section_rule};
use crate::pane::{BodyLine, Link, ViewModel};
use crate::state::{State, accounts, live_sessions, name_of, signed_out};
use crate::tree::session_icon;

/// When the usage is read again, for the pane's note.
pub fn poll_when(state: &State) -> Line {
    let poll = state.data.as_ref().and_then(|data| data.poll.as_ref());
    let Some(poll) = poll.filter(|poll| poll.holder.is_some()) else {
        return vec![s("Usage updates while Baton is running", "muted")];
    };
    if poll.state.running.is_some() {
        return vec![s("Refreshing usage", "accent")];
    }
    let Some(next) = poll.next_at.filter(|at| *at != 0) else {
        return vec![s("Refresh on request", "muted")];
    };
    let every = state
        .data
        .as_ref()
        .map_or(0.0, |data| data.policy.usage_poll_minutes);
    vec![s(
        format!(
            "Usage updates every {}m  ·  next {}",
            show(every),
            when(Some(next), state.now)
        ),
        "muted",
    )]
}

/// A line that a click selects `id` through, over the pane's whole width.
fn linked(line: Line, id: &str, width: usize) -> BodyLine {
    BodyLine {
        line,
        links: vec![Link {
            x: 0,
            width,
            select: id.to_owned(),
        }],
    }
}

/// A window as three cells of bar and its use.
fn meter(window: &WindowState, g: &Glyphs) -> [Span; 3] {
    // A plan with no such limit: said, where the bar would be.
    if window.absent {
        return [
            s("", "muted"),
            s(pad_end("none", 3), "muted"),
            s(pad_start("", 4), "muted"),
        ];
    }
    let used = window.used;
    let fill = used.map_or(0, |used| (round(used / 100.0 * 3.0).max(0.0) as usize).min(3));
    let over = used.is_some_and(|used| used >= window.threshold);
    let number = used.map_or_else(|| " -".to_owned(), |used| format!("{}%", round(used) as i64));
    [
        s(g.full.repeat(fill), if over { "err" } else { "accent" }),
        s(g.empty.repeat(3 - fill), "muted"),
        s(pad_start(&number, 5), if over { "err" } else { "text" }),
    ]
}

/// The Overview's body, with what a click on each line selects.
pub fn overview_rows(state: &State, width: usize) -> Vec<BodyLine> {
    let g = state.glyphs.glyphs();
    let list = accounts(state);
    let Some(data) = state.data.as_deref() else {
        return vec![vec![s("Loading accounts...", "muted")].into()];
    };
    if list.is_empty() {
        let mut body: Vec<BodyLine> = vec![
            vec![s("Your next conversation starts here.", "bold")].into(),
            BodyLine::default(),
        ];
        let words = "Add your first account, then sign in with Claude Code. Baton keeps the conversation moving between the accounts you add.";
        body.extend(
            wrap_line(&[s(words, "muted")], width, 0)
                .into_iter()
                .map(BodyLine::from),
        );
        body.push(BodyLine::default());
        body.push(linked(
            vec![s("+ Add an account", "accent bold")],
            "add-account",
            width,
        ));
        return body;
    }
    let live = live_sessions(state);
    let ready = list
        .iter()
        .filter(|a| !a.blocked && !signed_out(state, &a.id))
        .count();
    let mut body: Vec<BodyLine> = vec![
        vec![
            s(format!("{ready} ready"), if ready > 0 { "ok" } else { "warn" }),
            s(
                format!(
                    "   /   {}   /   {}",
                    plural(list.len(), "account"),
                    plural(live.len(), "live session")
                ),
                "muted",
            ),
        ]
        .into(),
        BodyLine::default(),
        section_rule("NEXT ACCOUNT", width, g).into(),
    ];
    let next = &data.next;
    if let Some(account) = &next.account {
        body.push(linked(
            vec![s(name_of(state, account), "bold")],
            &format!("acct:{account}"),
            width,
        ));
        body.extend(
            wrap_line(&[s(reason_words(&next.reason), "muted")], width, 0)
                .into_iter()
                .map(BodyLine::from),
        );
    } else {
        body.push(vec![s("All accounts are at their limit", "warn")].into());
        if let Some(earliest) = &next.earliest {
            let back = match next.available_at.filter(|_| next.available_known) {
                Some(at) => format!(" returns in {}", span(Some(at - state.now))),
                None => " · reset time unknown".to_owned(),
            };
            body.push(linked(
                vec![s(format!("{}{back}", name_of(state, earliest)), "muted")],
                &format!("acct:{earliest}"),
                width,
            ));
        }
    }
    let name_width = width.saturating_sub(34).clamp(14, 30);
    body.push(BodyLine::default());
    body.push(section_rule("ACCOUNTS", width, g).into());
    body.push(
        vec![
            s(pad_end("Account", name_width + 2), "muted"),
            s(pad_end("5-hour", 10), "muted"),
            s(pad_end("Weekly", 10), "muted"),
            s("Status", "muted"),
        ]
        .into(),
    );
    for a in list {
        let is_next = next.account.as_deref() == Some(a.id.as_str());
        let names = wrap_line(
            &[s(&a.name, if is_next { "bold" } else { "text" })],
            name_width,
            0,
        );
        let out = signed_out(state, &a.id);
        let limited = !out && !a.limited_models.is_empty();
        let status = if out {
            "Sign in".to_owned()
        } else if limited {
            // Only a model's own limit: other models still run here.
            let back = match a.available_at.filter(|_| a.available_known) {
                Some(at) => format!(", {}", span(Some(at - state.now))),
                None => String::new(),
            };
            format!("{} limit{back}", a.limited_models.join(", "))
        } else if a.blocked {
            match a.available_at.filter(|_| a.available_known) {
                Some(at) => format!("Back in {}", span(Some(at - state.now))),
                None => "At limit".to_owned(),
            }
        } else {
            "Ready".to_owned()
        };
        for (i, name) in names.iter().enumerate() {
            let mut line = fit_line(name, name_width, "");
            line.push(p("  "));
            if i == 0 {
                line.extend(meter(&a.windows.five_hour, g));
                line.push(p("  "));
                line.extend(meter(&a.weekly_all, g));
                line.push(p("  "));
                line.push(s(
                    &status,
                    if out {
                        "accent"
                    } else if limited {
                        "warn"
                    } else if a.blocked {
                        "muted"
                    } else {
                        "ok"
                    },
                ));
            }
            body.push(linked(line, &format!("acct:{}", a.id), width));
        }
    }
    body.push(BodyLine::default());
    body.push(section_rule("LIVE SESSIONS", width, g).into());
    if live.is_empty() {
        body.push(vec![s("No sessions running. Start one with baton.", "muted")].into());
    }
    for session in live {
        let status = match (session.wrapper_state.as_deref(), session.turn_state.as_deref()) {
            (Some("countdown"), _) => {
                format!("Moving to {}", session.target_name.as_deref().unwrap_or("null"))
            }
            (Some("waiting"), _) => "Waiting".to_owned(),
            (_, Some("running")) => "Working".to_owned(),
            _ => "Idle".to_owned(),
        };
        let id = format!("sess:{}", session.id);
        let line = spread(
            &[session_icon(session, g), p(" "), s(&session.name, "bold")],
            &[s(session.account_name.as_deref().unwrap_or(""), "muted")],
            width,
            "",
        );
        body.push(linked(line, &id, width));
        let workflow = session
            .background_tasks
            .iter()
            .any(|task| task.text("type") == Some("workflow"));
        let words = format!("  {status}{}", if workflow { " · workflow running" } else { "" });
        body.push(linked(vec![s(words, "muted")], &id, width));
    }
    let today = data.stats.days.last();
    let count = |per_day: &std::collections::BTreeMap<String, usize>| {
        today.and_then(|day| per_day.get(day)).copied().unwrap_or(0)
    };
    let (hops, refusals) = (count(&data.stats.totals.hops), count(&data.stats.totals.refusals));
    body.push(BodyLine::default());
    body.push(section_rule("TODAY", width, g).into());
    body.push(
        vec![s(
            format!("{}  ·  {}", plural(hops, "handoff"), plural(refusals, "refusal")),
            "muted",
        )]
        .into(),
    );
    if data.differ > 0 {
        body.push(BodyLine::default());
        body.push(
            vec![s(
                format!(
                    "the accounts differ on {} setting{} · baton sync",
                    data.differ,
                    if data.differ == 1 { "" } else { "s" }
                ),
                "warn",
            )]
            .into(),
        );
    }
    body
}

/// The Overview's body as lines.
pub fn overview_body(state: &State, width: usize) -> Vec<Line> {
    overview_rows(state, width)
        .into_iter()
        .map(|row| row.line)
        .collect()
}

/// The Overview's pane.
pub fn model(state: &State, width: usize) -> ViewModel {
    ViewModel {
        title: vec![s("Overview", "bold")],
        right: poll_when(state),
        body: overview_rows(state, width),
    }
}
