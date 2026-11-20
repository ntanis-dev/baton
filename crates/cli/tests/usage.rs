//! The usage commands from the command line: `baton status`, `baton usage` (with `--refresh`),
//! `baton pick` and `baton stats`, over readings, the history, a poll and the poll lease.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once"
)]

mod common;
mod support;

use std::cell::Cell;
use std::sync::Arc;

use baton_claude::auth::AuthStatus;
use baton_claude::poll_host::{PollHost, PollHostOptions, PollWork};
use baton_claude::poller::{Poller, PollerOptions};
use baton_claude::probe::{ProbeRequest, usage_from_report};
use baton_core::claude_path::Launcher;
use baton_core::clock::{DAY, HOUR, MINUTE, Ms, iso, local, local_ms, now_ms, parse_ms};
use baton_core::fsutil::{append_json_line, read_json};
use baton_core::paths::Paths;
use baton_core::usage::{RecordOptions, Sample, history_file, record_reading};
use baton_core::window::PerWindow;
use baton_profile::accounts::{AddOptions, add_account};
use baton_testkit::fixtures::{FAKE_USAGE_CLAUDE, bin};
use baton_testkit::{TestHome, accounts_home, temp_home, test_env, write_json};
use common::{Run, baton_env, baton_io};
use serde_json::{Value, json};
use support::{env_of, squash};

thread_local! {
    /// The moment the commands of the running test see as now.
    static NOW: Cell<Ms> = const { Cell::new(0) };
}

/// The clock of [`baton_at`]: the moment the test pinned.
fn pinned() -> Ms {
    NOW.with(Cell::get)
}

/// Runs `baton` in a test home with its clock at `at`, so that what it says of times does not move
/// while the test runs.
fn baton_at(home: &TestHome, at: Ms, args: &[&str]) -> Run {
    NOW.with(|now| now.set(at));
    baton_io(env_of(home), args, |io| io.now = pinned)
}

/// Runs `baton` in a test home by the real clock.
fn baton(home: &TestHome, args: &[&str]) -> Run {
    baton_env(env_of(home), args)
}

/// Whether the words of a line are what a pattern asks for, in order: a word is itself, `*` any one
/// word, `..` any words or none, `#` a number and `???` a word of as many letters or digits.
fn glob(words: &[&str], pattern: &[&str]) -> bool {
    let Some((first, rest)) = pattern.split_first() else {
        return words.is_empty();
    };
    if *first == ".." {
        return (0..=words.len()).any(|skip| glob(&words[skip..], rest));
    }
    let Some((word, more)) = words.split_first() else {
        return false;
    };
    let fits = match *first {
        "*" => true,
        "#" => word.chars().all(|c| c.is_ascii_digit()),
        marks if marks.chars().all(|c| c == '?') => {
            word.chars().count() == marks.len() && word.chars().all(|c| c.is_alphanumeric() || c == '_')
        }
        exact => *word == exact,
    };
    fits && glob(more, rest)
}

/// Whether a line of `out` is these words (see [`glob`]), however many spaces are between them.
fn has_line(out: &str, pattern: &[&str]) -> bool {
    out.lines().any(|line| {
        glob(
            &line
                .split(' ')
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>(),
            pattern,
        )
    })
}

/// Writes an account's reading as a session left it two minutes before `now`: each window as percent
/// used and how long from `now` it resets.
fn write_usage(paths: &Paths, id: &str, now: Ms, five: (f64, Ms), week: (f64, Ms)) {
    let window = |(used, resets_in): (f64, Ms)| json!({ "used": used, "resetsAt": iso(now + resets_in), "readAt": iso(now - 2 * MINUTE) });
    write_json(
        &paths.usage.join(format!("{id}.json")),
        &json!({ "account": id, "five_hour": window(five), "seven_day": window(week) }),
    );
}

#[test]
fn status_accounts_with_their_windows_blocks_sessions_and_where_new_work_goes() {
    let home = temp_home();
    let paths = &home.paths;
    for name in ["acct1", "acct2", "acct3"] {
        baton(&home, &["account", "add", name]);
    }
    let now = now_ms();
    write_usage(paths, "acct1", now, (95.0, 42 * MINUTE), (40.0, 3 * DAY));
    write_usage(paths, "acct2", now, (20.0, HOUR), (97.0, DAY));
    write_usage(paths, "acct3", now, (10.0, HOUR), (50.0, DAY));
    let text = baton_at(&home, now, &["status"]).stdout;
    // One row per account: 5-hour and weekly use, then when it is back.
    assert!(
        has_line(&text, &["acct1", "*", "95%", "*", "40%", "Back", "in", "42m"]),
        "{text}"
    );
    assert!(
        has_line(&text, &["acct2", "*", "20%", "*", "97%", "Back", "in", "1d"]),
        "{text}"
    );
    assert!(
        has_line(&text, &["acct3", "*", "10%", "*", "50%", "Ready"]),
        "{text}"
    );
    let lines: Vec<&str> = text.lines().collect();
    let next = lines
        .iter()
        .position(|line| line.contains("NEXT ACCOUNT"))
        .unwrap();
    assert_eq!(lines[next + 1].trim_start_matches(' '), "acct3", "{text}");
    assert!(text.contains("Usage poll: no baton running"), "{text}");
    // The accounts' windows, with where each reading came from.
    let usage = baton_at(&home, now, &["usage", "acct1"]).stdout;
    assert!(
        has_line(&usage, &["..", "5-hour", "..", "95%", "resets", "*", ".."]),
        "{usage}"
    );
    assert!(usage.contains("blocked"), "{usage}");
    let view = baton_at(&home, now, &["status", "--json"]).json();
    let blocks: Vec<Value> = view["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| json!([a["name"], a["blocked"], a["blockedBy"]]))
        .collect();
    assert_eq!(
        blocks,
        [
            json!(["acct1", true, ["five_hour"]]),
            json!(["acct2", true, ["seven_day"]]),
            json!(["acct3", false, []])
        ]
    );
    assert_eq!(view["accounts"][0]["availableAt"], iso(now + 42 * MINUTE));
    assert_eq!(
        view["next"],
        json!({ "account": "acct3", "id": "acct3", "reason": "urgency" })
    );
    write_usage(paths, "acct3", now, (99.0, 20 * MINUTE), (50.0, DAY));
    let blocked = baton_at(&home, now, &["status"]).stdout;
    assert!(blocked.contains("All accounts are at their limit"), "{blocked}");
    assert!(
        blocked.contains("acct3 returns in 19m") || blocked.contains("acct3 returns in 20m"),
        "{blocked}"
    );
    let pick = baton_at(&home, now, &["pick", "--json"]);
    assert_eq!(pick.code, 3);
    assert_eq!(pick.json()["earliest"], "acct3");
}

#[test]
fn stats_usage_per_day_refusals_hops_sessions_and_weekly_allowance_left_at_a_reset() {
    let home = temp_home();
    let paths = &home.paths;
    baton(&home, &["account", "add", "acct1"]);
    baton(&home, &["account", "add", "acct2"]);
    let now = now_ms();
    let at = |ago: Ms| iso(now - ago);
    let append = |kind: &str, ago: Ms, line: Value| {
        append_json_line(&history_file(paths, kind, now - ago), &line).unwrap();
    };
    let reset = iso(now - DAY);
    for (ago, used) in [(3 * DAY, 70), (2 * DAY, 80)] {
        append(
            "readings",
            ago,
            json!({ "at": at(ago), "account": "acct1", "window": "seven_day", "used": used, "resetsAt": reset }),
        );
    }
    // An hour ago, or at midnight when today is younger: on today's row.
    let day = local(now);
    let midnight = local_ms(day.year, day.month, day.day, 0, 0).unwrap();
    let today = HOUR.min(now - midnight);
    append(
        "readings",
        today,
        json!({ "at": at(today), "account": "acct1", "window": "five_hour", "used": 55, "resetsAt": at(-HOUR) }),
    );
    append(
        "events",
        HOUR,
        json!({ "at": at(HOUR), "type": "refusal", "account": "acct1", "window": "five_hour" }),
    );
    // The hop too is on today's row, whatever the hour.
    let hopped = 3_500_000.min(now - midnight);
    append(
        "events",
        hopped,
        json!({
            "at": at(hopped), "type": "hop", "session": "s",
            "from": "acct1", "to": "acct2", "reason": "refusal",
        }),
    );
    append(
        "events",
        2 * HOUR,
        json!({ "at": at(2 * HOUR), "type": "session", "account": "acct1", "source": "startup" }),
    );
    let stats = baton_at(&home, now, &["stats", "--json"]).json();
    let account = |name: &str| {
        stats["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .unwrap()
    };
    let d = account("acct1");
    assert_eq!(d["refusals"], 1);
    assert_eq!(d["sessions"], 1);
    assert_eq!(
        d["wasted"],
        json!([{ "resetAt": reset, "lastUsed": 80, "headroomLeft": 15, "unused": 20 }])
    );
    assert_eq!(d["daily"].as_array().unwrap().last().unwrap()["fiveHourMax"], 55);
    assert_eq!(stats["hopReasons"], json!({ "refusal": 1 }));
    let session = stats["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "s")
        .unwrap();
    let hops: Vec<Value> = session["hops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| json!([h["from"], h["to"], h["reason"]]))
        .collect();
    assert_eq!(hops, [json!(["acct1", "acct2", "refusal"])]);
    assert_eq!(
        session["costPerTurn"],
        json!({ "five_hour": null, "seven_day": null })
    );
    let hops_in: i64 = account("acct2")["daily"]
        .as_array()
        .unwrap()
        .iter()
        .map(|day| day["hopsIn"].as_i64().unwrap())
        .sum();
    assert_eq!(hops_in, 1);
    let text = baton_at(&home, now, &["stats"]).stdout;
    let unused = [
        "acct1",
        "???",
        "#",
        "20%",
        "unused",
        "15%",
        "under",
        "its",
        "threshold",
    ];
    assert!(has_line(&text, &unused), "{text}");
    let squashed = squash(&text);
    assert!(squashed.contains("Why hops 1 refusal"), "{text}");
    // The day's peak and the week, each under its day; an account with nothing says so once per row.
    assert!(squashed.contains("5h peak · · · · · · ▅ 55"), "{text}");
    assert!(squashed.contains("5h peak no readings"), "{text}");
    assert!(squashed.contains("Hops · · · · · · 1 1"), "{text}");
    assert_eq!(baton(&home, &["stats", "--days", "0"]).code, 2);
}

/// A usage report as Claude Code sends it at `at`: both windows, a model's own weekly limit, a broken
/// row, and the extra-usage spend.
fn report(at: Ms) -> Value {
    let meter = |kind: &str, group: &str, percent: f64, resets_in: Ms, active: bool| {
        json!({
            "kind": kind, "group": group, "percent": percent, "resets_at": iso(at + resets_in),
            "severity": "normal", "is_active": active,
        })
    };
    let mut scoped = meter("weekly_scoped", "weekly", 80.0, 48 * HOUR, false);
    scoped["scope"] = json!({ "model": { "display_name": "Fable" } });
    scoped["severity"] = json!("warning");
    json!({
        "session": {},
        "rate_limits": {
            "limits": [
                meter("session", "session", 42.0, 2 * HOUR, true),
                meter("weekly_all", "weekly", 63.0, 72 * HOUR, false),
                scoped,
                { "kind": "broken", "group": "weekly", "percent": null },
            ],
            "extra_usage": {
                "is_enabled": true, "monthly_limit": 5000, "used_credits": 1234,
                "utilization": 24.68, "currency": "USD",
            },
        },
    })
}

/// A poller with a fake read (every account answers [`report`]) and a clock that stands at `at`; the
/// accounts in `signed_out` are not signed in.
fn fake_poller(paths: &Paths, at: Ms, signed_out: &[&str]) -> Poller {
    let signed_out: Vec<String> = signed_out.iter().map(|id| (*id).to_owned()).collect();
    let mut options = PollerOptions::new(paths.clone(), test_env(&paths.home));
    options.now = Arc::new(move || at);
    options.launcher = Some(Launcher::program("fake-usage-claude"));
    options.auth = Some(Arc::new(move |id: &str, _: &Launcher| {
        Some(AuthStatus {
            ok: true,
            logged_in: !signed_out.iter().any(|out| out == id),
            ..AuthStatus::default()
        })
    }));
    options.probe = Some(Arc::new(|request: &ProbeRequest<'_>| {
        let at = (request.now)();
        usage_from_report(Some(&report(at)), "", at)
    }));
    Poller::new(options)
}

#[test]
fn a_poll_is_recorded_like_a_status_line_reading_and_baton_usage_says_where_each_reading_came_from() {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    // Real time here: `baton usage` reads the clock.
    let at = now_ms();
    let results = fake_poller(paths, at, &[]).refresh_wait(Some("acct1"), "request");
    assert_eq!(results[0].status, "ok");
    let stored = read_json(&paths.usage.join("acct1.json")).unwrap();
    assert_eq!(stored["source"], "poll");
    assert_eq!(stored["limitsAt"], iso(at));
    // A session's status line then reports the 5-hour window only.
    let five_hour = PerWindow {
        five_hour: Some(Sample {
            used: 50.0,
            resets_at: Some(iso(at + 2 * HOUR)),
        }),
        seven_day: None,
    };
    record_reading(paths, "acct1", &five_hour, RecordOptions::at(at));
    let after = read_json(&paths.usage.join("acct1.json")).unwrap();
    assert_eq!(after["five_hour"]["source"], "statusline");
    assert_eq!(after["seven_day"]["source"], "poll");
    assert_eq!(after["limits"].as_array().unwrap().len(), 3);
    let usage = baton_at(&home, at, &["usage"]).stdout;
    assert!(
        has_line(&usage, &["5-hour", "..", "50%", "resets", "*", ".."]),
        "{usage}"
    );
    // The week every model draws on, and Fable's own limit under its name: a bar each.
    assert!(
        has_line(&usage, &["weekly", "..", "63%", "resets", "*", ".."]),
        "{usage}"
    );
    assert!(
        has_line(&usage, &["Fable", "..", "80%", "resets", "*", ".."]),
        "{usage}"
    );
    let words = squash(&usage);
    assert!(!words.contains("Limits"), "{usage}");
    for said in [
        "Read 5-hour from session just now · weekly polled just now",
        "Extra usage on, 12.34 USD used of 50.00 USD this month",
        "Usage poll polled just now",
        "Usage poll: no baton running: usage is checked while baton or baton view runs.",
    ] {
        assert!(words.contains(said), "{said}: {usage}");
    }
    let status = baton_at(&home, at, &["status"]).stdout;
    assert!(status.contains("Usage poll: no baton running"), "{status}");
}

#[test]
fn a_signed_out_account_is_skipped_and_marked_and_baton_status_says_so() {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    add_account(paths, "acct2", &AddOptions::default()).unwrap();
    let t0 = parse_ms("2026-10-25T10:00:00Z").unwrap();
    let results = fake_poller(paths, t0, &["acct2"]).refresh_wait(None, "request");
    let outcomes: Vec<(&str, &str)> = results
        .iter()
        .map(|r| (r.id.as_str(), r.status.as_str()))
        .collect();
    assert_eq!(outcomes, [("acct1", "ok"), ("acct2", "signed-out")]);
    assert!(!paths.usage.join("acct2.json").exists());
    let status = baton(&home, &["status"]).stdout;
    assert!(status.contains("USAGE POLL"), "{status}");
    assert!(
        squash(&status).contains("acct2 signed out: not polled"),
        "{status}"
    );
    assert!(has_line(&status, &["acct2", "..", "Sign", "in"]), "{status}");
}

#[test]
fn on_request_baton_usage_refresh_polls_in_its_own_process_which_exits_when_it_is_done() {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    add_account(paths, "acct2", &AddOptions::default()).unwrap();
    let fake = bin(FAKE_USAGE_CLAUDE);
    let env = || env_of(&home).with("BATON_CLAUDE", fake.as_os_str());
    let all = baton_env(env(), &["usage", "--refresh"]);
    assert_eq!(all.code, 0, "{}", all.stderr);
    for account in ["acct1", "acct2"] {
        let polled = [
            "..", account, "5-hour", "42%", "·", "weekly", "63%", "polled", "now",
        ];
        assert!(has_line(&all.stdout, &polled), "{account}: {}", all.stdout);
    }
    let written = || read_json(&paths.usage.join("acct1.json")).unwrap()["at"].clone();
    let before = written();
    let one = baton_env(env(), &["usage", "--refresh", "acct2"]);
    assert_eq!(one.code, 0, "{}", one.stderr);
    assert!(!one.stdout.contains("acct1"), "{}", one.stdout);
    assert_eq!(written(), before);
    let signed_out = baton_env(
        env().with("FAKE_LOGGED_IN", "0"),
        &["usage", "--refresh", "acct2"],
    );
    assert!(
        has_line(
            &signed_out.stdout,
            &["..", "acct2", "signed", "out:", "not", "polled"]
        ),
        "{}",
        signed_out.stdout
    );
    assert_eq!(baton_env(env(), &["usage", "--refresh", "nobody"]).code, 2);
}

/// Polling that does nothing: the lease is what the test is about.
struct NoPolls;

impl PollWork for NoPolls {
    fn refresh(&self, _account: Option<&str>, _reason: &str) {}

    fn tick(&self) {}

    fn stop(&self) {}
}

#[test]
fn baton_status_says_which_instance_polls_and_that_none_does_after_the_last_one() {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let idle = "Usage poll: no baton running: usage is checked while baton or baton view runs.";
    assert!(baton(&home, &["status"]).stdout.contains(idle));
    // An instance of the kind a `baton` session is, in this process: it holds the lease.
    let mut options = PollHostOptions::new(paths.clone(), "start", test_env(&home.home));
    options.work = Some(Arc::new(NoPolls));
    let host = PollHost::new(options);
    assert!(host.beat());
    let status = baton(&home, &["status"]).stdout;
    let by = format!(
        "Usage poll: by baton (pid {}), every 30 min, next ",
        std::process::id()
    );
    assert!(status.contains(&by), "{status}");
    // The last instance exits: no lease, and status says nothing polls.
    host.stop();
    let status = baton(&home, &["status"]).stdout;
    assert!(status.contains(idle), "{status}");
}
