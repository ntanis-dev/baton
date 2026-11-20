//! The usage read and its poller: what a usage report becomes, how a poll is recorded, the schedule,
//! the back-off, and the read itself against a stand-in for `claude`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use baton_claude::auth::AuthStatus;
use baton_claude::poller::{Poller, PollerOptions};
use baton_claude::probe::{
    Next, Outcome, ProbeRequest, QueryOptions, Session, probe_dir, project_dir_name, read_plan_usage,
    usage_from_report,
};
use baton_core::claude_path::Launcher;
use baton_core::clock::{HOUR, MINUTE, Ms, iso, local_ms, now_ms, parse_ms};
use baton_core::config::{Account, Mark, check_policy_value, effective_policy, load_config, save_config};
use baton_core::fsutil::{is_alive, read_json, read_json_lines};
use baton_core::paths::{Paths, profile_dir};
use baton_core::poll::{BACKOFF_FIRST_MS, BACKOFF_MAX_MS, next_poll_at, read_poll_state};
use baton_core::state::{Want, load_snapshot};
use baton_core::usage::{ExtraUsage, RecordOptions, Sample, history_file, record_reading};
use baton_core::window::PerWindow;
use baton_testkit::fixtures::{FAKE_USAGE_CLAUDE, launcher};
use baton_testkit::{accounts_home, test_env, test_env_with, until};
use serde_json::{Value, json};

const M: Ms = MINUTE;
const H: Ms = HOUR;

/// 2026-10-25T10:00:00Z.
fn t0() -> Ms {
    parse_ms("2026-10-25T10:00:00Z").unwrap()
}

fn meter(kind: &str, group: &str, percent: f64, resets_at: Ms, extra: Value) -> Value {
    let mut row = json!({
        "kind": kind,
        "group": group,
        "percent": percent,
        "resets_at": iso(resets_at),
        "severity": "normal",
        "is_active": false,
    });
    for (key, value) in extra.as_object().unwrap() {
        row[key] = value.clone();
    }
    row
}

/// A usage report as Claude Code sends it: both windows, a model's own weekly limit, a broken row.
fn report(at: Ms) -> Value {
    json!({
        "session": {},
        "rate_limits": {
            "limits": [
                meter("session", "session", 42.0, at + 2 * H, json!({ "is_active": true })),
                meter("weekly_all", "weekly", 63.0, at + 72 * H, json!({})),
                meter(
                    "weekly_scoped",
                    "weekly",
                    80.0,
                    at + 48 * H,
                    json!({ "scope": { "model": { "display_name": "Fable" } }, "severity": "warning" }),
                ),
                { "kind": "broken", "group": "weekly", "percent": null },
            ],
            "extra_usage": {
                "is_enabled": true,
                "monthly_limit": 5000,
                "used_credits": 1234,
                "utilization": 24.68,
                "currency": "USD",
            },
        },
    })
}

fn sample(used: f64, resets_at: Ms) -> Option<Sample> {
    Some(Sample {
        used,
        resets_at: Some(iso(resets_at)),
    })
}

// The accounts and the policy, changed the way the commands change them (the profiles themselves are
// another crate's).
fn add_account(paths: &Paths, id: &str) {
    let mut config = load_config(paths);
    config.accounts.push(Account {
        id: id.to_owned(),
        name: id.to_owned(),
        mark: Mark::Normal,
        five_hour: None,
        weekly: None,
    });
    save_config(paths, &config).unwrap();
}

fn remove_account(paths: &Paths, id: &str) {
    let mut config = load_config(paths);
    config.accounts.retain(|account| account.id != id);
    save_config(paths, &config).unwrap();
}

fn set_policy(paths: &Paths, key: &str, value: Value) {
    let mut config = load_config(paths);
    config
        .policy
        .insert(key.to_owned(), check_policy_value(key, &value).unwrap());
    save_config(paths, &config).unwrap();
}

/// One read the fake poller was asked for.
struct Call {
    id: String,
    claude_path: PathBuf,
}

/// A poller with a fake read and a clock the test moves.
struct Fake {
    poller: Poller,
    clock: Arc<AtomicI64>,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl Fake {
    fn set(&self, t: Ms) {
        self.clock.store(t, Ordering::SeqCst);
    }

    fn ids(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.id.clone())
            .collect()
    }
}

const FAKE_PATH: &str = "fake-usage-claude";

fn fake_poller(paths: &Paths, outcomes: &[(&str, Vec<Outcome>)], signed_out: &[&str]) -> Fake {
    let clock = Arc::new(AtomicI64::new(t0()));
    let calls: Arc<Mutex<Vec<Call>>> = Arc::new(Mutex::new(Vec::new()));
    let queue: HashMap<String, VecDeque<Outcome>> = outcomes
        .iter()
        .map(|(id, list)| ((*id).to_owned(), list.iter().cloned().collect()))
        .collect();
    let queue = Mutex::new(queue);
    let signed_out: Vec<String> = signed_out.iter().map(|id| (*id).to_owned()).collect();
    let mut options = PollerOptions::new(paths.clone(), test_env(&paths.home));
    let time = Arc::clone(&clock);
    options.now = Arc::new(move || time.load(Ordering::SeqCst));
    options.launcher = Some(Launcher::program(FAKE_PATH));
    options.auth = Some(Arc::new(move |id: &str, _: &Launcher| {
        Some(AuthStatus {
            ok: true,
            logged_in: !signed_out.iter().any(|out| out == id),
            ..AuthStatus::default()
        })
    }));
    let seen = Arc::clone(&calls);
    options.probe = Some(Arc::new(move |request: &ProbeRequest<'_>| {
        let at = (request.now)();
        seen.lock().unwrap().push(Call {
            id: request.id.to_owned(),
            claude_path: request.launcher.path.clone(),
        });
        let next = queue
            .lock()
            .unwrap()
            .get_mut(request.id)
            .and_then(VecDeque::pop_front);
        next.unwrap_or_else(|| usage_from_report(Some(&report(at)), "", at))
    }));
    Fake {
        poller: Poller::new(options),
        clock,
        calls,
    }
}

#[test]
fn a_usage_report_becomes_a_reading_both_windows_the_other_meters_extra_usage_no_plan_usage_is_unavailable() {
    let t0 = t0();
    let Outcome::Ok {
        reading,
        limits,
        extra_usage,
    } = usage_from_report(Some(&report(t0)), "", t0)
    else {
        panic!("not ok");
    };
    assert_eq!(
        reading,
        PerWindow {
            five_hour: sample(42.0, t0 + 2 * H),
            seven_day: sample(63.0, t0 + 72 * H)
        }
    );
    let rows: Vec<Value> = limits
        .iter()
        .map(|l| json!([l.kind, l.group, l.label, l.used, l.active]))
        .collect();
    assert_eq!(
        rows,
        [
            json!(["session", "session", null, 42.0, true]),
            json!(["weekly_all", "weekly", null, 63.0, false]),
            json!(["weekly_scoped", "weekly", "Fable", 80.0, false]),
        ]
    );
    assert_eq!(limits[2].severity.as_deref(), Some("warning"));
    assert_eq!(limits[0].resets_at, Some(iso(t0 + 2 * H)));
    assert_eq!(
        extra_usage,
        Some(ExtraUsage {
            enabled: true,
            monthly_limit: Some(5000.0),
            used_credits: Some(1234.0),
            utilization: Some(24.68),
            currency: Some("USD".to_owned()),
        })
    );
    // An empty list: the endpoint answered with no meters.
    assert_eq!(
        usage_from_report(Some(&json!({ "rate_limits": { "limits": [] } })), "", t0),
        Outcome::Ok {
            reading: PerWindow::default(),
            limits: Vec::new(),
            extra_usage: None
        }
    );
    assert_eq!(
        usage_from_report(
            Some(&json!({ "session": {}, "rate_limits": null })),
            "Could not load your plan usage.\n",
            t0
        ),
        Outcome::Unavailable {
            reason: "Could not load your plan usage.".to_owned()
        }
    );
    assert_eq!(
        usage_from_report(Some(&json!({ "rate_limits": null })), "", t0).reason(),
        Some("the plan usage could not be fetched")
    );
    let none = usage_from_report(None, "", t0);
    assert_eq!(none.status(), "unavailable");
    assert!(none.reason().unwrap().contains("no usage report"), "{none:?}");
    assert_eq!(
        usage_from_report(None, "Something else", t0).reason(),
        Some("no usage report (Something else)")
    );
    let local = local_ms(2026, 10, 25, 1, 0).unwrap();
    assert_eq!(
        usage_from_report(
            Some(&json!({ "rate_limits": null })),
            "Usage data is rate limited until 3:14am.",
            local
        ),
        Outcome::RateLimited {
            until: local_ms(2026, 10, 25, 3, 14),
            reason: "Usage data is rate limited until 3:14am.".to_owned(),
        }
    );
    assert_eq!(
        usage_from_report(
            Some(&json!({ "rate_limits": null })),
            "Usage endpoint is rate limited. Please try again in a moment.",
            t0
        ),
        Outcome::RateLimited {
            until: None,
            reason: "Usage endpoint is rate limited. Please try again in a moment.".to_owned(),
        }
    );
    assert_eq!(
        usage_from_report(None, "Not logged in · Please run /login", t0).status(),
        "signed-out"
    );
}

#[test]
fn the_strictest_weekly_limit_in_force_decides_a_models_own_weekly_limit_over_the_all_models_one_until_it_resets()
 {
    let t0 = t0();
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let Outcome::Ok {
        reading,
        limits,
        extra_usage,
    } = usage_from_report(Some(&report(t0)), "", t0)
    else {
        panic!("not ok");
    };
    let polled = RecordOptions {
        source: "poll".to_owned(),
        refresh_ms: 0,
        limits: Some(Some(limits)),
        extra_usage: Some(extra_usage),
        ..RecordOptions::at(t0)
    };
    record_reading(paths, "acct1", &reading, polled);
    let week = |now: Ms| {
        load_snapshot(paths, now, Want::default()).states[0]
            .windows
            .seven_day
            .clone()
    };
    let first = week(t0 + H);
    assert_eq!(
        (first.used, first.label.as_deref(), first.source.as_deref()),
        (Some(80.0), Some("Fable"), Some("poll"))
    );
    assert_eq!(first.resets_at, Some(t0 + 48 * H));
    // A status line reading of the all-models window later: still the stricter.
    let later = PerWindow {
        five_hour: None,
        seven_day: sample(70.0, t0 + 72 * H),
    };
    record_reading(paths, "acct1", &later, RecordOptions::at(t0 + 2 * H));
    assert_eq!(week(t0 + 3 * H).used, Some(80.0));
    // After the model's own limit reset, the all-models window again.
    let after = week(t0 + 49 * H);
    assert_eq!((after.used, after.label), (Some(70.0), None));
}

#[test]
fn a_poll_is_recorded_like_a_status_line_reading_with_source_poll_a_status_line_reading_after_it_keeps_the_other_meters()
 {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let fake = fake_poller(paths, &[], &[]);
    let at = now_ms();
    fake.set(at);
    let results = fake.poller.refresh_wait(Some("acct1"), "request");
    assert_eq!(results[0].status, "ok");
    let usage = paths.usage.join("acct1.json");
    let stored = read_json(&usage).unwrap();
    assert_eq!(stored["source"], "poll");
    assert_eq!(stored["five_hour"]["source"], "poll");
    assert_eq!(stored["seven_day"]["used"], json!(63));
    assert_eq!(stored["limits"].as_array().unwrap().len(), 3);
    assert_eq!(stored["limitsAt"], json!(iso(at)));
    assert_eq!(stored["extraUsage"]["usedCredits"], json!(1234));
    assert_eq!(
        results[0]
            .reading
            .as_ref()
            .unwrap()
            .limits
            .as_ref()
            .unwrap()
            .len(),
        3
    );
    let history: Vec<Value> = read_json_lines(&history_file(paths, "readings", at))
        .iter()
        .map(|h| json!([h["account"], h["window"], h["used"], h["source"]]))
        .collect();
    assert_eq!(
        history,
        [
            json!(["acct1", "five_hour", 42, "poll"]),
            json!(["acct1", "seven_day", 63, "poll"])
        ]
    );
    // A session's status line then reports the 5-hour window only.
    let line = PerWindow {
        five_hour: sample(50.0, at + 2 * H),
        seven_day: None,
    };
    record_reading(paths, "acct1", &line, RecordOptions::at(now_ms()));
    let after = read_json(&usage).unwrap();
    assert_eq!(after["five_hour"]["source"], "statusline");
    assert_eq!(after["seven_day"]["source"], "poll");
    assert_eq!(after["limits"].as_array().unwrap().len(), 3);
}

#[test]
fn the_schedule_every_account_at_the_start_then_every_usage_poll_minutes_a_changed_interval_applies_at_once_0_polls_only_on_request()
 {
    let t0 = t0();
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    add_account(paths, "acct2");
    let fake = fake_poller(paths, &[], &[]);
    let due = || fake.poller.tick().map(|done| done.recv().unwrap());
    assert!(due().is_some());
    assert_eq!(fake.ids(), ["acct1", "acct2"]);
    assert_eq!(fake.calls.lock().unwrap()[0].claude_path, Path::new(FAKE_PATH));
    assert_eq!(read_poll_state(paths).last_run_at, Some(iso(t0)));
    let policy = || effective_policy(&load_config(paths));
    assert_eq!(
        next_poll_at(&read_poll_state(paths), &policy(), true, t0),
        Some(t0 + 30 * M)
    );
    assert_eq!(next_poll_at(&read_poll_state(paths), &policy(), false, t0), None);
    fake.set(t0 + 29 * M);
    assert!(due().is_none());
    fake.set(t0 + 30 * M);
    assert!(due().is_some());
    assert_eq!(fake.ids().len(), 4);
    set_policy(paths, "usagePollMinutes", json!(10));
    fake.set(t0 + 39 * M);
    assert!(due().is_none());
    fake.set(t0 + 40 * M);
    assert!(due().is_some());
    assert_eq!(fake.ids().len(), 6);
    // One account on request: the schedule stays.
    fake.set(t0 + 45 * M);
    let one = fake.poller.refresh_wait(Some("acct2"), "menu");
    assert_eq!(one[0].name, "acct2");
    assert_eq!(fake.ids().last().unwrap(), "acct2");
    assert_eq!(read_poll_state(paths).last_run_at, Some(iso(t0 + 40 * M)));
    set_policy(paths, "usagePollMinutes", json!(0));
    fake.set(t0 + 600 * M);
    assert!(due().is_none());
    assert_eq!(
        next_poll_at(&read_poll_state(paths), &policy(), true, t0 + 600 * M),
        None
    );
    // Requests wait their turn; a removed account is not polled.
    remove_account(paths, "acct1");
    let all = fake.poller.refresh_wait(None, "request");
    assert_eq!(all.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["acct2"]);
    let gone = fake.poller.refresh_wait(Some("acct1"), "request");
    assert_eq!(
        (gone[0].status.as_str(), gone[0].reason.as_deref()),
        ("unknown", Some("not one of Baton's accounts"))
    );
    assert_eq!(
        serde_json::to_value(&gone[0]).unwrap(),
        json!({ "id": "acct1", "name": "acct1", "status": "unknown", "reason": "not one of Baton's accounts" })
    );
    fake.poller.stop();
    assert_eq!(fake.poller.pending(), 0);
}

#[test]
fn back_off_the_rate_limits_own_time_else_5_minutes_doubling_up_to_2_hours_a_reading_clears_it() {
    let t0 = t0();
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let limited = |until: Option<Ms>| Outcome::RateLimited {
        until,
        reason: "Usage endpoint is rate limited".to_owned(),
    };
    let mut outcomes = vec![limited(Some(t0 + 40 * M))];
    outcomes.extend((0..7).map(|_| limited(None)));
    let fake = fake_poller(paths, &[("acct1", outcomes)], &[]);
    let ask = || fake.poller.refresh_wait(Some("acct1"), "request").remove(0);
    let first = ask();
    assert_eq!(first.status, "rate-limited");
    assert_eq!(first.backoff_until, Some(iso(t0 + 40 * M)));
    fake.set(t0 + 10 * M);
    let waiting = ask();
    assert_eq!(waiting.status, "backoff");
    assert_eq!(waiting.until, Some(iso(t0 + 40 * M)));
    assert_eq!(waiting.reason.as_deref(), Some("Usage endpoint is rate limited"));
    assert_eq!(fake.ids().len(), 1, "not asked while it waits");
    assert_eq!(
        serde_json::to_string(&waiting).unwrap(),
        format!(
            r#"{{"id":"acct1","name":"acct1","status":"backoff","until":"{}","reason":"Usage endpoint is rate limited"}}"#,
            iso(t0 + 40 * M)
        )
    );
    fake.set(t0 + 40 * M);
    let mut waits = Vec::new();
    for _ in 0..7 {
        let r = ask();
        assert_eq!(r.status, "rate-limited");
        waits.push(r.backoff_ms.unwrap() / M as f64);
        fake.set(parse_ms(r.backoff_until.as_deref().unwrap()).unwrap());
    }
    assert_eq!(waits, [5.0, 10.0, 20.0, 40.0, 80.0, 120.0, 120.0]);
    assert_eq!(BACKOFF_FIRST_MS, 5 * M);
    assert_eq!(BACKOFF_MAX_MS, 2 * H);
    assert_eq!(read_poll_state(paths).accounts["acct1"].failures, Some(8));
    let ok = ask();
    assert_eq!(ok.status, "ok");
    let now = fake.clock.load(Ordering::SeqCst);
    assert_eq!(
        (ok.backoff_until, ok.backoff_ms, ok.failures, ok.ok_at),
        (None, None, 0, Some(iso(now)))
    );
    // The state file, as it has always been written.
    let stored = read_json(&baton_core::poll::poll_file(paths)).unwrap();
    assert_eq!(
        stored["accounts"]["acct1"],
        json!({
            "at": iso(now), "status": "ok", "reason": null, "okAt": iso(now),
            "backoffUntil": null, "backoffMs": null, "failures": 0
        })
    );
    assert_eq!(stored["running"], Value::Null);
}

#[test]
fn a_signed_out_account_is_skipped_and_marked_the_others_are_polled() {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    add_account(paths, "acct2");
    let fake = fake_poller(paths, &[], &["acct2"]);
    let results = fake.poller.refresh_wait(None, "request");
    let said: Vec<(&str, &str)> = results
        .iter()
        .map(|r| (r.id.as_str(), r.status.as_str()))
        .collect();
    assert_eq!(said, [("acct1", "ok"), ("acct2", "signed-out")]);
    assert_eq!(
        results[1].reason.as_deref(),
        Some("not signed in: baton login acct2")
    );
    assert_eq!(fake.ids(), ["acct1"]);
    // As JSON: the shape `baton usage --refresh --json` has always printed.
    let out = serde_json::to_value(&results).unwrap();
    let keys = |value: &Value| value.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    let asked = [
        "id",
        "name",
        "at",
        "status",
        "reason",
        "okAt",
        "backoffUntil",
        "backoffMs",
        "failures",
        "reading",
    ];
    assert_eq!(keys(&out[0]), asked);
    assert_eq!(keys(&out[1]), asked);
    assert_eq!(out[1]["reading"], Value::Null);
    assert_eq!(out[0]["reading"]["five_hour"]["used"], json!(42));
    assert_eq!(results[1].entry().status.as_deref(), Some("signed-out"));
    assert!(!paths.usage.join("acct2.json").exists());
    assert_eq!(
        read_poll_state(paths).accounts["acct2"].failures,
        Some(0),
        "not a failure"
    );
}

/// Makes `link` a folder that is really `target` (a junction on Windows, a symbolic link elsewhere):
/// what a profile's projects/ is to the shared base's.
fn link_dir(link: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    #[cfg(windows)]
    {
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn the_read_runs_the_installed_claude_under_the_profile_with_nothing_loaded_and_leaves_no_transcript_behind()
{
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    add_account(paths, "acct2");
    link_dir(
        &profile_dir(paths, "acct2").join("projects"),
        &paths.claude_dir.join("projects"),
    );
    let root = home.home.parent().unwrap();
    let log = root.join("probe-log.jsonl");
    let elsewhere = root.join("elsewhere");
    let env = test_env_with(
        &home.home,
        &[
            ("FAKE_USAGE_LOG", &log.to_string_lossy()),
            ("FAKE_TRANSCRIPT", "1"),
            ("CLAUDE_CONFIG_DIR", &elsewhere.to_string_lossy()),
            ("CLAUDE_CODE_SESSION_ID", "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f"),
            ("BATON_WRAPPER", "w1"),
            ("CLAUDECODE", "1"),
        ],
    );
    let claude = launcher(FAKE_USAGE_CLAUDE);
    for id in ["acct1", "acct2"] {
        let request = ProbeRequest {
            timeout_ms: 30_000,
            ..ProbeRequest::new(paths, id, &env, &claude)
        };
        let Outcome::Ok { reading, .. } = read_plan_usage(&request) else {
            panic!("{id} was not read");
        };
        assert_eq!(reading.five_hour.unwrap().used, 42.0);
    }
    let runs = read_json_lines(&log);
    assert_eq!(runs.len(), 2);
    let texts = |list: &Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_owned())
            .collect()
    };
    for run in &runs {
        let argv = texts(&run["argv"]);
        assert_eq!(
            argv,
            [
                "--output-format",
                "stream-json",
                "--verbose",
                "--input-format",
                "stream-json",
                "--max-turns=1",
                "--tools=",
                "--setting-sources=",
                "--strict-mcp-config",
                "--no-session-persistence",
            ],
            "exactly what the Agent SDK passes for these options"
        );
        assert!(!argv.iter().any(|arg| arg.starts_with("--resume")));
        let names = texts(&run["envNames"]);
        for name in ["CLAUDE_CODE_SESSION_ID", "BATON_WRAPPER", "CLAUDECODE"] {
            assert!(!names.iter().any(|n| n == name), "{name} is not passed on");
        }
        assert!(
            names.iter().any(|n| n == "CLAUDE_CODE_ENTRYPOINT"),
            "it says it is driven like the SDK"
        );
        assert_eq!(
            run["cwd"].as_str().unwrap().to_lowercase(),
            probe_dir(paths).to_string_lossy().to_lowercase()
        );
    }
    let config_dir = |run: &Value| PathBuf::from(run["configDir"].as_str().unwrap());
    assert_eq!(
        config_dir(&runs[0]),
        profile_dir(paths, "acct1"),
        "always the account's own profile"
    );
    assert_eq!(config_dir(&runs[1]), profile_dir(paths, "acct2"));
    let name = project_dir_name(&probe_dir(paths).to_string_lossy());
    for base in [
        paths.claude_dir.clone(),
        profile_dir(paths, "acct1"),
        profile_dir(paths, "acct2"),
    ] {
        assert!(
            !base.join("projects").join(&name).exists(),
            "no transcript in {}",
            base.display()
        );
    }
}

/// A session that never answers and ignores being told to stop.
struct Stuck {
    aborted: Arc<AtomicBool>,
}

impl Session for Stuck {
    fn next(&mut self, timeout: Duration) -> Next {
        std::thread::sleep(timeout);
        Next::Waiting
    }

    fn abort(&mut self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    fn close(&mut self) {}
}

#[test]
fn a_claude_that_hangs_is_ended_after_the_timeout_its_process_with_it_a_session_that_never_answers_too() {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let pid_file = home.home.parent().unwrap().join("hang.pid");
    let claude = launcher(FAKE_USAGE_CLAUDE);
    let env = test_env_with(
        &home.home,
        &[
            ("FAKE_USAGE_MODE", "hang"),
            ("FAKE_USAGE_PID", &pid_file.to_string_lossy()),
        ],
    );
    let started = Instant::now();
    let hung = read_plan_usage(&ProbeRequest {
        timeout_ms: 1500,
        ..ProbeRequest::new(paths, "acct1", &env, &claude)
    });
    assert_eq!(
        hung,
        Outcome::Timeout {
            reason: "no answer in 2 s; claude was ended".to_owned()
        }
    );
    assert!(started.elapsed() < Duration::from_secs(15));
    let pid: i64 = fs::read_to_string(&pid_file).unwrap().parse().unwrap();
    until(|| (!is_alive(pid)).then_some(()));
    // A session that never yields and ignores the abort.
    let env = test_env(&home.home);
    let aborted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&aborted);
    let never = move |_: &QueryOptions<'_>| -> Result<Box<dyn Session>, String> {
        Ok(Box::new(Stuck {
            aborted: Arc::clone(&flag),
        }))
    };
    let stuck = read_plan_usage(&ProbeRequest {
        timeout_ms: 200,
        query: Some(&never),
        ..ProbeRequest::new(paths, "acct1", &env, &claude)
    });
    assert_eq!(stuck.status(), "timeout");
    assert!(aborted.load(Ordering::SeqCst), "the session was told to stop");
    // One that is called off.
    let stop = AtomicBool::new(true);
    let called_off = read_plan_usage(&ProbeRequest {
        stop: Some(&stop),
        query: Some(&never),
        ..ProbeRequest::new(paths, "acct1", &env, &claude)
    });
    assert_eq!(called_off, Outcome::Stopped);
    // One that fails outright.
    let fails =
        |_: &QueryOptions<'_>| -> Result<Box<dyn Session>, String> { Err("spawn claude ENOENT".to_owned()) };
    let failed = read_plan_usage(&ProbeRequest {
        query: Some(&fails),
        ..ProbeRequest::new(paths, "acct1", &env, &claude)
    });
    assert_eq!(
        failed,
        Outcome::Failed {
            reason: "spawn claude ENOENT".to_owned()
        }
    );
    // A claude that is not there at all.
    let missing = Launcher::program(home.home.join("no-claude-here"));
    let absent = read_plan_usage(&ProbeRequest::new(paths, "acct1", &env, &missing));
    assert_eq!(absent.status(), "failed");
    assert!(
        absent.reason().unwrap().starts_with("spawn claude: "),
        "{absent:?}"
    );
}

#[test]
fn rate_limits_null_a_rate_limited_endpoint_and_no_report_through_the_real_protocol() {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let claude = launcher(FAKE_USAGE_CLAUDE);
    let read = |mode: &str| {
        let env = test_env_with(&home.home, &[("FAKE_USAGE_MODE", mode)]);
        read_plan_usage(&ProbeRequest {
            timeout_ms: 30_000,
            ..ProbeRequest::new(paths, "acct1", &env, &claude)
        })
    };
    assert_eq!(
        read("null"),
        Outcome::Unavailable {
            reason: "Could not load your plan usage.".to_owned()
        }
    );
    let Outcome::RateLimited { until, .. } = read("ratelimited") else {
        panic!("not rate limited");
    };
    assert!(until.unwrap() > now_ms());
    let none = read("none");
    assert_eq!(
        none,
        Outcome::Unavailable {
            reason: "no usage report (Current session 42% used)".to_owned()
        }
    );
}

#[test]
fn the_poller_reads_through_a_real_claude_and_a_stop_ends_a_read_in_progress() {
    let home = accounts_home(&["acct1", "acct2"]);
    let paths = &home.paths;
    let pid_file = home.home.parent().unwrap().join("hang.pid");
    // Asked for real: auth status and the read, under each account's own profile.
    let mut options = PollerOptions::new(paths.clone(), test_env(&home.home));
    options.launcher = Some(launcher(FAKE_USAGE_CLAUDE));
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let said = Arc::clone(&lines);
    options.log = Some(Arc::new(move |line: &str| {
        said.lock().unwrap().push(line.to_owned())
    }));
    let poller = Poller::new(options);
    let results = poller.refresh_wait(None, "request");
    assert_eq!(
        results.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(),
        ["ok", "ok"]
    );
    assert_eq!(*lines.lock().unwrap(), ["acct1: ok", "acct2: ok"]);
    assert_eq!(
        read_json(&paths.usage.join("acct2.json")).unwrap()["five_hour"]["used"],
        json!(42)
    );
    // Signed out, as claude itself says it.
    let mut options = PollerOptions::new(
        paths.clone(),
        test_env_with(&home.home, &[("FAKE_LOGGED_IN", "0")]),
    );
    options.launcher = Some(launcher(FAKE_USAGE_CLAUDE));
    let signed_out = Poller::new(options).refresh_wait(Some("acct1"), "request");
    assert_eq!(signed_out[0].status, "signed-out");
    // A read that hangs ends with the poller, its claude with it.
    let env = test_env_with(
        &home.home,
        &[
            ("FAKE_USAGE_MODE", "hang"),
            ("FAKE_USAGE_PID", &pid_file.to_string_lossy()),
        ],
    );
    let mut options = PollerOptions::new(paths.clone(), env);
    options.launcher = Some(launcher(FAKE_USAGE_CLAUDE));
    let poller = Poller::new(options);
    let waiting = poller.refresh(Some("acct1"), "request");
    let pid: i64 = until(|| {
        fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.parse().ok())
    });
    assert_eq!(read_poll_state(paths).running.unwrap().account, "acct1");
    let started = Instant::now();
    poller.stop();
    assert!(started.elapsed() < Duration::from_secs(15));
    assert_eq!(waiting.recv().unwrap()[0].status, "stopped");
    until(|| (!is_alive(pid)).then_some(()));
    assert_eq!(read_poll_state(paths).running, None);
    assert!(poller.tick().is_none(), "a stopped poller polls no more");
}

#[test]
fn picking_and_polling_only_ever_use_batons_own_profiles() {
    use baton_core::env::Env;
    use baton_core::paths::profile_env;
    use baton_core::policy::pick_start;

    let home = accounts_home(&["acct1", "acct2"]);
    let paths = &home.paths;
    for id in ["acct1", "acct2"] {
        let own = profile_dir(paths, id);
        assert_eq!(
            profile_env(paths, id, &Env::new()).get_os("CLAUDE_CONFIG_DIR"),
            Some(own.as_os_str())
        );
        let default_account =
            Env::from_pairs([("CLAUDE_CONFIG_DIR", paths.claude_dir.clone().into_os_string())]);
        assert_eq!(
            profile_env(paths, id, &default_account).get_os("CLAUDE_CONFIG_DIR"),
            Some(own.as_os_str())
        );
    }
    let snapshot = load_snapshot(paths, now_ms(), Want::default());
    assert_eq!(
        snapshot
            .states
            .iter()
            .map(|state| state.id.as_str())
            .collect::<Vec<_>>(),
        ["acct1", "acct2"]
    );
    let picked = pick_start(&snapshot.states, &snapshot.policy, &[])
        .account
        .unwrap();
    assert!(["acct1", "acct2"].contains(&picked.as_str()));
    // The poller asks only the added accounts, each under its own profile.
    let asked: Arc<Mutex<Vec<(String, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&asked);
    let mut options = PollerOptions::new(paths.clone(), test_env(&home.home));
    options.launcher = Some(Launcher::program(FAKE_PATH));
    options.auth = Some(Arc::new(|_: &str, _: &Launcher| {
        Some(AuthStatus {
            ok: true,
            logged_in: true,
            ..AuthStatus::default()
        })
    }));
    options.probe = Some(Arc::new(move |request: &ProbeRequest<'_>| {
        let env = profile_env(request.paths, request.id, request.env);
        let dir = PathBuf::from(env.get_os("CLAUDE_CONFIG_DIR").unwrap());
        seen.lock().unwrap().push((request.id.to_owned(), dir));
        Outcome::Unavailable {
            reason: "test".to_owned(),
        }
    }));
    let results = Poller::new(options).refresh_wait(None, "request");
    assert_eq!(
        results.iter().map(|r| r.status.as_str()).collect::<Vec<_>>(),
        ["unavailable", "unavailable"]
    );
    assert_eq!(
        *asked.lock().unwrap(),
        [
            ("acct1".to_owned(), profile_dir(paths, "acct1")),
            ("acct2".to_owned(), profile_dir(paths, "acct2"))
        ]
    );
}
