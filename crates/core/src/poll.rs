//! What the usage poll leaves on disk, for everyone who reads it: its state, the lease of the instance
//! that polls, and the requests a command leaves for it.
//!
//! The poll runs only while a baton instance the operator started runs (there is no background
//! service): the menu (`baton view`) and every `baton` session wrapper carry a poll host, and exactly
//! one of them, the holder of the poll lease, polls on the schedule.
//!
//! ```text
//! ~/.baton/poll/state.json   { lastRunAt, running: { account, since } | null,
//!                            accounts: { <id>: { at, status, reason, okAt, backoffUntil,
//!                            backoffMs, failures } } }
//! ~/.baton/poll/lease.json   { pid, kind ("menu" | "start"), instance, since, heartbeatAt }:
//!                            renewed every heartbeat (3 s); a lease whose process is gone, or not
//!                            renewed for 10 s, is taken by another live instance
//! ~/.baton/poll/requests/    a poll asked for by a command (an account added, renamed or signed
//!                            in): the holder takes it at its next heartbeat
//! ```
//!
//! status is ok, unavailable (the plan usage could not be fetched; the reason says why), rate-limited
//! (the usage endpoint refused: Baton waits until the time it named, else 5 minutes, doubling up to 2
//! hours, before asking that account again), timeout, failed or signed-out (not asked).

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::clock::{HOUR, MINUTE, Ms, iso, parse_ms};
use crate::config::Policy;
use crate::fsutil::{is_alive, random_hex, read_json_as, write_json_atomic};
use crate::json::{lenient, ser_opt_num};
use crate::paths::Paths;

/// The first wait after the usage endpoint refused without naming a time.
pub const BACKOFF_FIRST_MS: Ms = 5 * MINUTE;
/// The longest such wait.
pub const BACKOFF_MAX_MS: Ms = 2 * HOUR;
/// How often an instance renews (or tries to take) the lease.
pub const HEARTBEAT_MS: Ms = 3_000;
/// A lease not renewed for this long is free.
pub const LEASE_STALE_MS: Ms = 10_000;

/// A baton instance by what the operator ran: `menu` is "the menu", `start` is "baton".
pub fn kind_words(kind: &str) -> &str {
    match kind {
        "menu" => "the menu",
        "start" => "baton",
        other => other,
    }
}

/// `~/.baton/poll`.
pub fn poll_dir(paths: &Paths) -> PathBuf {
    paths.root.join("poll")
}

/// The poll's state.
pub fn poll_file(paths: &Paths) -> PathBuf {
    poll_dir(paths).join("state.json")
}

/// The lease of the instance that polls.
pub fn lease_file(paths: &Paths) -> PathBuf {
    poll_dir(paths).join("lease.json")
}

/// Where a command leaves a poll it asks for.
pub fn request_dir(paths: &Paths) -> PathBuf {
    poll_dir(paths).join("requests")
}

/// The poller's last word on one account.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PollEntry {
    /// When the account was last asked.
    #[serde(default, deserialize_with = "lenient")]
    pub at: Option<String>,
    /// What came of it: ok, unavailable, rate-limited, timeout, failed, signed-out.
    #[serde(default, deserialize_with = "lenient")]
    pub status: Option<String>,
    /// Why, for anything but ok.
    #[serde(default, deserialize_with = "lenient")]
    pub reason: Option<String>,
    /// When it last answered.
    #[serde(rename = "okAt", default, deserialize_with = "lenient")]
    pub ok_at: Option<String>,
    /// Not asked again before this.
    #[serde(rename = "backoffUntil", default, deserialize_with = "lenient")]
    pub backoff_until: Option<String>,
    /// The current wait, when the endpoint named no time.
    #[serde(
        rename = "backoffMs",
        default,
        deserialize_with = "lenient",
        serialize_with = "ser_opt_num"
    )]
    pub backoff_ms: Option<f64>,
    /// Failures in a row.
    #[serde(default, deserialize_with = "lenient")]
    pub failures: Option<u32>,
}

/// The account being asked now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Running {
    /// The account's id.
    #[serde(default)]
    pub account: String,
    /// Since when.
    #[serde(default, deserialize_with = "lenient")]
    pub since: Option<String>,
    /// Why it is asked: schedule, request, a command's reason.
    #[serde(default, deserialize_with = "lenient")]
    pub reason: Option<String>,
}

/// `poll/state.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PollState {
    /// When every account was last asked.
    #[serde(rename = "lastRunAt", default, deserialize_with = "lenient")]
    pub last_run_at: Option<String>,
    /// The account being asked now.
    #[serde(default, deserialize_with = "lenient")]
    pub running: Option<Running>,
    /// The last word on each account, by id.
    #[serde(default, deserialize_with = "lenient_entries")]
    pub accounts: BTreeMap<String, PollEntry>,
}

/// Reads the accounts' entries, leaving out what is not one.
fn lenient_entries<'de, D>(deserializer: D) -> Result<BTreeMap<String, PollEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let serde_json::Value::Object(entries) = serde_json::Value::deserialize(deserializer)? else {
        return Ok(BTreeMap::new());
    };
    Ok(entries
        .into_iter()
        .filter(|(_, entry)| entry.is_object())
        .filter_map(|(id, entry)| Some((id, serde_json::from_value(entry).ok()?)))
        .collect())
}

/// The poll's state; empty when there is none.
pub fn read_poll_state(paths: &Paths) -> PollState {
    read_json_as(&poll_file(paths)).unwrap_or_default()
}

/// Replaces the poll's state.
pub fn write_poll_state(paths: &Paths, state: &PollState) -> io::Result<()> {
    write_json_atomic(&poll_file(paths), state)
}

/// How often a running instance polls; None when it only polls on request.
pub fn poll_every_ms(policy: &Policy) -> Option<Ms> {
    (policy.usage_poll_minutes > 0.0).then_some((policy.usage_poll_minutes * MINUTE as f64) as Ms)
}

/// When the next scheduled poll runs: the last run plus the interval (now, when none ran yet), or None
/// when no baton instance runs (`running`: false) or the interval is 0.
pub fn next_poll_at(poll: &PollState, policy: &Policy, running: bool, now: Ms) -> Option<Ms> {
    let every = poll_every_ms(policy)?;
    if !running {
        return None;
    }
    Some(match poll.last_run_at.as_deref().and_then(parse_ms) {
        Some(last) => now.max(last + every),
        None => now,
    })
}

/// `poll/lease.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Lease {
    /// The holder's process.
    #[serde(default, deserialize_with = "lenient")]
    pub pid: Option<i64>,
    /// What the operator ran: `menu` or `start`.
    #[serde(default, deserialize_with = "lenient")]
    pub kind: Option<String>,
    /// The holder's own id.
    #[serde(default, deserialize_with = "lenient")]
    pub instance: Option<String>,
    /// Since when it holds the lease.
    #[serde(default, deserialize_with = "lenient")]
    pub since: Option<String>,
    /// When it last renewed it.
    #[serde(rename = "heartbeatAt", default, deserialize_with = "lenient")]
    pub heartbeat_at: Option<String>,
}

/// The lease as it is on disk.
pub fn read_lease(paths: &Paths) -> Option<Lease> {
    read_json_as(&lease_file(paths))
}

/// Whether a lease is in force: its process is alive and it was renewed within `stale_ms`.
pub fn lease_live(lease: Option<&Lease>, now: Ms, alive: &dyn Fn(i64) -> bool, stale_ms: Ms) -> bool {
    let Some(lease) = lease else {
        return false;
    };
    lease.pid.is_some_and(alive)
        && now - lease.heartbeat_at.as_deref().and_then(parse_ms).unwrap_or(0) < stale_ms
}

/// Who polls now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Holder {
    /// What the operator ran: `menu` or `start`.
    pub kind: String,
    /// Its process.
    pub pid: i64,
    /// Since when it polls.
    pub since: Option<String>,
}

/// Who polls now, or None (no baton instance runs).
pub fn poll_holder(paths: &Paths, now: Ms) -> Option<Holder> {
    poll_holder_with(paths, now, &is_alive)
}

/// [`poll_holder`] with the liveness check given.
pub fn poll_holder_with(paths: &Paths, now: Ms, alive: &dyn Fn(i64) -> bool) -> Option<Holder> {
    let lease = read_lease(paths)?;
    if !lease_live(Some(&lease), now, alive, LEASE_STALE_MS) {
        return None;
    }
    Some(Holder {
        kind: lease.kind.unwrap_or_default(),
        pid: lease.pid?,
        since: lease.since,
    })
}

/// A poll a command asks for (an account added, renamed or signed in). With no instance running,
/// nothing polls; the next scheduled poll reads the account anyway.
pub fn request_poll(paths: &Paths, account: &str, reason: &str, at: Ms) {
    let file = request_dir(paths).join(format!("{at}-{}.json", random_hex(3)));
    let _ = write_json_atomic(
        &file,
        &json!({ "account": account, "reason": reason, "at": iso(at) }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::read_json;
    use crate::paths::baton_paths;

    #[test]
    fn the_state_reads_whatever_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        assert_eq!(read_poll_state(&paths), PollState::default());
        write_json_atomic(
            &poll_file(&paths),
            &json!({
                "lastRunAt": 5,
                "running": { "account": "work", "since": "2026-01-01T00:00:00.000Z" },
                "accounts": { "work": { "status": "ok", "failures": 0, "backoffMs": 300000 }, "odd": 3 }
            }),
        )
        .unwrap();
        let state = read_poll_state(&paths);
        assert_eq!(state.last_run_at, None);
        assert_eq!(state.running.as_ref().unwrap().account, "work");
        assert_eq!(state.accounts.len(), 1);
        assert_eq!(state.accounts["work"].status.as_deref(), Some("ok"));
        assert_eq!(state.accounts["work"].backoff_ms, Some(300_000.0));
        write_poll_state(&paths, &state).unwrap();
        assert_eq!(read_poll_state(&paths), state);
        assert_eq!(
            read_json(&poll_file(&paths)).unwrap()["accounts"]["work"]["backoffMs"],
            json!(300000)
        );
    }

    #[test]
    fn the_next_poll() {
        let policy = Policy::default();
        let mut state = PollState::default();
        assert_eq!(next_poll_at(&state, &policy, true, 1000), Some(1000));
        assert_eq!(next_poll_at(&state, &policy, false, 1000), None);
        state.last_run_at = Some(iso(1000));
        assert_eq!(
            next_poll_at(&state, &policy, true, 2000),
            Some(1000 + 30 * MINUTE)
        );
        assert_eq!(
            next_poll_at(&state, &policy, true, 1000 + HOUR),
            Some(1000 + HOUR)
        );
        let never = Policy {
            usage_poll_minutes: 0.0,
            ..Policy::default()
        };
        assert_eq!(poll_every_ms(&never), None);
        assert_eq!(next_poll_at(&state, &never, true, 2000), None);
    }

    #[test]
    fn who_polls() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let now = 1_000_000;
        assert_eq!(poll_holder(&paths, now), None);
        write_json_atomic(
            &lease_file(&paths),
            &json!({ "pid": 42, "kind": "menu", "instance": "i", "since": iso(now - 60_000), "heartbeatAt": iso(now - 2000) }),
        )
        .unwrap();
        let alive = |pid: i64| pid == 42;
        let dead = |_: i64| false;
        assert_eq!(
            poll_holder_with(&paths, now, &alive),
            Some(Holder {
                kind: "menu".into(),
                pid: 42,
                since: Some(iso(now - 60_000))
            })
        );
        assert_eq!(poll_holder_with(&paths, now, &dead), None);
        assert_eq!(
            poll_holder_with(&paths, now + LEASE_STALE_MS, &alive),
            None,
            "not renewed in time"
        );
        assert_eq!(kind_words("menu"), "the menu");
        assert_eq!(kind_words("start"), "baton");
    }

    #[test]
    fn a_command_leaves_a_request() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        request_poll(&paths, "work", "added", 1000);
        let files: Vec<_> = std::fs::read_dir(request_dir(&paths))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(files.len(), 1);
        assert!(files[0].file_name().to_string_lossy().starts_with("1000-"));
        assert_eq!(
            read_json(&files[0].path()),
            Some(json!({ "account": "work", "reason": "added", "at": "1970-01-01T00:00:01.000Z" }))
        );
    }
}
