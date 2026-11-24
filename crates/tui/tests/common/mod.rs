//! A realistic ~/.baton for the menu's frames, written with Baton's own formats: four accounts (acct2
//! preferred, acct4 last resort, acct3 blocked by its weekly limit until a reset in 2 days, acct1
//! blocked by its 5-hour limit until a reset in 40 minutes), two live sessions (one in the middle of a
//! workflow, one counting down to a hop), recent ones, a week of readings, hops and refusals, and the
//! usage poller (this process holding the poll lease, the last poll 12 minutes ago: acct2 with a
//! model's own weekly limit and extra usage, acct3 waiting out the usage endpoint's rate limit, acct4
//! signed out).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, missing_docs)]

pub mod frames;
pub mod vt;

use std::sync::Arc;

use baton_core::clock::{Ms, iso, parse_ms};
use baton_core::fsutil::{append_json_line, write_json_atomic};
use baton_core::paths::{Paths, profile_dir};
use baton_core::theme::GlyphSet;
use baton_core::usage::{Rejection, history_file, record_rejection};
use baton_testkit::write_json;
use baton_tui::data::load_menu_data;
use baton_tui::keys::Key;
use baton_tui::layout::Size;
use baton_tui::reducer::reduce_at;
use baton_tui::state::{Action, Auth, Effect, Init, Outcome, State, initial_state};
use baton_tui::ui::app::frame_text;
use serde_json::{Value, json};

pub const M: Ms = 60_000;
pub const H: Ms = 60 * M;
pub const D: Ms = 24 * H;
pub const S1: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";
pub const S2: &str = "9a8b7c6d-5e4f-4a3b-8c2d-1e0f9a8b7c6d";
const S3: &str = "1b2c3d4e-5f6a-4b7c-8d9e-0f1a2b3c4d5e";
const S4: &str = "2c3d4e5f-6a7b-4c8d-9e0f-1a2b3c4d5e6f";

/// The moment every fixture is built around.
pub fn now() -> Ms {
    parse_ms("2026-10-25T10:00:00Z").unwrap()
}

fn window(used: f64, resets_in: Ms, read_ago: Ms, source: &str) -> Value {
    json!({ "used": used, "resetsAt": iso(now() + resets_in), "readAt": iso(now() - read_ago), "source": source })
}

fn seen(used: f64, resets_in: Ms) -> Value {
    window(used, resets_in, 2 * M, "statusline")
}

fn polled(used: f64, resets_in: Ms) -> Value {
    window(used, resets_in, 12 * M, "poll")
}

/// Writes the fixture home. `all_blocked`: acct2 and acct4 blocked too.
pub fn build_home(paths: &Paths, all_blocked: bool) {
    let now = now();
    let pid = std::process::id();
    write_json(
        &paths.config,
        &json!({
            "version": 1,
            "accounts": [
                { "id": "acct1", "name": "acct1", "mark": "normal" },
                { "id": "acct2", "name": "acct2", "mark": "preferred" },
                { "id": "acct3", "name": "acct3", "mark": "normal" },
                { "id": "acct4", "name": "acct4", "mark": "last-resort", "weekly": 90 }
            ],
            "policy": {}
        }),
    );
    let acct2_five = if all_blocked {
        seen(100.0, 70 * M)
    } else {
        polled(42.0, 2 * H + 5 * M)
    };
    let acct2_week = if all_blocked {
        seen(66.0, 4 * D)
    } else {
        polled(63.0, 4 * D)
    };
    let usage: Vec<(&str, Value, Value)> = vec![
        ("acct1", seen(100.0, 40 * M), seen(71.0, 3 * D + 2 * H)),
        ("acct2", acct2_five.clone(), acct2_week.clone()),
        ("acct3", seen(12.0, 3 * H), window(97.0, 2 * D, 42 * M, "poll")),
        if all_blocked {
            ("acct4", seen(30.0, 4 * H), seen(92.0, 5 * D))
        } else {
            (
                "acct4",
                window(5.0, 4 * H, 42 * M, "statusline"),
                window(18.0, 6 * D, 42 * M, "statusline"),
            )
        },
    ];
    for (name, five, week) in &usage {
        let mut reading = json!({
            "account": name,
            "at": iso(now - 2 * M),
            "source": "statusline",
            "five_hour": five,
            "seven_day": week,
        });
        if *name == "acct2" {
            let extra = json!({
                "limits": [
                    { "kind": "session", "group": "session", "label": null, "used": 42,
                      "resetsAt": iso(now + 2 * H + 5 * M), "severity": "normal", "active": true },
                    { "kind": "weekly_all", "group": "weekly", "label": null, "used": 63,
                      "resetsAt": iso(now + 4 * D), "severity": "normal", "active": false },
                    { "kind": "weekly_scoped", "group": "weekly", "label": "Fable", "used": 71,
                      "resetsAt": iso(now + 4 * D), "severity": "warning", "active": false }
                ],
                "limitsAt": iso(now - 12 * M),
                "extraUsage": { "enabled": true, "monthlyLimit": 5000, "usedCredits": 1250, "utilization": 25, "currency": "USD" }
            });
            reading
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
        }
        write_json_atomic(&paths.usage.join(format!("{name}.json")), &reading).unwrap();
    }
    // The usage poller: this process holds a menu lease, with the last poll 12 minutes ago.
    let poll = paths.root.join("poll");
    write_json_atomic(
        &poll.join("lease.json"),
        &json!({ "pid": pid, "instance": "fixture", "kind": "menu", "since": iso(now - 12 * M), "heartbeatAt": iso(now) }),
    )
    .unwrap();
    let ok = json!({
        "at": iso(now - 12 * M), "status": "ok", "reason": null, "okAt": iso(now - 12 * M),
        "backoffUntil": null, "backoffMs": null, "failures": 0
    });
    let mut signed_out = ok.clone();
    signed_out["status"] = json!("signed-out");
    signed_out["reason"] = json!("not signed in: baton login acct4");
    signed_out["okAt"] = Value::Null;
    write_json_atomic(
        &poll.join("state.json"),
        &json!({
            "lastRunAt": iso(now - 12 * M),
            "running": null,
            "accounts": {
                "acct1": ok,
                "acct2": ok,
                "acct3": {
                    "at": iso(now - 12 * M), "status": "rate-limited",
                    "reason": "Usage endpoint is rate limited. Please try again in a moment.",
                    "okAt": iso(now - 42 * M), "backoffUntil": iso(now + 3 * M), "backoffMs": 15 * M, "failures": 2
                },
                "acct4": signed_out
            }
        }),
    )
    .unwrap();
    let refusal = |window: &str, at: Ms, until: Ms, source: &str, session: &str| Rejection {
        window: window.to_owned(),
        at: Some(iso(at)),
        until: Some(iso(until)),
        until_source: Some(source.to_owned()),
        error: Some("rate_limit".to_owned()),
        session: Some(session.to_owned()),
        ..Rejection::default()
    };
    record_rejection(
        paths,
        "acct1",
        &refusal("five_hour", now - 25 * M, now + 40 * M, "message", S2),
    );
    if all_blocked {
        record_rejection(
            paths,
            "acct2",
            &refusal("five_hour", now - 5 * M, now + 70 * M, "reading", S1),
        );
    }
    // A week of readings: a 5-hour peak and the weekly use each day.
    let days = [6, 5, 4, 3, 2, 1, 0];
    let peaks: [(&str, [f64; 7]); 4] = [
        ("acct1", [55.0, 80.0, 97.0, 64.0, 88.0, 92.0, 100.0]),
        ("acct2", [20.0, 45.0, 70.0, 30.0, 52.0, 61.0, 42.0]),
        ("acct3", [90.0, 95.0, 99.0, 85.0, 92.0, 77.0, 12.0]),
        ("acct4", [0.0, 0.0, 12.0, 0.0, 8.0, 0.0, 5.0]),
    ];
    let weekly: [(&str, [f64; 7]); 4] = [
        ("acct1", [12.0, 25.0, 41.0, 49.0, 58.0, 66.0, 71.0]),
        ("acct2", [10.0, 18.0, 31.0, 38.0, 47.0, 55.0, 63.0]),
        ("acct3", [60.0, 72.0, 83.0, 9.0, 30.0, 70.0, 97.0]),
        ("acct4", [0.0, 0.0, 5.0, 5.0, 11.0, 11.0, 18.0]),
    ];
    for (n, (name, list)) in peaks.iter().enumerate() {
        for (i, ago) in days.iter().enumerate() {
            let at = now - ago * D - 3 * H;
            let file = history_file(paths, "readings", at);
            append_json_line(
                &file,
                &json!({
                    "at": iso(at), "account": name, "window": "five_hour", "used": list[i],
                    "resetsAt": iso(at + 2 * H), "session": S1, "source": "statusline"
                }),
            )
            .unwrap();
            // acct3's weekly window reset 4 days ago with 83% used.
            let resets_at = if *name == "acct3" && i < 3 {
                iso(now - 4 * D + H)
            } else {
                iso(now + 2 * D)
            };
            append_json_line(
                &file,
                &json!({
                    "at": iso(at), "account": name, "window": "seven_day", "used": weekly[n].1[i],
                    "resetsAt": resets_at, "session": S1, "source": "statusline"
                }),
            )
            .unwrap();
        }
    }
    let event = |ago: Ms, mut e: Value| {
        let at = now - ago;
        let mut line = json!({ "at": iso(at) });
        line.as_object_mut().unwrap().append(e.as_object_mut().unwrap());
        append_json_line(&history_file(paths, "events", at), &line).unwrap();
    };
    event(
        5 * D,
        json!({ "type": "session", "session": S4, "account": "acct3", "source": "startup" }),
    );
    event(
        4 * D + H,
        json!({ "type": "hop", "session": S4, "from": "acct3", "to": "acct1", "reason": "planned" }),
    );
    event(
        3 * D,
        json!({ "type": "refusal", "account": "acct3", "window": "seven_day", "session": S4 }),
    );
    event(
        3 * D - M,
        json!({ "type": "hop", "session": S4, "from": "acct3", "to": "acct1", "reason": "refusal" }),
    );
    event(
        D + 2 * H,
        json!({ "type": "session", "session": S3, "account": "acct2", "source": "startup" }),
    );
    event(
        D,
        json!({ "type": "hop", "session": S3, "from": "acct2", "to": "acct4", "reason": "compaction" }),
    );
    event(
        3 * H,
        json!({ "type": "session", "session": S1, "account": "acct1", "source": "startup" }),
    );
    event(
        2 * H + 10 * M,
        json!({ "type": "session", "session": S2, "account": "acct1", "source": "startup" }),
    );
    event(
        2 * H,
        json!({ "type": "refusal", "account": "acct1", "window": "five_hour", "session": S1 }),
    );
    event(
        2 * H - M,
        json!({ "type": "hop", "session": S1, "from": "acct1", "to": "acct2", "reason": "refusal" }),
    );
    event(
        25 * M,
        json!({ "type": "refusal", "account": "acct1", "window": "five_hour", "session": S2 }),
    );
    // Two live sessions (this process stands in for their wrappers).
    let live = |wrapper: &str, id: &str, account: &str, cwd: &str, state: &str, extra: Value| {
        let mut record = json!({
            "pid": pid, "wrapper": wrapper, "account": account, "session": id,
            "startedAt": iso(now - 3 * H), "cwd": cwd, "state": state
        });
        record
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        write_json_atomic(&paths.wrappers.join(format!("{wrapper}.json")), &record).unwrap();
        // It runs: it holds its lock, as a wrapper does, for as long as this process is there. (Its
        // process id alone would not do: this process started long after the record says it did.)
        if let Some(lock) = baton_core::sessions::hold_wrapper(paths, wrapper) {
            std::mem::forget(lock);
        }
        let dir = paths.sessions.join(id);
        write_json_atomic(
            &dir.join("start.json"),
            &json!({
                "at": iso(now - 3 * H), "source": "startup", "account": account, "cwd": cwd,
                "model": "claude-opus-5-5", "wrapper": wrapper
            }),
        )
        .unwrap();
        write_json_atomic(
            &dir.join("live.json"),
            &json!({
                "at": iso(now - M), "account": account, "model": "claude-opus-5-5", "effort": "high",
                "cwd": cwd, "reading": null
            }),
        )
        .unwrap();
    };
    live(
        "1001-aaaa",
        S1,
        "acct2",
        "/work/sports-odds",
        "running",
        json!({}),
    );
    write_json_atomic(
        &paths.sessions.join(S1).join("turn.json"),
        &json!({
            "at": iso(now - 4 * M), "state": "running", "event": "prompt", "account": "acct2", "permissionMode": "auto",
            "backgroundTasks": [
                { "type": "workflow", "status": "running", "name": "monitor sweep" },
                { "type": "subagent", "status": "running", "name": "Explore" },
                { "type": "subagent", "status": "running", "name": "Explore" },
                { "type": "shell", "status": "running", "name": null }
            ]
        }),
    )
    .unwrap();
    for (i, used) in [30.0, 33.0, 35.0, 38.0, 42.0].iter().enumerate() {
        let i = i as Ms;
        append_json_line(
            &paths.sessions.join(S1).join("turns.jsonl"),
            &json!({
                "at": iso(now - (50 - i * 10) * M),
                "account": "acct2",
                "five_hour": { "used": used, "resetsAt": acct2_five["resetsAt"] },
                "seven_day": { "used": 60.0 + i as f64 * 0.5, "resetsAt": acct2_week["resetsAt"] }
            }),
        )
        .unwrap();
    }
    let extra = if all_blocked {
        json!({ "target": "acct1", "until": iso(now + 40 * M) })
    } else {
        json!({ "target": "acct2" })
    };
    live(
        "1002-bbbb",
        S2,
        "acct1",
        "/work/pelican",
        if all_blocked { "waiting" } else { "countdown" },
        extra,
    );
    write_json_atomic(
        &paths.sessions.join(S2).join("turn.json"),
        &json!({
            "at": iso(now - 25 * M), "state": "idle", "event": "stop-failure", "account": "acct1",
            "error": "rate_limit", "window": "five_hour"
        }),
    )
    .unwrap();
    // Recent, ended sessions.
    for (id, account, cwd, ago) in [
        (S3, "acct4", "/work/royal-flush", 20 * H),
        (S4, "acct1", "/work/baton", 3 * D),
    ] {
        let dir = paths.sessions.join(id);
        write_json_atomic(
            &dir.join("start.json"),
            &json!({ "at": iso(now - ago - H), "source": "startup", "account": account, "cwd": cwd }),
        )
        .unwrap();
        write_json_atomic(
            &dir.join("end.json"),
            &json!({ "at": iso(now - ago), "reason": "prompt_input_exit", "account": account }),
        )
        .unwrap();
    }
    for name in ["acct2", "acct3", "acct4"] {
        write_json(&profile_dir(paths, name).join(".claude.json"), &json!({}));
    }
}

/// The sign-ins the frames are drawn with: acct4 signed out.
pub fn auth() -> Vec<(String, Auth)> {
    ["acct1", "acct2", "acct3", "acct4"]
        .iter()
        .map(|name| {
            let signed_in = *name != "acct4";
            let method = if signed_in { "claude.ai" } else { "none" };
            (
                (*name).to_owned(),
                Auth {
                    logged_in: Some(signed_in),
                    auth_method: Some(method.to_owned()),
                    error: None,
                },
            )
        })
        .collect()
}

/// What goes into the reducer in a test: a key by its name, or an action.
#[derive(Clone, Debug)]
pub enum Input {
    Key(&'static str),
    Action(Action),
}

impl From<&'static str> for Input {
    fn from(name: &'static str) -> Input {
        Input::Key(name)
    }
}

impl From<Action> for Input {
    fn from(action: Action) -> Input {
        Input::Action(action)
    }
}

/// Typing or pasting several characters at once.
pub fn text(typed: &str) -> Input {
    Input::Action(Action::Text(typed.to_owned()))
}

/// The menu's state on the fixture home, after `keys`.
pub fn menu_state_with(paths: &Paths, size: Size, keys: &[&'static str], glyphs: GlyphSet) -> State {
    let mut state = initial_state(Init {
        size,
        now: now(),
        color: false,
        glyphs,
        ..Init::default()
    });
    state.auth = auth().into_iter().collect();
    state = reduce_at(
        &state,
        now(),
        Action::Data(Arc::new(load_menu_data(paths, now()))),
    )
    .state;
    for key in keys {
        state = press(&state, paths, *key).state;
    }
    state
}

/// The menu's state on the fixture home at 120 x 30, after `keys`.
pub fn menu_state(paths: &Paths, keys: &[&'static str]) -> State {
    menu_state_with(paths, Size { cols: 120, rows: 30 }, keys, GlyphSet::Rich)
}

/// One key or action; a reload it asks for is carried out.
pub fn press(state: &State, paths: &Paths, input: impl Into<Input>) -> Outcome {
    let action = match input.into() {
        Input::Key(name) => Action::Key(Key::from_name(name)),
        Input::Action(action) => action,
    };
    let out = reduce_at(state, now(), action);
    if out.effects.contains(&Effect::Reload) {
        let data = Arc::new(load_menu_data(paths, now()));
        return Outcome {
            state: reduce_at(&out.state, now(), Action::Data(data)).state,
            effects: out.effects,
        };
    }
    out
}

/// A frame as text rows, the spinners at their first frame.
pub fn frame(state: &State) -> Vec<String> {
    frame_text(state, 0)
}

/// The sizes the frames are checked at, by name.
pub fn sizes() -> Vec<(&'static str, Size)> {
    vec![
        ("80x24", Size { cols: 80, rows: 24 }),
        ("100x30", Size { cols: 100, rows: 30 }),
        ("120x30", Size { cols: 120, rows: 30 }),
        ("160x45", Size { cols: 160, rows: 45 }),
    ]
}

/// The size of a name.
pub fn size(name: &str) -> Size {
    sizes()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, size)| size)
        .unwrap()
}

/// Each screen: the keys from a fresh menu (Overview selected).
pub fn screens() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("overview", vec![]),
        ("account", vec!["down", "down"]),
        ("account-blocked", vec!["down", "down", "down"]),
        ("session", vec!["down", "down", "down", "down", "down"]),
        ("policy", vec!["end", "up", "enter"]),
        ("stats", vec!["end"]),
        ("actions", vec!["down", "down", "enter"]),
        ("filter", vec!["/", "a", "c", "c", "t", "3"]),
        ("thresholds", vec!["down", "down", "enter", "t"]),
        ("remove", vec!["down", "down", "down", "enter", "d"]),
        ("remove-account-1", vec!["down", "enter", "d"]),
        ("rename", vec!["down", "down", "enter", "e"]),
        (
            "hop-to",
            vec!["down", "down", "down", "down", "down", "enter", "H"],
        ),
    ]
}

/// The keys of a screen.
pub fn screen(name: &str) -> Vec<&'static str> {
    screens()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, keys)| keys)
        .unwrap()
}

/// The state of a screen at a size.
pub fn screen_state(paths: &Paths, size_name: &str, screen_name: &str) -> State {
    menu_state_with(paths, size(size_name), &screen(screen_name), GlyphSet::Rich)
}
