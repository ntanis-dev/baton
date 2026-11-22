//! The hook handlers and the status line over a temporary home: what each event records, what the
//! status line prints, and that bad input never fails a session.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::fs;
use std::path::Path;
use std::time::Instant;

use baton_core::clock::{DAY, HOUR, MINUTE, Ms, iso, now_ms};
use baton_core::config::load_config;
use baton_core::env::Env;
use baton_core::fsutil::{read_json, read_json_lines};
use baton_core::hop::resolve_session;
use baton_core::paths::{Paths, account_from_env, forward, profile_dir};
use baton_core::sessions::{read_session_file, read_turn_samples, wrapper_file, wrapper_session_file};
use baton_core::term::make_terminal;
use baton_core::theme::GlyphSet;
use baton_core::usage::{Reading, history_file, load_rejections, read_reading};
use baton_core::window::PerWindow;
use baton_install::hooks::{
    Cell, LineOptions, Overall, StatusLine, append_usage, columns_of, handle_hook, handle_status_line,
    overall_usage, own_status_line, run_previous_status_line, status_line_shell,
};
use baton_testkit::{accounts_home, four_accounts_home, test_env, write_json};
use regex::Regex;
use serde_json::{Value, json};

const SID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";

/// The environment of a claude running under acct2's profile, and nothing else.
fn env2(paths: &Paths, extra: &[(&str, &str)]) -> Env {
    let mut env = Env::new();
    env.set("CLAUDE_CONFIG_DIR", profile_dir(paths, "acct2").into_os_string());
    for (name, value) in extra {
        env.set(*name, *value);
    }
    env
}

/// For a status line as plain text, with the rich glyphs.
fn plain_env(paths: &Paths, extra: &[(&str, &str)]) -> Env {
    let mut env = env2(paths, &[("NO_COLOR", "1"), ("BATON_GLYPHS", "rich")]);
    for (name, value) in extra {
        env.set(*name, *value);
    }
    env
}

/// Every file under `dir`, by its path from there with forward slashes.
fn all_files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(forward(path.strip_prefix(base).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

/// Baton's own line, when that is what the status line printed.
fn text_of(line: StatusLine) -> String {
    match line {
        StatusLine::Text(text) => text,
        other => panic!("not Baton's own line: {other:?}"),
    }
}

/// A stored reading from JSON.
fn reading(value: Value) -> Reading {
    serde_json::from_value(value).unwrap()
}

/// The seconds Claude Code writes a reset as.
fn seconds(at: Ms) -> i64 {
    at / 1000
}

#[test]
fn the_account_comes_from_claude_config_dir_a_baton_profile_is_its_id_plain_claude_and_any_other_folder_are_none()
 {
    let home = four_accounts_home();
    let paths = &home.paths;
    let at =
        |dir: std::ffi::OsString| account_from_env(&Env::from_pairs([("CLAUDE_CONFIG_DIR", dir)]), paths);
    assert_eq!(account_from_env(&Env::new(), paths), None);
    assert_eq!(at(paths.claude_dir.clone().into_os_string()), None);
    assert_eq!(
        at(profile_dir(paths, "acct2").into_os_string()).as_deref(),
        Some("acct2")
    );
    assert_eq!(
        at(format!("{}/", forward(profile_dir(paths, "acct2"))).into()).as_deref(),
        Some("acct2")
    );
    assert_eq!(at(home.home.join("elsewhere").into_os_string()), None);
    // "default" is an ordinary id now: an account the operator named so.
    assert_eq!(
        at(paths.accounts.join("default").into_os_string()).as_deref(),
        Some("default")
    );
}

#[test]
fn session_start_prompt_stop_and_session_end_are_recorded_for_the_session_and_its_wrapper() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let env = env2(paths, &[("BATON_WRAPPER", "123-abcd"), ("CLAUDE_PID", "4242")]);
    let now = now_ms();
    let recorded = handle_hook(
        "session-start",
        &json!({ "session_id": SID, "source": "startup", "cwd": "E:/w", "transcript_path": "E:/t.jsonl", "model": "m" }),
        &env,
        paths,
        now,
    );
    assert_eq!(recorded, ["start", "wrapper"]);
    let start = read_session_file(paths, SID, "start.json").unwrap();
    assert_eq!(
        start,
        json!({
            "at": iso(now),
            "source": "startup",
            "account": "acct2",
            "cwd": "E:/w",
            "transcriptPath": "E:/t.jsonl",
            "model": "m",
            "wrapper": "123-abcd"
        })
    );
    // With the pid of the claude that wrote it: the wrapper follows only its own.
    let mapped = read_json(&wrapper_session_file(paths, "123-abcd")).unwrap();
    assert_eq!(
        mapped,
        json!({ "session": SID, "at": iso(now), "source": "startup", "account": "acct2", "pid": 4242 })
    );
    let recorded = handle_hook(
        "prompt",
        &json!({ "session_id": SID, "permission_mode": "acceptEdits", "prompt": "hi" }),
        &env,
        paths,
        now,
    );
    assert_eq!(recorded, ["turn"]);
    assert_eq!(
        read_session_file(paths, SID, "turn.json").unwrap(),
        json!({ "at": iso(now), "state": "running", "event": "prompt", "account": "acct2", "permissionMode": "acceptEdits" })
    );
    // The status line's reading becomes the turn-end sample.
    handle_status_line(
        &json!({
            "session_id": SID,
            "rate_limits": { "five_hour": { "used_percentage": 40, "resets_at": seconds(now + HOUR) } }
        }),
        &env,
        paths,
        now,
    );
    let recorded = handle_hook(
        "stop",
        &json!({
            "session_id": SID,
            "permission_mode": "acceptEdits",
            "background_tasks": [{ "id": "1", "type": "subagent", "status": "running", "agent_type": "Explore" }]
        }),
        &env,
        paths,
        now,
    );
    assert_eq!(recorded, ["turn", "sample"]);
    let turn = read_session_file(paths, SID, "turn.json").unwrap();
    assert_eq!(turn["state"], "idle");
    assert_eq!(
        turn["backgroundTasks"],
        json!([{ "type": "subagent", "status": "running", "name": "Explore" }])
    );
    let samples = read_turn_samples(paths, SID);
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].windows.five_hour.as_ref().unwrap().used, 40.0);
    assert_eq!(samples[0].account.as_deref(), Some("acct2"));
    let recorded = handle_hook(
        "session-start",
        &json!({ "session_id": SID, "source": "compact" }),
        &env,
        paths,
        now,
    );
    assert_eq!(recorded, ["start", "wrapper", "compact"]);
    assert_eq!(
        read_session_file(paths, SID, "compact.json"),
        Some(json!({ "at": iso(now) }))
    );
    let recorded = handle_hook(
        "session-end",
        &json!({ "session_id": SID, "reason": "prompt_input_exit" }),
        &env,
        paths,
        now,
    );
    assert_eq!(recorded, ["end"]);
    assert_eq!(
        read_session_file(paths, SID, "end.json").unwrap(),
        json!({ "at": iso(now), "reason": "prompt_input_exit", "account": "acct2" })
    );
    // The sessions of the conversation are in the history, with their wrapper.
    let events = read_json_lines(&history_file(paths, "events", now));
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[0],
        json!({ "at": iso(now), "type": "session", "session": SID, "account": "acct2", "source": "startup", "wrapper": "123-abcd" })
    );
    assert_eq!(events[1]["source"], "compact");
    assert_eq!(
        all_files(&paths.root)
            .into_iter()
            .filter(|f| f.ends_with(".tmp"))
            .count(),
        0
    );
}

#[test]
fn a_usage_refusal_records_the_account_as_rejected_for_its_window_with_the_session() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let env = env2(paths, &[]);
    let now = now_ms();
    let resets = seconds(now) + 3600;
    handle_status_line(
        &json!({
            "session_id": SID,
            "rate_limits": {
                "five_hour": { "used_percentage": 100, "resets_at": resets },
                "seven_day": { "used_percentage": 50, "resets_at": resets + 86400 }
            }
        }),
        &env,
        paths,
        now,
    );
    let recorded = handle_hook(
        "stop-failure",
        &json!({
            "session_id": SID,
            "error": "rate_limit",
            "error_details": "429 Too Many Requests",
            "last_assistant_message": "API Error: Rate limit reached"
        }),
        &env,
        paths,
        now,
    );
    assert_eq!(recorded, ["rejection", "turn"]);
    let ids = ["acct2".to_owned()];
    let all = load_rejections(paths, &ids);
    let r = all["acct2"].five_hour.as_ref().unwrap();
    assert_eq!(r.session.as_deref(), Some(SID));
    assert_eq!(r.error.as_deref(), Some("rate_limit"));
    assert_eq!(r.until, Some(iso(resets * 1000)));
    assert_eq!(r.until_source.as_deref(), Some("reading"));
    assert_eq!(r.message.as_deref(), Some("API Error: Rate limit reached"));
    assert_eq!(r.at, Some(iso(now)));
    let turn = read_session_file(paths, SID, "turn.json").unwrap();
    assert_eq!(
        turn,
        json!({
            "at": iso(now),
            "state": "idle",
            "event": "stop-failure",
            "account": "acct2",
            "error": "rate_limit",
            "window": "five_hour",
            "until": iso(resets * 1000),
            "message": "API Error: Rate limit reached"
        })
    );
    let events = read_json_lines(&history_file(paths, "events", now));
    assert_eq!(
        events,
        [json!({
            "at": iso(now),
            "type": "refusal",
            "session": SID,
            "account": "acct2",
            "error": "rate_limit",
            "window": "five_hour",
            "until": iso(resets * 1000)
        })]
    );
    // Another API error ends the turn but is no refusal.
    let recorded = handle_hook(
        "stop-failure",
        &json!({ "session_id": SID, "error": "server_error" }),
        &env,
        paths,
        now,
    );
    assert_eq!(recorded, ["turn"]);
    let turn = read_session_file(paths, SID, "turn.json").unwrap();
    assert_eq!(turn["error"], "server_error");
    assert_eq!(
        (&turn["window"], &turn["until"], &turn["message"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );
    let after = &load_rejections(paths, &ids)["acct2"];
    assert!(after.five_hour.is_some() && after.seven_day.is_none() && after.unknown.is_none());
}

#[test]
fn a_refusal_names_its_window_and_its_reset_from_what_claude_code_said() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let env = env2(paths, &[]);
    let now = now_ms();
    // No reading at all, and details that are not text: they are read as their JSON.
    handle_hook(
        "stop-failure",
        &json!({
            "session_id": SID,
            "error": "rate_limit",
            "error_details": { "note": "weekly limit reached" },
            "last_assistant_message": "resets in 2h 30m"
        }),
        &env,
        paths,
        now,
    );
    let ids = ["acct2".to_owned()];
    let r = load_rejections(paths, &ids)["acct2"].seven_day.clone().unwrap();
    assert_eq!(r.until, Some(iso(now + 150 * MINUTE)));
    assert_eq!(r.until_source.as_deref(), Some("message"));
    // An error that is not text is "unknown", and no refusal.
    handle_hook(
        "stop-failure",
        &json!({ "session_id": SID, "error": { "nested": true } }),
        &env,
        paths,
        now,
    );
    assert_eq!(
        read_session_file(paths, SID, "turn.json").unwrap()["error"],
        "unknown"
    );
    // A long message is kept to its first 400 characters.
    let long = "x".repeat(900);
    handle_hook(
        "stop-failure",
        &json!({ "session_id": SID, "error": "billing_error", "last_assistant_message": long }),
        &env,
        paths,
        now,
    );
    let unknown = load_rejections(paths, &ids)["acct2"].unknown.clone().unwrap();
    assert_eq!(unknown.message.unwrap().len(), 400);
    assert_eq!(unknown.until, None);
}

#[test]
fn bad_input_never_fails_and_writes_nothing() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let env = env2(paths, &[]);
    let now = now_ms();
    let inputs = [
        json!(null),
        json!(7),
        json!("x"),
        json!([]),
        json!({}),
        json!({ "session_id": "../../evil" }),
        json!({ "session_id": "" }),
        json!({ "session_id": 3 }),
        json!({ "session_id": SID, "error": { "nested": true }, "error_details": { "a": 1 } }),
    ];
    for input in &inputs {
        for event in [
            "session-start",
            "prompt",
            "stop",
            "stop-failure",
            "session-end",
            "unknown-event",
        ] {
            let recorded = handle_hook(event, input, &env, paths, now);
            if input["session_id"] != SID || event == "unknown-event" {
                assert!(recorded.is_empty(), "{event} {input}");
            }
        }
    }
    for input in [
        json!(null),
        json!([1, 2]),
        json!("x"),
        json!({ "session_id": 5, "rate_limits": "x" }),
    ] {
        assert_eq!(
            handle_status_line(&input, &plain_env(paths, &[]), paths, now),
            StatusLine::Text("\u{257a}\u{2501}\u{2578} acct2".to_owned())
        );
    }
    assert!(!paths.root.join("..").join("..").join("evil").exists());
    assert_eq!(
        all_files(&paths.sessions)
            .into_iter()
            .filter(|f| !f.starts_with(SID))
            .count(),
        0
    );
    assert!(!paths.usage.exists(), "nothing was read, so nothing is recorded");
    // Not one of Baton's profiles: nothing at all.
    let other = "7a0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";
    let foreign = Env::from_pairs([("CLAUDE_CONFIG_DIR", home.home.join("not-baton").into_os_string())]);
    assert!(handle_hook("prompt", &json!({ "session_id": other }), &foreign, paths, now).is_empty());
    assert!(!paths.sessions.join(other).exists());
    assert_eq!(
        handle_status_line(&json!({ "session_id": other }), &foreign, paths, now),
        StatusLine::Text(String::new())
    );
    // A profile folder of an account that was removed is not Baton's either.
    let removed = Env::from_pairs([("CLAUDE_CONFIG_DIR", paths.accounts.join("gone").into_os_string())]);
    assert!(handle_hook("prompt", &json!({ "session_id": other }), &removed, paths, now).is_empty());
    assert!(!paths.sessions.join(other).exists());
}

#[test]
fn hooks_are_fast() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let env = env2(paths, &[]);
    let started = Instant::now();
    for _ in 0..20 {
        handle_hook(
            "prompt",
            &json!({ "session_id": SID, "permission_mode": "default" }),
            &env,
            paths,
            now_ms(),
        );
        handle_hook("stop", &json!({ "session_id": SID }), &env, paths, now_ms());
    }
    let each = started.elapsed().as_secs_f64() * 1000.0 / 40.0;
    assert!(each < 25.0, "{each} ms per event");
}

#[test]
fn the_status_line_records_the_reading_and_prints_batons_own_line_when_there_was_none() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let env = plain_env(paths, &[("BATON_WRAPPER", "9-ffff")]);
    let now = now_ms();
    let resets = seconds(now) + 3600;
    let out = handle_status_line(
        &json!({
            "session_id": SID,
            "model": { "id": "claude-opus-5-5" },
            "effort": { "level": "high" },
            "workspace": { "current_dir": "E:/w" },
            "rate_limits": {
                "five_hour": { "used_percentage": 42.4, "resets_at": resets },
                "seven_day": { "used_percentage": 63, "resets_at": resets + 86400 }
            }
        }),
        &env,
        paths,
        now,
    );
    // The baton mark, the account, its windows; the other accounts have no reading yet, so there is
    // nothing to add for them.
    assert_eq!(
        text_of(out),
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 42% \u{b7} week 63%"
    );
    let stored = read_reading(paths, "acct2").unwrap();
    assert_eq!(stored.five_hour.as_ref().unwrap().used, 42.4);
    assert_eq!(stored.session.as_deref(), Some(SID));
    let live = read_session_file(paths, SID, "live.json").unwrap();
    assert_eq!(
        live,
        json!({
            "at": iso(now),
            "account": "acct2",
            "model": "claude-opus-5-5",
            "effort": "high",
            "cwd": "E:/w",
            "reading": {
                "five_hour": { "used": 42.4, "resetsAt": iso(resets * 1000) },
                "seven_day": { "used": 63, "resetsAt": iso((resets + 86400) * 1000) }
            }
        })
    );
    let mapped = read_json(&wrapper_session_file(paths, "9-ffff")).unwrap();
    assert_eq!(
        mapped,
        json!({ "session": SID, "at": iso(now), "source": "statusline", "account": "acct2", "pid": null })
    );
    assert_eq!(all_files(&paths.history).len(), 1);
    assert_eq!(read_json_lines(&history_file(paths, "readings", now)).len(), 2);
    // The same again a moment later: nothing is written a second time.
    let again = json!({ "session_id": SID, "model": { "id": "claude-opus-5-5" }, "effort": { "level": "high" }, "cwd": "E:/w",
        "rate_limits": { "five_hour": { "used_percentage": 42.4, "resets_at": resets }, "seven_day": { "used_percentage": 63, "resets_at": resets + 86400 } } });
    handle_status_line(&again, &env, paths, now + 5000);
    assert_eq!(
        read_session_file(paths, SID, "live.json").unwrap()["at"],
        json!(iso(now))
    );
    assert_eq!(read_json_lines(&history_file(paths, "readings", now)).len(), 2);
    assert_eq!(
        own_status_line(
            Some("a"),
            Some(&reading(
                json!({ "five_hour": { "used": 99, "resetsAt": iso(now - 1) } })
            )),
            now,
            &LineOptions::default()
        ),
        "\u{257a}\u{2501}\u{2578} a \u{b7} 5h reset"
    );
    assert_eq!(own_status_line(None, None, now, &LineOptions::default()), "");
    assert_eq!(own_status_line(Some(""), None, now, &LineOptions::default()), "");
}

/// A reading for an account: used % in each window, resetting in an hour and in three days.
fn reading_of(now: Ms, five: f64, week: f64) -> Value {
    json!({
        "five_hour": { "used": five, "resetsAt": iso(now + HOUR), "readAt": iso(now - MINUTE) },
        "seven_day": { "used": week, "resetsAt": iso(now + 3 * DAY), "readAt": iso(now - MINUTE) }
    })
}

#[test]
fn the_status_line_draws_all_the_accounts_a_bar_each_and_the_mean_use_of_those_with_a_reading() {
    let home = four_accounts_home(); // acct1..acct4, this one is acct2
    let paths = &home.paths;
    let now = now_ms();
    write_json(&paths.usage.join("acct1.json"), &reading_of(now, 20.0, 40.0));
    write_json(&paths.usage.join("acct3.json"), &reading_of(now, 60.0, 80.0));
    // acct4 was never measured: left out of the mean, not counted as empty. acct2's stored reading
    // is old; the live one passed in is what counts.
    write_json(&paths.usage.join("acct2.json"), &reading_of(now, 5.0, 5.0));
    let config = load_config(paths);
    let live = reading(reading_of(now, 42.0, 63.0));
    let all = overall_usage(paths, &config, "acct2", Some(&live), now);
    assert_eq!(all.count, 4);
    assert_eq!(
        all.cells.five_hour.as_deref().unwrap(),
        [
            Cell {
                used: Some(20.0),
                limit: 90.0,
                current: false
            },
            Cell {
                used: Some(42.0),
                limit: 90.0,
                current: true
            },
            Cell {
                used: Some(60.0),
                limit: 90.0,
                current: false
            },
            Cell {
                used: None,
                limit: 90.0,
                current: false
            },
        ]
    );
    let weekly: Vec<Option<f64>> = all
        .cells
        .seven_day
        .as_deref()
        .unwrap()
        .iter()
        .map(|c| c.used)
        .collect();
    assert_eq!(weekly, [Some(40.0), Some(63.0), Some(80.0), None]);
    assert_eq!(all.means.five_hour.unwrap().round(), 41.0); // (20 + 42 + 60) / 3
    assert_eq!(all.means.seven_day, Some(61.0)); // (40 + 63 + 80) / 3
    // A window past its reset is empty again.
    let mut reset = reading_of(now, 60.0, 80.0);
    reset["five_hour"] = json!({ "used": 60, "resetsAt": iso(now - 1), "readAt": iso(now - HOUR) });
    write_json(&paths.usage.join("acct3.json"), &reset);
    let after = overall_usage(paths, &config, "acct2", Some(&live), now);
    assert_eq!(after.means.five_hour.unwrap().round(), 21.0); // (20 + 42 + 0) / 3
    assert_eq!(after.means.seven_day, Some(61.0));
    // The live reading moves the figure at once; the others' stored ones do not.
    let busier = overall_usage(
        paths,
        &config,
        "acct2",
        Some(&reading(reading_of(now, 90.0, 63.0))),
        now,
    );
    assert_eq!(busier.means.five_hour.unwrap().round(), 37.0); // (20 + 90 + 0) / 3

    let input = json!({
        "session_id": SID,
        "rate_limits": {
            "five_hour": { "used_percentage": 42, "resets_at": seconds(now) + 3600 },
            "seven_day": { "used_percentage": 63, "resets_at": seconds(now) + 86400 }
        }
    });
    let line =
        |extra: &[(&str, &str)]| text_of(handle_status_line(&input, &plain_env(paths, extra), paths, now));
    // One bar per account, as tall as its use (acct1 20%, acct2 the live 42%, acct3 reset, acct4
    // never measured: a dot), then the mean.
    assert_eq!(
        line(&[]),
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 42% \u{b7} week 63% \u{2503} 5h \u{2582}\u{2584}\u{2581}\u{b7} 21% \u{b7} week \u{2584}\u{2585}\u{2587}\u{b7} 61%"
    );
    // The classic console's glyphs; a narrow terminal drops the overall part first, then the windows.
    assert_eq!(
        line(&[("BATON_GLYPHS", "plain")]),
        "=#= acct2 \u{b7} 5h 42% \u{b7} week 63% | 5h .-_. 21% \u{b7} week -=*. 61%"
    );
    assert_eq!(
        line(&[("COLUMNS", "40")]),
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 42% \u{b7} week 63%"
    );
    assert_eq!(line(&[("COLUMNS", "12")]), "\u{257a}\u{2501}\u{2578} acct2");
}

/// A reading for an account whose plan has no 5-hour limit: its last poll listed the week alone, and
/// what is left of an older 5-hour reading says nothing.
fn weekly_only(now: Ms, week: f64) -> Value {
    json!({
        "five_hour": { "used": 0, "resetsAt": null, "readAt": iso(now - 2 * DAY), "source": "poll" },
        "seven_day": { "used": week, "resetsAt": iso(now + 3 * DAY), "readAt": iso(now - MINUTE) },
        "limits": [{
            "kind": "weekly_all", "group": "weekly", "label": null, "used": week,
            "resetsAt": iso(now + 3 * DAY), "severity": "normal", "active": false,
        }],
        "limitsAt": iso(now - HOUR)
    })
}

#[test]
fn a_plan_with_no_five_hour_limit_has_no_5h_in_the_status_line_nor_has_the_overall_part_when_no_account_has_one()
 {
    let home = four_accounts_home(); // acct1..acct4, this one is acct2
    let paths = &home.paths;
    let now = now_ms();
    write_json(&paths.usage.join("acct1.json"), &weekly_only(now, 40.0));
    write_json(&paths.usage.join("acct2.json"), &weekly_only(now, 60.0));
    write_json(&paths.usage.join("acct3.json"), &reading_of(now, 60.0, 80.0));
    // Claude Code reports the week alone on such a plan.
    let input = json!({
        "session_id": SID,
        "rate_limits": { "seven_day": { "used_percentage": 63, "resets_at": seconds(now) + 86400 } }
    });
    let line = || text_of(handle_status_line(&input, &plain_env(paths, &[]), paths, now));
    // This account has no 5h of its own. Together: the accounts that have the limit, acct3 and
    // acct4 (never measured, so nothing says it has none).
    assert_eq!(
        line(),
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} week 63% \u{2503} 5h \u{2585}\u{b7} 60% \u{b7} week \u{2584}\u{2585}\u{2587}\u{b7} 61%"
    );
    let config = load_config(paths);
    let stored = read_reading(paths, "acct2");
    let all = overall_usage(paths, &config, "acct2", stored.as_ref(), now);
    let five: Vec<Option<f64>> = all
        .cells
        .five_hour
        .as_deref()
        .unwrap()
        .iter()
        .map(|c| c.used)
        .collect();
    assert_eq!(five, [Some(60.0), None]);
    assert_eq!(all.means.five_hour, Some(60.0));
    assert_eq!(all.cells.seven_day.as_deref().unwrap().len(), 4);
    // No account with the limit has a reading: no 5h together either.
    write_json(&paths.usage.join("acct3.json"), &weekly_only(now, 80.0));
    assert_eq!(
        line(),
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} week 63% \u{2503} week \u{2584}\u{2585}\u{2587}\u{b7} 61%"
    );
    // No account has the limit at all.
    write_json(&paths.usage.join("acct4.json"), &weekly_only(now, 20.0));
    assert_eq!(
        line(),
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} week 63% \u{2503} week \u{2584}\u{2585}\u{2587}\u{2582} 51%"
    );
    let stored = read_reading(paths, "acct2");
    let none = overall_usage(paths, &config, "acct2", stored.as_ref(), now);
    assert!(none.cells.five_hour.as_deref().unwrap().is_empty());
    assert_eq!(none.means.five_hour, None);
    // A session that reports the window again has the limit after all.
    let back = json!({
        "session_id": SID,
        "rate_limits": {
            "five_hour": { "used_percentage": 42, "resets_at": seconds(now) + 3600 },
            "seven_day": { "used_percentage": 63, "resets_at": seconds(now) + 86400 }
        }
    });
    let shown = text_of(handle_status_line(&back, &plain_env(paths, &[]), paths, now));
    assert!(
        shown.starts_with("\u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 42% \u{b7} week 63%"),
        "{shown}"
    );
}

#[test]
fn the_overall_bars_one_account_is_one_bar_and_with_no_one_to_compare_with_nothing_is_added() {
    let home = accounts_home(&["a", "b"]);
    let paths = &home.paths;
    let now = now_ms();
    let own = reading(json!({ "five_hour": { "used": 30, "resetsAt": iso(now + HOUR) } }));
    let line = |overall: &Overall| {
        own_status_line(
            Some("a"),
            Some(&own),
            now,
            &LineOptions {
                overall: Some(overall),
                ..LineOptions::default()
            },
        )
    };
    let means = PerWindow {
        five_hour: Some(43.0),
        seven_day: None,
    };
    let cells = Overall {
        count: 2,
        means,
        cells: PerWindow {
            five_hour: Some(vec![
                Cell {
                    used: Some(30.0),
                    limit: 90.0,
                    current: true,
                },
                Cell {
                    used: Some(55.0),
                    limit: 90.0,
                    current: false,
                },
            ]),
            seven_day: Some(vec![
                Cell {
                    used: None,
                    limit: 95.0,
                    current: true,
                },
                Cell {
                    used: None,
                    limit: 95.0,
                    current: false,
                },
            ]),
        },
    };
    assert_eq!(
        line(&cells),
        "\u{257a}\u{2501}\u{2578} a \u{b7} 5h 30% \u{2503} 5h \u{2583}\u{2585} 43%"
    );
    // Without the per-account cells, the mean alone.
    let bare = Overall {
        count: 2,
        means,
        cells: PerWindow::default(),
    };
    assert_eq!(
        line(&bare),
        "\u{257a}\u{2501}\u{2578} a \u{b7} 5h 30% \u{2503} 5h 43%"
    );
    // Nothing to compare with: one account, or no reading at all.
    for overall in [
        Overall {
            count: 1,
            means: PerWindow {
                five_hour: Some(30.0),
                seven_day: None,
            },
            cells: PerWindow::default(),
        },
        Overall {
            count: 2,
            means: PerWindow::default(),
            cells: PerWindow::default(),
        },
    ] {
        assert_eq!(line(&overall), "\u{257a}\u{2501}\u{2578} a \u{b7} 5h 30%");
    }
    // Only this account has a reading: the overall figure would only repeat its own.
    let alone = Overall {
        count: 2,
        means: PerWindow {
            five_hour: Some(30.0),
            seven_day: None,
        },
        cells: PerWindow {
            five_hour: Some(vec![
                Cell {
                    used: Some(30.0),
                    limit: 90.0,
                    current: true,
                },
                Cell {
                    used: None,
                    limit: 90.0,
                    current: false,
                },
            ]),
            seven_day: None,
        },
    };
    assert_eq!(line(&alone), "\u{257a}\u{2501}\u{2578} a \u{b7} 5h 30%");
    write_json(&paths.usage.join("b.json"), &json!({}));
    assert_eq!(
        overall_usage(paths, &load_config(paths), "a", None, now)
            .means
            .five_hour,
        None
    );
}

#[test]
fn this_accounts_bar_is_underlined_the_others_are_not() {
    let now = now_ms();
    let own = reading(json!({ "five_hour": { "used": 30, "resetsAt": iso(now + HOUR) } }));
    let overall = Overall {
        count: 2,
        means: PerWindow {
            five_hour: Some(43.0),
            seven_day: None,
        },
        cells: PerWindow {
            five_hour: Some(vec![
                Cell {
                    used: Some(30.0),
                    limit: 90.0,
                    current: true,
                },
                Cell {
                    used: Some(55.0),
                    limit: 90.0,
                    current: false,
                },
            ]),
            seven_day: None,
        },
    };
    let out = own_status_line(
        Some("a"),
        Some(&own),
        now,
        &LineOptions {
            term: Some(make_terminal(false, 24, GlyphSet::Rich, 0)),
            overall: Some(&overall),
            ..LineOptions::default()
        },
    );
    // Underline (4) on the first bar only.
    assert!(
        Regex::new("\x1b\\[4;[0-9;]+m\u{2583}").unwrap().is_match(&out),
        "{out:?}"
    );
    assert!(
        !Regex::new("\x1b\\[4;[0-9;]+m\u{2585}").unwrap().is_match(&out),
        "{out:?}"
    );
    assert!(
        Regex::new("\x1b\\[[0-9;]+m\u{2585}").unwrap().is_match(&out),
        "{out:?}"
    );
}

#[test]
fn the_status_line_is_coloured_like_the_menu_where_the_terminal_can_show_it_plain_where_not() {
    let now = now_ms();
    let own = reading(json!({ "five_hour": { "used": 95, "resetsAt": iso(now + HOUR) } }));
    let line = |depth: u8| {
        own_status_line(
            Some("a"),
            Some(&own),
            now,
            &LineOptions {
                term: Some(make_terminal(false, depth, GlyphSet::Rich, 0)),
                ..LineOptions::default()
            },
        )
    };
    let true24 = line(24);
    // The mark in the brand colour (#e4bc89), a use past its threshold in red.
    assert!(
        true24.contains("\x1b[1;38;2;228;188;137m\u{257a}\u{2501}\u{2578}"),
        "{true24:?}"
    );
    assert!(true24.contains("\x1b[38;2;207;139;130m95%"), "{true24:?}");
    let bare = Regex::new("\x1b\\[[0-9;]*m")
        .unwrap()
        .replace_all(&true24, "")
        .into_owned();
    assert_eq!(bare, "\u{257a}\u{2501}\u{2578} a \u{b7} 5h 95%");
    assert!(!line(1).contains('\x1b'));
    // The account's own thresholds colour its figures.
    let lenient = own_status_line(
        Some("a"),
        Some(&own),
        now,
        &LineOptions {
            term: Some(make_terminal(false, 24, GlyphSet::Rich, 0)),
            limits: PerWindow {
                five_hour: 100.0,
                seven_day: 100.0,
            },
            ..LineOptions::default()
        },
    );
    assert!(lenient.contains("\x1b[38;2;215;172;120m95%"), "{lenient:?}");
}

/// A status line command for the shell Claude Code runs them in here: it prints `mine:`, the line it
/// was given on its input, and whether it runs under a profile.
fn echo_command(env: &Env) -> String {
    if status_line_shell(env)
        .command
        .to_string_lossy()
        .to_lowercase()
        .contains("powershell")
    {
        "[Console]::Out.Write('mine:' + [Console]::In.ReadToEnd() + ':' + $(if ($env:CLAUDE_CONFIG_DIR) { 'profile' } else { 'default' }))".to_owned()
    } else {
        "IFS= read -r line; printf 'mine:%s:%s' \"$line\" \"${CLAUDE_CONFIG_DIR:+profile}\"".to_owned()
    }
}

#[test]
fn the_status_line_runs_the_operators_previous_command_with_the_same_input() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let mut env = test_env(&home.home);
    env.set("CLAUDE_CONFIG_DIR", profile_dir(paths, "acct2").into_os_string());
    let command = echo_command(&env);
    write_json(
        &paths.install,
        &json!({ "previousStatusLine": { "type": "command", "command": command } }),
    );
    let now = now_ms();
    let input = json!({
        "session_id": SID,
        "rate_limits": { "five_hour": { "used_percentage": 10, "resets_at": seconds(now) + 3600 } }
    })
    .to_string();
    let parsed: Value = serde_json::from_str(&input).unwrap();
    let StatusLine::Previous {
        command: theirs,
        usage,
    } = handle_status_line(&parsed, &env, paths, now)
    else {
        panic!("the operator's own command runs instead");
    };
    assert_eq!(theirs, command);
    assert_eq!(usage, None, "off by default");
    assert_eq!(
        read_reading(paths, "acct2").unwrap().five_hour.unwrap().used,
        10.0,
        "recorded before passing on"
    );
    let out = run_previous_status_line(&theirs, &input, &env);
    assert_eq!(out.code, 0, "{out:?}");
    assert_eq!(out.stdout, format!("mine:{input}:profile"));
    // A command that fails says so with its code, and one that cannot be run at all is 1.
    let failing = run_previous_status_line("exit 3", &input, &env);
    assert_eq!((failing.stdout.as_str(), failing.code), ("", 3));
}

#[test]
fn append_usage_adds_batons_line_after_the_operators_on_its_last_line_as_far_as_the_width_allows() {
    let ours = |room: usize| {
        if room == 0 || room >= 4 {
            "OURS".to_owned()
        } else {
            String::new()
        }
    };
    assert_eq!(append_usage("theirs\n", &ours, 0), "theirs  OURS\n");
    assert_eq!(
        append_usage("theirs", &ours, 0),
        "theirs  OURS",
        "no newline added"
    );
    assert_eq!(
        append_usage("line one\nline two\n", &ours, 0),
        "line one\nline two  OURS\n",
        "on the last line"
    );
    assert_eq!(append_usage("theirs\r\n", &ours, 0), "theirs  OURS\n");
    // Their colour is closed before ours; widths count what is seen.
    let styled = |_: usize| "\x1b[1mOURS\x1b[0m".to_owned();
    assert_eq!(
        append_usage("\x1b[36mtheirs", &styled, 0),
        "\x1b[36mtheirs\x1b[0m  \x1b[1mOURS\x1b[0m"
    );
    assert!(append_usage("\x1b[36mabcdefgh\x1b[0m", &ours, 20).contains("OURS"));
    // Their line alone when theirs printed nothing, or there is no room left.
    assert_eq!(append_usage("", &ours, 0), "OURS\n");
    assert_eq!(append_usage("\n", &ours, 0), "OURS\n");
    assert_eq!(append_usage("abcdefgh\n", &ours, 10), "abcdefgh\n", "no room");
    assert_eq!(append_usage("abcdefgh\n", &ours, 12), "abcdefgh\n", "2 left");
    assert_eq!(
        append_usage("abcdefgh\n", &ours, 14),
        "abcdefgh  OURS\n",
        "4 left"
    );
    let wide = |_: usize| "WAY TOO WIDE".to_owned();
    assert_eq!(
        append_usage("abcdefgh\n", &wide, 20),
        "abcdefgh\n",
        "ours does not fit"
    );
    assert_eq!(
        append_usage("abcdefgh\r\n", &wide, 20),
        "abcdefgh\r\n",
        "theirs is left as it is"
    );
}

/// The accounts of a home as config.json holds them, with this policy.
fn set_policy(paths: &Paths, policy: Value) {
    let accounts: Vec<Value> = load_config(paths)
        .accounts
        .iter()
        .map(|a| json!({ "id": a.id, "name": a.name, "mark": a.mark.as_str() }))
        .collect();
    write_json(
        &paths.config,
        &json!({ "version": 1, "accounts": accounts, "policy": policy }),
    );
}

#[test]
fn with_the_operators_own_status_line_batons_usage_is_added_only_when_the_policy_says_so() {
    let home = four_accounts_home();
    let paths = &home.paths;
    write_json(
        &paths.install,
        &json!({ "previousStatusLine": { "type": "command", "command": "mine" } }),
    );
    let now = now_ms();
    let input = json!({
        "session_id": SID,
        "rate_limits": { "five_hour": { "used_percentage": 42, "resets_at": seconds(now) + 3600 } }
    });
    let run = |env: &Env| handle_status_line(&input, env, paths, now);
    // Off by default: only their command runs.
    assert_eq!(
        run(&plain_env(paths, &[])),
        StatusLine::Previous {
            command: "mine".to_owned(),
            usage: None
        }
    );
    set_policy(paths, json!({ "statusLineUsage": true }));
    let StatusLine::Previous { command, usage } = run(&plain_env(paths, &[])) else {
        panic!("their command still runs");
    };
    assert_eq!(command, "mine");
    assert_eq!(
        usage.unwrap().line(0),
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 42%"
    );
    // Plain claude (no Baton profile) never gets it.
    let plain_claude = Env::from_pairs([("NO_COLOR", "1")]);
    assert_eq!(
        run(&plain_claude),
        StatusLine::Previous {
            command: "mine".to_owned(),
            usage: None
        }
    );
    // A previous status line with no command to run is none.
    write_json(
        &paths.install,
        &json!({ "previousStatusLine": { "type": "command", "command": "" } }),
    );
    assert_eq!(run(&plain_claude), StatusLine::Text(String::new()));
    write_json(&paths.install, &json!({ "previousStatusLine": null }));
    assert_eq!(
        text_of(run(&plain_env(paths, &[]))),
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 42%"
    );
}

#[test]
fn the_operators_line_is_printed_and_with_status_line_usage_on_batons_after_it() {
    let home = four_accounts_home();
    let paths = &home.paths;
    write_json(
        &paths.install,
        &json!({ "previousStatusLine": { "type": "command", "command": "echo mine-ok" } }),
    );
    let now = now_ms();
    let input = json!({
        "session_id": SID,
        "rate_limits": { "five_hour": { "used_percentage": 10, "resets_at": seconds(now) + 3600 } }
    });
    // What `baton statusline` does with what the status line returns.
    let run = |extra: &[(&str, &str)]| {
        let mut env = test_env(&home.home);
        env.remove("COLUMNS");
        env.set("CLAUDE_CONFIG_DIR", profile_dir(paths, "acct2").into_os_string());
        for (name, value) in [("NO_COLOR", "1"), ("BATON_GLYPHS", "rich")].iter().chain(extra) {
            env.set(*name, *value);
        }
        let StatusLine::Previous { command, usage } = handle_status_line(&input, &env, paths, now) else {
            panic!("their command runs");
        };
        let out = run_previous_status_line(&command, &input.to_string(), &env);
        assert_eq!(out.code, 0);
        match usage {
            Some(usage) => append_usage(&out.stdout, &|room| usage.line(room), columns_of(&env)),
            None => out.stdout,
        }
    };
    set_policy(paths, json!({}));
    assert_eq!(
        run(&[]).replace("\r\n", "\n"),
        "mine-ok\n",
        "off by default: theirs as it is"
    );
    set_policy(paths, json!({ "statusLineUsage": true }));
    assert_eq!(
        run(&[]),
        "mine-ok  \u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 10%\n"
    );
    // Too narrow for any of it: theirs only.
    assert_eq!(run(&[("COLUMNS", "10")]).replace("\r\n", "\n"), "mine-ok\n");
    // Room for the mark and the account, not for the window.
    assert_eq!(
        run(&[("COLUMNS", "20")]),
        "mine-ok  \u{257a}\u{2501}\u{2578} acct2\n"
    );
}

#[test]
fn baton_hop_inside_a_session_names_the_conversation_its_wrapper_follows_not_one_a_claude_started_inside_wrote()
 {
    let home = four_accounts_home();
    let paths = &home.paths;
    let nested = "0d9c8b7a-6f5e-4d3c-2b1a-0f9e8d7c6b5a";
    let env = Env::from_pairs([("BATON_WRAPPER", "123-abcd")]);
    // Before the wrapper has taken a conversation up: its mapping.
    write_json(
        &wrapper_session_file(paths, "123-abcd"),
        &json!({ "session": SID, "pid": 1 }),
    );
    assert_eq!(resolve_session(paths, None, &env).as_deref(), Ok(SID));
    write_json(
        &wrapper_file(paths, "123-abcd"),
        &json!({ "wrapper": "123-abcd", "session": SID }),
    );
    write_json(
        &wrapper_session_file(paths, "123-abcd"),
        &json!({ "session": nested, "pid": 2 }),
    );
    assert_eq!(resolve_session(paths, None, &env).as_deref(), Ok(SID));
}
