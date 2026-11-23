//! Readable text (and the JSON shapes) for the CLI and the menu. Times read one way everywhere
//! ([`crate::text::when`], [`crate::text::span`]), and the JSON carries moments and numbers, never
//! text that was made for a screen.

use serde_json::{Value, json};

use crate::clock::{MINUTE, Ms, iso, iso_opt, parse_ms};
use crate::config::Policy;
use crate::json::{Object, num, opt_num};
use crate::paths::profile_dir;
use crate::policy::{AccountState, WindowState, pick_start};
use crate::poll::{PollEntry, kind_words};
use crate::state::{PollSnapshot, Snapshot};
use crate::text::{span, when};
use crate::usage::{Meter, Reading};

/// What to say when the operator has not added an account yet.
pub const NO_ACCOUNTS: &str = "No accounts yet: add one with baton account add <name> (or a in the menu), then sign it in with baton login <name>.";

/// What to say when no baton instance runs to poll the usage.
pub const NO_POLLER: &str = "no baton running: usage is checked while baton or baton view runs";

/// How long ago `t` was: `never`, `just now`, `12m ago`.
fn ago(t: Option<Ms>, now: Ms) -> String {
    match t {
        None => "never".to_owned(),
        Some(t) if now - t < MINUTE => "just now".to_owned(),
        Some(t) => format!("{} ago", span(Some(now - t))),
    }
}

/// Where a reading came from, and how old it is: "polled 12m ago" (the usage poller), "from session 2m
/// ago" (a session's status line).
pub fn read_words(source: Option<&str>, read_at: Option<Ms>, now: Ms) -> String {
    let Some(at) = read_at else {
        return "never read".to_owned();
    };
    match source {
        Some("poll") => format!("polled {}", ago(Some(at), now)),
        Some("statusline") => format!("from session {}", ago(Some(at), now)),
        _ => format!("read {}", ago(Some(at), now)),
    }
}

/// One window of an account for `baton status --json`: moments and numbers, no text.
fn window_view(w: &WindowState) -> Value {
    let mut out = Object::new();
    out.insert("used".to_owned(), opt_num(w.used));
    out.insert("threshold".to_owned(), num(w.threshold));
    out.insert("headroom".to_owned(), num(w.headroom));
    out.insert("resetsAt".to_owned(), json!(iso_opt(w.resets_at)));
    out.insert("readAt".to_owned(), json!(iso_opt(w.read_at)));
    out.insert("source".to_owned(), json!(w.source));
    if let Some(label) = w.label.as_deref().filter(|label| !label.is_empty()) {
        out.insert("limit".to_owned(), json!(label));
    }
    out.insert("reset".to_owned(), json!(w.reset));
    out.insert("stale".to_owned(), json!(w.stale));
    if w.absent {
        out.insert("absent".to_owned(), json!(true));
    }
    out.insert("blocked".to_owned(), json!(w.blocked));
    out.insert("rejected".to_owned(), json!(w.rejected));
    out.insert("rejectedUntil".to_owned(), json!(iso_opt(w.rejected_until)));
    Value::Object(out)
}

/// The poller's last word on an account, in words.
pub fn poll_words(entry: Option<&PollEntry>, now: Ms) -> String {
    let Some((entry, status)) = entry.and_then(|e| Some((e, e.status.as_deref().filter(|s| !s.is_empty())?)))
    else {
        return "not polled yet".to_owned();
    };
    let asked = ago(entry.at.as_deref().and_then(parse_ms), now);
    let reason = entry
        .reason
        .as_deref()
        .filter(|r| !r.is_empty())
        .map(|r| format!(": {r}"))
        .unwrap_or_default();
    match status {
        "ok" => format!("polled {asked}"),
        "unavailable" => format!("usage unavailable{reason} ({asked})"),
        "rate-limited" => {
            let again = match entry.backoff_until.as_deref().and_then(parse_ms) {
                Some(until) => format!("{} (in {})", when(Some(until), now), span(Some(until - now))),
                None => "later".to_owned(),
            };
            format!("usage endpoint rate limited ({asked}); asks again {again}")
        }
        "signed-out" => "signed out: not polled".to_owned(),
        "timeout" => format!("poll timed out ({asked})"),
        other => format!("poll {other}{reason} ({asked})"),
    }
}

/// The plan's meters beyond the two windows (a model's own weekly limit), from the last poll, that
/// have not reset.
pub fn other_limits(reading: Option<&Reading>, now: Ms) -> Vec<&Meter> {
    let Some(limits) = reading.and_then(|r| r.limits.as_ref()) else {
        return Vec::new();
    };
    limits
        .iter()
        .filter(|r| r.kind != "session" && r.kind != "weekly_all")
        .filter(|r| {
            r.resets_at
                .as_deref()
                .and_then(parse_ms)
                .is_none_or(|resets| resets > now)
        })
        .collect()
}

/// The usage poll in one line: which baton instance polls, the schedule, the next run. None for a
/// snapshot read without the poll.
pub fn poll_line(snapshot: &Snapshot) -> Option<String> {
    poll_line_of(
        snapshot.now,
        snapshot.poll.as_ref(),
        &snapshot.policy,
        &snapshot.states,
    )
}

/// [`poll_line`] from its parts: the time, the poll as it was read, the policy and the accounts.
pub fn poll_line_of(
    now: Ms,
    poll: Option<&PollSnapshot>,
    policy: &Policy,
    states: &[AccountState],
) -> Option<String> {
    let poll = poll?;
    let every = policy.usage_poll_minutes;
    let Some(holder) = &poll.holder else {
        return Some(format!("Usage poll: {NO_POLLER}."));
    };
    let next = match poll.next_at.filter(|at| *at != 0) {
        Some(at) => format!("next {} (in {})", when(Some(at), now), span(Some(at - now))),
        None => "only on request (usagePollMinutes 0)".to_owned(),
    };
    let last = match poll.state.last_run_at.as_deref().filter(|at| !at.is_empty()) {
        Some(at) => format!("last {}", when(parse_ms(at), now)),
        None => "none yet".to_owned(),
    };
    let running = match &poll.state.running {
        Some(running) => {
            let name = states
                .iter()
                .find(|s| s.id == running.account)
                .map_or(running.account.as_str(), |s| s.name.as_str());
            format!(" · polling {name} now")
        }
        None => String::new(),
    };
    let by = format!("by {} (pid {})", kind_words(&holder.kind), holder.pid);
    let schedule = if every > 0.0 {
        format!("every {}, ", span(Some((every * MINUTE as f64) as Ms)))
    } else {
        String::new()
    };
    Some(format!("Usage poll: {by}, {schedule}{next}; {last}{running}"))
}

/// The conversations on an account: the ones a wrapper runs, then the ones only seen.
fn sessions_on(snapshot: &Snapshot, id: &str) -> Vec<Value> {
    use crate::json::Get;
    let mut out = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for w in snapshot.wrappers.iter().flatten() {
        if let (Some(account), Some(session)) = (w.text("account"), w.word("session"))
            && account == id
        {
            out.push(json!({ "id": session, "by": "baton", "state": w.at("state"), "pid": w.at("pid") }));
            seen.push(session);
        }
    }
    for s in snapshot.sessions.iter().flatten() {
        if s.account.as_deref() == Some(id) && !seen.contains(&s.id.as_str()) {
            out.push(json!({ "id": s.id, "by": "seen", "state": s.state, "at": iso(s.at) }));
        }
    }
    out
}

/// Everything `baton status` shows, as JSON.
pub fn status_view(snapshot: &Snapshot) -> Value {
    let now = snapshot.now;
    let states = &snapshot.states;
    let next = pick_start(states, &snapshot.policy, &[]);
    let label = |id: &str| {
        states
            .iter()
            .find(|s| s.id == id)
            .map_or_else(|| id.to_owned(), |s| s.name.clone())
    };
    let accounts: Vec<Value> = states
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut out = Object::new();
            out.insert("order".to_owned(), json!(i + 1));
            out.insert("id".to_owned(), json!(s.id));
            out.insert("name".to_owned(), json!(s.name));
            out.insert("mark".to_owned(), json!(s.mark.as_str()));
            out.insert(
                "profile".to_owned(),
                json!(profile_dir(&snapshot.paths, &s.id).to_string_lossy()),
            );
            out.insert("five_hour".to_owned(), window_view(&s.windows.five_hour));
            out.insert("seven_day".to_owned(), window_view(&s.windows.seven_day));
            out.insert("blocked".to_owned(), json!(s.blocked));
            out.insert("blockedBy".to_owned(), json!(s.blocked_by));
            // Blocked by nothing but these models' own limits: other models still run here.
            if !s.limited_models.is_empty() {
                out.insert("limitedModels".to_owned(), json!(s.limited_models));
                out.insert("week".to_owned(), window_view(&s.weekly_all));
            }
            out.insert(
                "availableAt".to_owned(),
                json!(if s.available_known {
                    iso_opt(s.available_at)
                } else {
                    None
                }),
            );
            out.insert("availableKnown".to_owned(), json!(s.available_known));
            out.insert("urgency".to_owned(), num((s.urgency * 100.0).round() / 100.0));
            out.insert("sessions".to_owned(), Value::Array(sessions_on(snapshot, &s.id)));
            if let Some(poll) = &snapshot.poll {
                let reading = snapshot.readings.get(&s.id);
                out.insert("poll".to_owned(), json!(poll.state.accounts.get(&s.id)));
                out.insert("limits".to_owned(), json!(other_limits(reading, now)));
                out.insert(
                    "extraUsage".to_owned(),
                    json!(reading.and_then(|r| r.extra_usage.as_ref())),
                );
            }
            Value::Object(out)
        })
        .collect();
    let mut out = Object::new();
    out.insert("now".to_owned(), json!(iso(now)));
    out.insert("mode".to_owned(), json!(snapshot.policy.mode.as_str()));
    out.insert("accounts".to_owned(), Value::Array(accounts));
    if let Some(poll) = &snapshot.poll {
        out.insert(
            "poll".to_owned(),
            json!({
                "holder": poll.holder,
                "everyMinutes": num(snapshot.policy.usage_poll_minutes),
                "lastRunAt": poll.state.last_run_at,
                "nextAt": iso_opt(poll.next_at),
                "running": poll.state.running,
            }),
        );
    }
    // Names for reading, ids alongside.
    out.insert(
        "next".to_owned(),
        match &next.account {
            Some(account) => json!({ "account": label(account), "id": account, "reason": next.reason }),
            None => json!({
                "account": null,
                "allBlocked": true,
                "earliest": next.earliest.as_deref().map(label),
                "earliestId": next.earliest,
                "availableAt": if next.available_known { iso_opt(next.available_at) } else { None },
                "availableKnown": next.available_known,
            }),
        },
    );
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{HOUR, local_ms};
    use crate::fsutil::write_json_atomic;
    use crate::paths::baton_paths;
    use crate::poll::{lease_file, poll_file};
    use crate::state::{Want, load_snapshot};
    use crate::usage::{RecordOptions, Sample, record_reading};
    use crate::window::PerWindow;

    #[test]
    fn where_a_reading_came_from() {
        let now = 10 * HOUR;
        assert_eq!(
            read_words(Some("poll"), Some(now - 12 * MINUTE), now),
            "polled 12m ago"
        );
        assert_eq!(
            read_words(Some("statusline"), Some(now - 5000), now),
            "from session just now"
        );
        assert_eq!(read_words(None, Some(now - 2 * HOUR), now), "read 2h ago");
        assert_eq!(read_words(Some("poll"), None, now), "never read");
    }

    #[test]
    fn the_pollers_last_word() {
        let now = local_ms(2026, 3, 10, 12, 0).unwrap();
        let entry = |status: &str| PollEntry {
            at: Some(iso(now - 5 * MINUTE)),
            status: Some(status.to_owned()),
            reason: Some("why".to_owned()),
            ..PollEntry::default()
        };
        assert_eq!(poll_words(None, now), "not polled yet");
        assert_eq!(poll_words(Some(&PollEntry::default()), now), "not polled yet");
        assert_eq!(poll_words(Some(&entry("ok")), now), "polled 5m ago");
        assert_eq!(
            poll_words(Some(&entry("unavailable")), now),
            "usage unavailable: why (5m ago)"
        );
        assert_eq!(
            poll_words(Some(&entry("signed-out")), now),
            "signed out: not polled"
        );
        assert_eq!(
            poll_words(Some(&entry("timeout")), now),
            "poll timed out (5m ago)"
        );
        assert_eq!(
            poll_words(Some(&entry("failed")), now),
            "poll failed: why (5m ago)"
        );
        let limited = PollEntry {
            backoff_until: Some(iso(now + 10 * MINUTE)),
            ..entry("rate-limited")
        };
        assert_eq!(
            poll_words(Some(&limited), now),
            "usage endpoint rate limited (5m ago); asks again 12:10 (in 10m)"
        );
        assert_eq!(
            poll_words(Some(&entry("rate-limited")), now),
            "usage endpoint rate limited (5m ago); asks again later"
        );
    }

    #[test]
    fn the_status_view() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let now = local_ms(2026, 3, 10, 12, 0).unwrap();
        write_json_atomic(
            &paths.config,
            &json!({ "accounts": [{ "id": "work", "name": "Work", "mark": "preferred" }, { "id": "spare", "name": "Spare" }] }),
        )
        .unwrap();
        let reading = PerWindow {
            five_hour: Some(Sample {
                used: 42.25,
                resets_at: Some(iso(now + 2 * HOUR)),
            }),
            seven_day: None,
        };
        record_reading(&paths, "work", &reading, RecordOptions::at(now - 2 * MINUTE));
        let snapshot = load_snapshot(
            &paths,
            now,
            Want {
                sessions: true,
                poll: true,
            },
        );
        assert_eq!(
            poll_line(&snapshot).as_deref(),
            Some("Usage poll: no baton running: usage is checked while baton or baton view runs.")
        );
        let view = status_view(&snapshot);
        assert_eq!(view["mode"], "smart");
        assert_eq!(
            view["next"],
            json!({ "account": "Work", "id": "work", "reason": "preferred" })
        );
        let work = &view["accounts"][0];
        assert_eq!(work["order"], 1);
        assert_eq!(work["five_hour"]["used"], json!(42.25));
        assert_eq!(work["five_hour"]["threshold"], json!(90));
        // Moments and numbers: what a screen says of them is the screen's to say.
        assert_eq!(work["five_hour"]["resetsAt"], json!(iso(now + 2 * HOUR)));
        assert_eq!(work["five_hour"]["source"], "statusline");
        assert_eq!(work["five_hour"]["text"], Value::Null);
        assert_eq!(work["seven_day"]["used"], Value::Null);
        assert_eq!(work["blockedBy"], json!([]));
        assert_eq!(work["availableAt"], json!(iso(now)));
        assert_eq!(work["poll"], Value::Null);
        assert_eq!(view["poll"]["everyMinutes"], json!(30));
        assert_eq!(view["poll"]["holder"], Value::Null);

        // An instance that polls, and the schedule in one line.
        write_json_atomic(
            &lease_file(&paths),
            &json!({ "pid": std::process::id(), "kind": "menu", "instance": "i", "since": iso(now), "heartbeatAt": iso(now) }),
        )
        .unwrap();
        write_json_atomic(
            &poll_file(&paths),
            &json!({ "lastRunAt": iso(now - 10 * MINUTE), "running": { "account": "spare" } }),
        )
        .unwrap();
        let snapshot = load_snapshot(
            &paths,
            now,
            Want {
                sessions: false,
                poll: true,
            },
        );
        assert_eq!(
            poll_line(&snapshot).unwrap(),
            format!(
                "Usage poll: by the menu (pid {}), every 30m, next 12:20 (in 20m); last 11:50 · polling Spare now",
                std::process::id()
            )
        );
        assert_eq!(poll_line(&load_snapshot(&paths, now, Want::default())), None);
    }
}
