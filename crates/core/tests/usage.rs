//! Usage readings and refusals on disk, and what Claude Code's limit messages say of a reset.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot build its data stops the test at once, as the tests do"
)]

use std::fs;

use baton_core::clock::{DAY, HOUR, MINUTE, Ms, iso, local_ms};
use baton_core::fsutil::{read_json, read_json_lines};
use baton_core::paths::{Paths, baton_paths};
use baton_core::usage::{
    Inferred, Reading, RecordOptions, Recorded, Rejection, Rejections, Sample, history_file, infer_rejection,
    load_rejections, parse_reset_time, read_reading, reading_from_status_line, record_reading,
    record_rejection,
};
use baton_core::window::{Limit, PerWindow};
use serde_json::json;

/// The moment the tests run at: 10:00 local time on 2026-10-25.
fn at() -> Ms {
    local_ms(2026, 10, 25, 10, 0).unwrap()
}

/// A local time on 2026-10-25, or `day_offset` days after it.
fn local(hour: u32, minute: u32, day_offset: u32) -> Ms {
    local_ms(2026, 10, 25 + day_offset, hour, minute).unwrap()
}

/// A temporary home's paths; the folder goes when the first value is dropped.
fn home() -> (tempfile::TempDir, Paths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = baton_paths(dir.path());
    (dir, paths)
}

/// A reading of the 5-hour window alone.
fn five(used: f64, resets: &str) -> PerWindow<Option<Sample>> {
    PerWindow {
        five_hour: Some(Sample {
            used,
            resets_at: Some(resets.to_owned()),
        }),
        seven_day: None,
    }
}

/// A reading of the weekly window alone.
fn week(used: f64, resets: &str) -> PerWindow<Option<Sample>> {
    PerWindow {
        five_hour: None,
        seven_day: Some(Sample {
            used,
            resets_at: Some(resets.to_owned()),
        }),
    }
}

/// Whether a reading was found changed, and whether it was written.
fn outcome(recorded: &Recorded) -> (bool, bool) {
    (recorded.changed, recorded.written)
}

#[test]
fn status_line_rate_limits_become_a_reading_epoch_seconds_to_iso() {
    let reading = reading_from_status_line(&json!({
        "rate_limits": {
            "five_hour": { "used_percentage": 23.5, "resets_at": 1_792_920_000 },
            "seven_day": { "used_percentage": 41.2 },
        },
    }));
    assert_eq!(
        reading,
        Some(PerWindow {
            five_hour: Some(Sample {
                used: 23.5,
                resets_at: Some("2026-10-25T09:20:00.000Z".to_owned())
            }),
            seven_day: Some(Sample {
                used: 41.2,
                resets_at: None
            }),
        })
    );
    assert_eq!(reading_from_status_line(&json!({})), None);
    assert_eq!(
        reading_from_status_line(&json!({ "rate_limits": { "five_hour": { "used_percentage": "x" } } })),
        None
    );
    assert_eq!(reading_from_status_line(&json!(null)), None);
    // One window is enough.
    let only_week =
        reading_from_status_line(&json!({ "rate_limits": { "seven_day": { "used_percentage": "7" } } }))
            .unwrap();
    assert_eq!(only_week.five_hour, None);
    assert_eq!(
        only_week.seven_day,
        Some(Sample {
            used: 7.0,
            resets_at: None
        })
    );
}

#[test]
fn readings_written_on_change_or_when_old_absent_windows_kept_changes_appended_to_the_history() {
    let (_dir, paths) = home();
    let at = at();
    let first = record_reading(
        &paths,
        "a",
        &five(10.0, "R1"),
        RecordOptions {
            session: Some("s1".to_owned()),
            ..RecordOptions::at(at)
        },
    );
    assert_eq!(outcome(&first), (true, true));
    let same = record_reading(&paths, "a", &five(10.0, "R1"), RecordOptions::at(at + 1000));
    assert_eq!(outcome(&same), (false, false));
    assert_eq!(same.reading, None);
    let refresh = record_reading(&paths, "a", &five(10.0, "R1"), RecordOptions::at(at + 31_000));
    assert_eq!(outcome(&refresh), (false, true));
    record_reading(&paths, "a", &week(50.0, "W1"), RecordOptions::at(at + 40_000));
    let stored = read_reading(&paths, "a").unwrap();
    assert_eq!(stored.five_hour.as_ref().unwrap().used, 10.0);
    assert_eq!(stored.seven_day.as_ref().unwrap().used, 50.0);
    assert_eq!(
        stored.five_hour.as_ref().unwrap().read_at,
        Some(iso(at + 31_000)),
        "when it was last read"
    );
    assert_eq!(stored.at, Some(iso(at + 40_000)));
    let history = read_json_lines(&history_file(&paths, "readings", at));
    let changes: Vec<_> = history
        .iter()
        .map(|h| (h["window"].as_str().unwrap(), h["used"].as_f64().unwrap()))
        .collect();
    assert_eq!(changes, [("five_hour", 10.0), ("seven_day", 50.0)]);
    assert_eq!(
        history[0],
        json!({
            "at": iso(at), "account": "a", "window": "five_hour", "used": 10,
            "resetsAt": "R1", "session": "s1", "source": "statusline",
        })
    );
    assert!(!record_reading(&paths, "../evil", &five(1.0, "R1"), RecordOptions::at(at)).written);
    let left: Vec<String> = fs::read_dir(&paths.usage)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        ["a.json"],
        "one file per account, and no temporary file left behind"
    );
    // A change of value, or a new window period, is a change; a reset time moved by a second is not.
    let moved = PerWindow {
        five_hour: Some(Sample {
            used: 10.0,
            resets_at: Some("R2".to_owned()),
        }),
        seven_day: None,
    };
    assert_eq!(
        outcome(&record_reading(
            &paths,
            "a",
            &moved,
            RecordOptions::at(at + 41_000)
        )),
        (true, true)
    );
    assert_eq!(
        outcome(&record_reading(
            &paths,
            "a",
            &five(12.0, "R2"),
            RecordOptions::at(at + 42_000)
        )),
        (true, true)
    );
    // A reading of no window at all changes nothing.
    let nothing = PerWindow {
        five_hour: None,
        seven_day: None,
    };
    assert_eq!(
        outcome(&record_reading(
            &paths,
            "a",
            &nothing,
            RecordOptions::at(at + 43_000)
        )),
        (false, false)
    );
}

#[test]
fn refusals_are_stored_per_account_and_window() {
    let (_dir, paths) = home();
    record_rejection(
        &paths,
        "a",
        &Rejection {
            window: "five_hour".to_owned(),
            at: Some("T1".to_owned()),
            until: Some("U1".to_owned()),
            session: Some("s".to_owned()),
            ..Rejection::default()
        },
    );
    record_rejection(
        &paths,
        "a",
        &Rejection {
            window: "seven_day".to_owned(),
            at: Some("T2".to_owned()),
            ..Rejection::default()
        },
    );
    record_rejection(
        &paths,
        "b",
        &Rejection {
            window: "bogus".to_owned(),
            at: Some("T3".to_owned()),
            ..Rejection::default()
        },
    );
    record_rejection(
        &paths,
        "../evil",
        &Rejection {
            window: "five_hour".to_owned(),
            ..Rejection::default()
        },
    );
    let ids = ["a", "b", "c"].map(str::to_owned);
    let all = load_rejections(&paths, &ids);
    assert_eq!(all["a"].five_hour.as_ref().unwrap().until.as_deref(), Some("U1"));
    assert_eq!(all["a"].seven_day.as_ref().unwrap().until, None);
    assert_eq!(all["b"].unknown.as_ref().unwrap().at.as_deref(), Some("T3"));
    assert_eq!(all["b"].get(Limit::Unknown).unwrap().window, "unknown");
    assert_eq!(all["c"], Rejections::default());
    // The file as it has always been written.
    assert_eq!(
        read_json(&paths.rejected.join("a.five_hour.json")),
        Some(json!({
            "account": "a", "window": "five_hour", "at": "T1", "until": "U1",
            "untilSource": "none", "error": null, "session": "s", "message": null,
        }))
    );
    let files: Vec<_> = fs::read_dir(&paths.rejected).unwrap().flatten().collect();
    assert_eq!(files.len(), 3);
    // Only the accounts asked for.
    assert!(!load_rejections(&paths, &ids[..1]).contains_key("b"));
}

#[test]
fn reset_times_in_claude_codes_limit_messages() {
    let at = at();
    assert_eq!(
        parse_reset_time("You've hit your session limit · resets 3pm", at),
        Some(local(15, 0, 0))
    );
    assert_eq!(
        parse_reset_time("limit reached · resets 3:20pm (Europe/Athens)", at),
        Some(local(15, 20, 0))
    );
    assert_eq!(parse_reset_time("resets at 15:20", at), Some(local(15, 20, 0)));
    assert_eq!(parse_reset_time("resets 9am", at), Some(local(9, 0, 1))); // already past today
    assert_eq!(
        parse_reset_time("You've hit your weekly limit · resets Oct 27, 9am", at),
        Some(local(9, 0, 2))
    );
    assert_eq!(parse_reset_time("resets in 2h 30m", at), Some(at + 150 * MINUTE));
    assert_eq!(parse_reset_time("resets in 45 min", at), Some(at + 45 * MINUTE));
    assert_eq!(parse_reset_time("API Error: Rate limit reached", at), None);
    assert_eq!(parse_reset_time("resets 7", at), None);
    assert_eq!(parse_reset_time("", at), None);
}

#[test]
fn refusal_inference_the_window_from_the_message_or_the_reading_the_end_from_the_message_or_the_reading() {
    let at = at();
    let five_resets = iso(at + HOUR);
    let week_resets = iso(at + 3 * DAY);
    let reading: Reading = serde_json::from_value(json!({
        "five_hour": { "used": 100, "resetsAt": five_resets },
        "seven_day": { "used": 60, "resetsAt": week_resets },
    }))
    .unwrap();
    let infer = |error: &str, message: &str, reading: Option<&Reading>| {
        infer_rejection(Some(error), Some(message), None, reading, at)
    };
    assert_eq!(
        infer(
            "rate_limit",
            "You've hit your session limit · resets 3pm",
            Some(&reading)
        ),
        Inferred {
            window: Limit::FiveHour,
            until: Some(iso(local(15, 0, 0))),
            until_source: "message"
        }
    );
    assert_eq!(
        infer("rate_limit", "You've hit your weekly limit", Some(&reading)),
        Inferred {
            window: Limit::SevenDay,
            until: Some(week_resets),
            until_source: "reading"
        }
    );
    assert_eq!(
        infer("rate_limit", "API Error: Rate limit reached", Some(&reading)),
        Inferred {
            window: Limit::FiveHour,
            until: Some(five_resets),
            until_source: "reading"
        }
    );
    assert_eq!(
        infer("rate_limit", "Opus limit reached", None).window,
        Limit::SevenDay
    );
    assert_eq!(
        infer("billing_error", "credit balance too low", None),
        Inferred {
            window: Limit::Unknown,
            until: None,
            until_source: "none"
        }
    );
    // The details count like the message, and no words at all leave it to the reading.
    assert_eq!(
        infer_rejection(
            Some("rate_limit"),
            None,
            Some("weekly limit · resets in 2h"),
            Some(&reading),
            at
        ),
        Inferred {
            window: Limit::SevenDay,
            until: Some(iso(at + 2 * HOUR)),
            until_source: "message"
        }
    );
    assert_eq!(
        infer_rejection(Some("rate_limit"), None, None, Some(&reading), at).window,
        Limit::FiveHour
    );
    assert_eq!(
        infer_rejection(None, None, None, Some(&reading), at).window,
        Limit::Unknown
    );
}
