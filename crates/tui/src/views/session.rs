//! A session: its account, project folder, start, its hops with their reasons, the cost of a turn and
//! the burn rate, and whether subagents or a workflow run (no planned hop then).

use baton_core::clock::parse_ms;
use baton_core::json::{Get, show};
use baton_core::lines::{Facts, Line, p, s, wrap_facts, wrap_line};
use baton_core::text::{hhmm, plural, round, span, when};

use super::common::{field, section};
use crate::pane::{ViewModel, body};
use crate::state::{State, recent_sessions, session_by_id};

/// A percentage to one decimal, or "?".
fn fixed(n: Option<f64>) -> String {
    match n {
        Some(n) => format!("{}%", show(round(n * 10.0) / 10.0)),
        None => "?".to_owned(),
    }
}

/// One session's body.
pub fn session_body(state: &State, id: &str, width: usize) -> Vec<Line> {
    let Some(x) = session_by_id(state, id) else {
        return vec![vec![s("This session is gone.", "muted")]];
    };
    let g = state.glyphs.glyphs();
    let now = state.now;
    let mut body: Vec<Line> = Vec::new();
    let location: Line = if x.live {
        match x.wrapper_state.as_deref() {
            Some("waiting") => {
                let until = match x.until.filter(|at| *at != 0) {
                    Some(at) => format!(" until {}", when(Some(at), now)),
                    None => String::new(),
                };
                vec![s(
                    format!(
                        "waiting for {}{until}",
                        x.target_name.as_deref().unwrap_or("an account")
                    ),
                    "warn bold",
                )]
            }
            Some("countdown") => {
                vec![s(
                    format!("moving to {}", x.target_name.as_deref().unwrap_or("null")),
                    "warn bold",
                )]
            }
            _ => vec![s("live under baton", "ok")],
        }
    } else {
        vec![s(
            if x.ended {
                "ended"
            } else {
                "not running under baton"
            },
            "muted",
        )]
    };
    // The name is the pane's title already.
    body.extend(wrap_facts(
        &[
            vec![
                s("on ", "muted"),
                s(x.account_name.as_deref().unwrap_or("?"), "accent bold"),
            ],
            location,
        ],
        width,
        &Facts {
            indent: 2,
            ..Facts::default()
        },
    ));
    body.extend(field("Folder", &[p(x.cwd.as_deref().unwrap_or("?"))], width, 10));
    body.extend(field("Session", &[s(&x.id, "muted")], width, 10));
    if let Some(started) = x.started_at.filter(|at| *at != 0) {
        body.extend(field(
            "Started",
            &[
                p(when(Some(started), now)),
                s(format!(" ({} ago)", span(Some(now - started))), "muted"),
            ],
            width,
            10,
        ));
    }
    if let Some(model) = x.model.as_deref().filter(|m| !m.is_empty()) {
        body.extend(field("Model", &[p(model)], width, 10));
    }
    let kind_of = |task: &serde_json::Value| task.text("type").unwrap_or_default().to_owned();
    let kinds: Vec<String> = x.background_tasks.iter().map(kind_of).collect();
    let busy: Vec<&String> = kinds
        .iter()
        .filter(|kind| matches!(kind.as_str(), "subagent" | "workflow" | "teammate"))
        .collect();
    let shells = kinds.iter().filter(|kind| kind.as_str() == "shell").count();
    let mut status: Line = if x.turn_state.as_deref() == Some("running") {
        vec![
            s(format!("{} ", g.spinner[0]), "accent spin"),
            s("working", "accent"),
        ]
    } else if x.turn_event.as_deref() == Some("stop-failure") {
        vec![s(
            format!("turn ended on {}", x.error.as_deref().unwrap_or("an API error")),
            if x.error.as_deref() == Some("rate_limit") {
                "err"
            } else {
                "warn"
            },
        )]
    } else {
        vec![s("between turns", "muted")]
    };
    if !busy.is_empty() {
        let workflow = busy.iter().any(|kind| kind.as_str() == "workflow");
        let others = busy.iter().filter(|kind| kind.as_str() != "workflow").count();
        status.push(s(" · ", "muted"));
        status.push(s(if workflow { "workflow running" } else { "" }, "hot"));
        status.push(s(if workflow && others > 0 { ", " } else { "" }, "muted"));
        status.push(s(
            if others > 0 {
                plural(others, "subagent")
            } else {
                String::new()
            },
            "hot",
        ));
    }
    if shells > 0 {
        status.push(s(format!(" · {}", plural(shells, "background shell")), "muted"));
    }
    body.extend(field("Now", &status, width, 10));
    if !busy.is_empty() {
        body.extend(wrap_line(
            &[s(
                "          no planned hop while these run; only a refusal moves it",
                "muted",
            )],
            width,
            10,
        ));
    }
    let per_turn: Line = match x.cost.five_hour {
        None => vec![s("not enough turns measured yet", "muted")],
        five => vec![
            s(format!("{} of 5h", fixed(five)), "bold"),
            s(" · ", "muted"),
            s(format!("{} of weekly", fixed(x.cost.seven_day)), "bold"),
            s(format!("  ({})", plural(x.turns, "turn")), "muted"),
        ],
    };
    body.extend(field("Per turn", &per_turn, width, 10));
    let burn: Line = match x.burn.five_hour {
        None => vec![s("no recent readings", "muted")],
        five => vec![
            s(format!("{}/h 5h", fixed(five)), "bold"),
            s(" · ", "muted"),
            s(format!("{}/h weekly", fixed(x.burn.seven_day)), "bold"),
        ],
    };
    body.extend(field("Burn", &burn, width, 10));
    body.push(Vec::new());
    body.push(section(
        "Hops",
        if x.hops.is_empty() {
            "none in the last 7 days"
        } else {
            ""
        },
    ));
    for h in &x.hops {
        let at =
            h.at.as_deref()
                .and_then(parse_ms)
                .map_or_else(|| "?".to_owned(), hhmm);
        let reason = h.reason.as_deref().unwrap_or("undefined");
        body.extend(wrap_line(
            &[
                s(format!("  {at} "), "muted"),
                s(if h.from.is_empty() { "?" } else { &h.from }, ""),
                s(format!(" {} ", g.right), "muted"),
                s(if h.to.is_empty() { "?" } else { &h.to }, "bold"),
                s(
                    format!("  {reason}"),
                    if reason == "refusal" { "warn" } else { "muted" },
                ),
            ],
            width,
            8,
        ));
    }
    body
}

/// A session's pane.
pub fn model(state: &State, id: &str, width: usize) -> ViewModel {
    let x = session_by_id(state, id);
    ViewModel {
        title: vec![s(x.map_or("Session", |x| x.name.as_str()), "bold")],
        right: match x {
            Some(x) if x.live => vec![s("live", "ok")],
            Some(_) => vec![s("recent", "muted")],
            None => Vec::new(),
        },
        body: body(session_body(state, id, width)),
    }
}

/// The pane of the recent sessions' fold.
pub fn recent_model(state: &State, width: usize) -> ViewModel {
    let list = recent_sessions(state);
    let mut lines: Vec<Line> = vec![
        section(
            "Recent sessions",
            &format!("{} in the last 7 days, newest first", list.len()),
        ),
        Vec::new(),
    ];
    for x in list {
        let ago = match x.last_at.filter(|at| *at != 0) {
            Some(at) => format!("{} ago", span(Some(state.now - at))),
            None => String::new(),
        };
        lines.extend(wrap_facts(
            &[
                vec![s(x.account_name.as_deref().unwrap_or("?"), "accent")],
                vec![s(ago, "muted")],
                vec![s(plural(x.hops.len(), "hop"), "muted")],
            ],
            width,
            &Facts {
                lead: vec![s(&x.name, "bold"), p("  ")],
                indent: 2,
                ..Facts::default()
            },
        ));
    }
    ViewModel {
        title: vec![s("Recent sessions", "bold")],
        right: Vec::new(),
        body: body(lines),
    }
}
