//! Per-session state, written by the hooks, the status line, `baton hop` and the session wrapper. Every
//! file has one kind of writer, so a plain atomic replace is enough:
//!
//! ```text
//! sessions/<id>/start.json   SessionStart: { at, source, account, cwd, transcriptPath, model, wrapper }
//! sessions/<id>/turn.json    UserPromptSubmit / Stop / StopFailure:
//!                            { at, state: running|idle, event, account, permissionMode,
//!                              error, window, until, message, backgroundTasks }
//! sessions/<id>/live.json    status line: { at, account, model, effort, cwd, reading }
//! sessions/<id>/compact.json SessionStart source=compact: { at }
//! sessions/<id>/end.json     SessionEnd: { at, reason }
//! sessions/<id>/hop.json     baton hop / the /hop command: { at, to, by }
//! sessions/<id>/lock.json    the wrapper holding the session: { pid, wrapper, at, account }
//! sessions/<id>/turns.jsonl  one line per turn end with the session's reading (cost estimates)
//! wrappers/<wrapper>.json    a running wrapper: { pid, wrapper, account, session, startedAt, cwd, state,
//!                              left, arrivedAt, endedWell } (the last three: its run of hops)
//! wrappers/<wrapper>.session.json   SessionStart in a process the wrapper started: { session, at, source }
//! ```

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::clock::{DAY, HOUR, Ms, iso, ms_of};
use crate::fsutil::{
    append_json_line, is_alive, mtime_ms, read_json, read_json_lines, remove_file, remove_tree,
    write_json_atomic,
};
use crate::json::{Get, compact};
use crate::paths::{Paths, is_id};
use crate::usage::TurnSample;

/// Whether `id` can be a session's or a wrapper's id.
pub fn valid_id(id: &str) -> bool {
    is_id(id)
}

/// The folder of a session's state; None when `id` is not a session id.
pub fn session_dir(paths: &Paths, id: &str) -> Option<PathBuf> {
    valid_id(id).then(|| paths.sessions.join(id))
}

/// A file of a session's state.
fn file(paths: &Paths, id: &str, name: &str) -> Option<PathBuf> {
    Some(session_dir(paths, id)?.join(name))
}

/// The error for an id that is not one.
fn not_an_id(id: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("Not a session id: {id}"))
}

/// One of a session's files, read.
pub fn read_session_file(paths: &Paths, id: &str, name: &str) -> Option<Value> {
    read_json(&file(paths, id, name)?)
}

/// Replaces one of a session's files.
pub fn write_session_file(paths: &Paths, id: &str, name: &str, value: &Value) -> io::Result<()> {
    write_json_atomic(&file(paths, id, name).ok_or_else(|| not_an_id(id))?, value)
}

/// Appends a turn end's reading to the session's samples.
pub fn append_turn_sample(paths: &Paths, id: &str, sample: &Value) -> io::Result<()> {
    append_json_line(
        &file(paths, id, "turns.jsonl").ok_or_else(|| not_an_id(id))?,
        sample,
    )
}

/// The session's readings at its turn ends, oldest first.
pub fn read_turn_samples(paths: &Paths, id: &str) -> Vec<TurnSample> {
    match file(paths, id, "turns.jsonl") {
        Some(file) => read_json_lines(&file)
            .iter()
            .map(TurnSample::from_value)
            .collect(),
        None => Vec::new(),
    }
}

/// Asks the wrapper running session `id` to move it: to the account `to`, or to the best one.
pub fn request_hop(paths: &Paths, id: &str, to: Option<&str>, by: &str, at: Ms) -> io::Result<()> {
    write_session_file(
        paths,
        id,
        "hop.json",
        &json!({ "at": iso(at), "to": to, "by": by }),
    )
}

/// What came of asking for a session's lock.
#[derive(Clone, Debug, PartialEq)]
pub enum Lock {
    /// The lock is the caller's.
    Taken,
    /// A live process holds it.
    Held(Value),
    /// It could not be taken.
    Failed(String),
}

/// The pid of a lock's or a wrapper's record, when its process is alive.
fn alive_pid(record: &Value) -> bool {
    record.int("pid").is_some_and(is_alive)
}

/// The per-session lock: one conversation runs in one process at a time. Exclusive create; a lock
/// whose process is gone is taken over. `owner` is `{ pid, wrapper, at, account }`.
pub fn acquire_lock(paths: &Paths, id: &str, owner: &Value) -> Lock {
    let Some(target) = file(paths, id, "lock.json") else {
        return Lock::Failed(format!("Not a session id: {id}"));
    };
    if let Some(dir) = target.parent()
        && let Err(error) = fs::create_dir_all(dir)
    {
        return Lock::Failed(error.to_string());
    }
    for _ in 0..3 {
        match OpenOptions::new().write(true).create_new(true).open(&target) {
            Ok(mut out) => {
                return match out.write_all((compact(owner) + "\n").as_bytes()) {
                    Ok(()) => Lock::Taken,
                    Err(error) => Lock::Failed(error.to_string()),
                };
            }
            Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
                return Lock::Failed(error.to_string());
            }
            Err(_) => {
                let holder = read_json(&target);
                if let Some(holder) = holder {
                    if holder.is_object() && holder.at("wrapper") == owner.at("wrapper") {
                        return Lock::Taken;
                    }
                    if alive_pid(&holder) {
                        return Lock::Held(holder);
                    }
                }
                let _ = remove_file(&target);
            }
        }
    }
    Lock::Failed("could not take the lock".to_owned())
}

/// Lets go of a session's lock when `wrapper` holds it.
pub fn release_lock(paths: &Paths, id: &str, wrapper: &str) {
    let Some(target) = file(paths, id, "lock.json") else {
        return;
    };
    if read_json(&target).is_some_and(|holder| holder.text("wrapper") == Some(wrapper)) {
        let _ = remove_file(&target);
    }
}

/// The live holder of a session's lock.
pub fn lock_holder(paths: &Paths, id: &str) -> Option<Value> {
    read_session_file(paths, id, "lock.json").filter(alive_pid)
}

/// A running wrapper's record.
pub fn wrapper_file(paths: &Paths, wrapper: &str) -> PathBuf {
    paths.wrappers.join(format!("{wrapper}.json"))
}

/// The session a process the wrapper started reported.
pub fn wrapper_session_file(paths: &Paths, wrapper: &str) -> PathBuf {
    paths.wrappers.join(format!("{wrapper}.session.json"))
}

/// The wrappers that run now.
pub fn list_wrappers(paths: &Paths) -> Vec<Value> {
    let Ok(entries) = fs::read_dir(&paths.wrappers) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .iter()
        .filter(|name| name.ends_with(".json") && !name.ends_with(".session.json"))
        .filter_map(|name| read_json(&paths.wrappers.join(name)))
        .filter(alive_pid)
        .collect()
}

/// Forgets a wrapper.
pub fn remove_wrapper(paths: &Paths, wrapper: &str) {
    let _ = remove_file(&wrapper_file(paths, wrapper));
    let _ = remove_file(&wrapper_session_file(paths, wrapper));
}

/// A session seen recently that has not ended.
#[derive(Clone, Debug, PartialEq)]
pub struct RecentSession {
    /// The session's id.
    pub id: String,
    /// The account it was last seen on.
    pub account: Option<String>,
    /// When it was last seen.
    pub at: Ms,
    /// The wrapper that started it.
    pub wrapper: Option<String>,
    /// Its working folder.
    pub cwd: Option<String>,
    /// `running` or `idle`.
    pub state: Option<String>,
}

/// The names of the folders in `sessions/` that are session ids, in order (whatever order the file
/// system lists them in).
fn session_ids(paths: &Paths) -> Vec<String> {
    let Ok(entries) = fs::read_dir(&paths.sessions) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|id| valid_id(id))
        .collect();
    ids.sort();
    ids
}

/// Sessions seen within `within_ms` (by any hook or the status line) that have not ended, the latest
/// first.
pub fn recent_sessions(paths: &Paths, now: Ms, within_ms: Ms) -> Vec<RecentSession> {
    let mut out = Vec::new();
    for id in session_ids(paths) {
        // Every write is a rename into the folder, so its time is the last activity.
        let Some(mtime) = mtime_ms(&paths.sessions.join(&id)) else {
            continue;
        };
        if now - mtime > within_ms {
            continue;
        }
        let read = |name: &str| read_session_file(paths, &id, name).unwrap_or(Value::Null);
        let (live, turn, start, end) = (
            read("live.json"),
            read("turn.json"),
            read("start.json"),
            read("end.json"),
        );
        let seen = |record: &Value| ms_of(record.at("at")).unwrap_or(0);
        let at = seen(&live).max(seen(&turn)).max(seen(&start)).max(mtime);
        if now - at > within_ms {
            continue;
        }
        if end.is_object() && seen(&end) >= at - 1000 {
            continue;
        }
        let first = |key: &str, records: &[&Value]| {
            records
                .iter()
                .find_map(|r| r.get(key).filter(|v| !v.is_null()))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        out.push(RecentSession {
            account: first("account", &[&live, &turn, &start]),
            at,
            wrapper: first("wrapper", &[&start]),
            cwd: first("cwd", &[&live, &start]),
            state: first("state", &[&turn]),
            id,
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.at));
    out
}

/// [`recent_sessions`] over the last six hours.
pub fn recent_sessions_now(paths: &Paths, now: Ms) -> Vec<RecentSession> {
    recent_sessions(paths, now, 6 * HOUR)
}

/// Forgets sessions not seen for `days` (their folders hold only Baton's own small files). The history
/// keeps their events. Returns how many were removed.
pub fn prune_sessions(paths: &Paths, now: Ms, days: i64) -> usize {
    let mut removed = 0;
    for id in session_ids(paths) {
        let dir = paths.sessions.join(&id);
        let Some(mtime) = mtime_ms(&dir) else {
            continue;
        };
        if now - mtime < days * DAY || lock_holder(paths, &id).is_some() {
            continue;
        }
        if remove_tree(&dir).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::now_ms;
    use crate::paths::baton_paths;

    const ID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";

    fn me() -> i64 {
        i64::from(std::process::id())
    }

    #[test]
    fn session_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        assert_eq!(read_session_file(&paths, ID, "turn.json"), None);
        write_session_file(&paths, ID, "turn.json", &json!({ "state": "idle" })).unwrap();
        assert_eq!(
            read_session_file(&paths, ID, "turn.json"),
            Some(json!({ "state": "idle" }))
        );
        assert!(write_session_file(&paths, "../x", "turn.json", &json!({})).is_err());
        assert_eq!(read_session_file(&paths, "../x", "turn.json"), None);
        assert_eq!(session_dir(&paths, "a/b"), None);
        request_hop(&paths, ID, Some("spare"), "cli", 1000).unwrap();
        assert_eq!(
            read_session_file(&paths, ID, "hop.json"),
            Some(json!({ "at": "1970-01-01T00:00:01.000Z", "to": "spare", "by": "cli" }))
        );
        append_turn_sample(
            &paths,
            ID,
            &json!({ "account": "work", "five_hour": { "used": 3 } }),
        )
        .unwrap();
        append_turn_sample(
            &paths,
            ID,
            &json!({ "account": "work", "five_hour": { "used": 5 } }),
        )
        .unwrap();
        let samples = read_turn_samples(&paths, ID);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[1].windows.five_hour.as_ref().unwrap().used, 5.0);
        assert!(read_turn_samples(&paths, "nope/").is_empty());
    }

    #[test]
    fn one_wrapper_holds_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let owner = json!({ "pid": me(), "wrapper": "w1", "account": "work" });
        assert_eq!(acquire_lock(&paths, ID, &owner), Lock::Taken);
        assert_eq!(
            acquire_lock(&paths, ID, &owner),
            Lock::Taken,
            "its own lock again"
        );
        let other = json!({ "pid": me(), "wrapper": "w2", "account": "work" });
        assert_eq!(acquire_lock(&paths, ID, &other), Lock::Held(owner.clone()));
        assert_eq!(lock_holder(&paths, ID), Some(owner));
        release_lock(&paths, ID, "w2");
        assert!(lock_holder(&paths, ID).is_some(), "only its holder lets it go");
        release_lock(&paths, ID, "w1");
        assert_eq!(lock_holder(&paths, ID), None);
        // A lock whose process is gone is taken over.
        write_session_file(&paths, ID, "lock.json", &json!({ "pid": 0, "wrapper": "dead" })).unwrap();
        assert_eq!(lock_holder(&paths, ID), None);
        assert_eq!(acquire_lock(&paths, ID, &other), Lock::Taken);
        assert_eq!(lock_holder(&paths, ID).unwrap()["wrapper"], "w2");
        assert!(matches!(acquire_lock(&paths, "no/id", &other), Lock::Failed(_)));
    }

    #[test]
    fn wrappers_that_run() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        assert!(list_wrappers(&paths).is_empty());
        write_json_atomic(
            &wrapper_file(&paths, "w1"),
            &json!({ "pid": me(), "wrapper": "w1" }),
        )
        .unwrap();
        write_json_atomic(
            &wrapper_file(&paths, "gone"),
            &json!({ "pid": 0, "wrapper": "gone" }),
        )
        .unwrap();
        write_json_atomic(&wrapper_session_file(&paths, "w1"), &json!({ "session": ID })).unwrap();
        let running = list_wrappers(&paths);
        assert_eq!(running.len(), 1);
        assert_eq!(running[0]["wrapper"], "w1");
        remove_wrapper(&paths, "w1");
        assert!(list_wrappers(&paths).is_empty());
        assert!(!wrapper_session_file(&paths, "w1").exists());
    }

    #[test]
    fn recent_sessions_and_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let now = now_ms();
        let ended = "ended-session";
        write_session_file(
            &paths,
            ID,
            "start.json",
            &json!({ "at": iso(now - 5000), "account": "work", "wrapper": "w1", "cwd": "/a" }),
        )
        .unwrap();
        write_session_file(
            &paths,
            ID,
            "turn.json",
            &json!({ "at": iso(now - 1000), "account": "spare", "state": "running" }),
        )
        .unwrap();
        write_session_file(
            &paths,
            ended,
            "start.json",
            &json!({ "at": iso(now - 5000), "account": "work" }),
        )
        .unwrap();
        write_session_file(&paths, ended, "end.json", &json!({ "at": iso(now + 2000) })).unwrap();
        fs::create_dir_all(paths.sessions.join("not a session")).unwrap();
        let recent = recent_sessions_now(&paths, now);
        assert_eq!(recent.len(), 1);
        let seen = &recent[0];
        assert_eq!(seen.id, ID);
        assert_eq!(
            seen.account.as_deref(),
            Some("spare"),
            "the latest writer names the account"
        );
        assert_eq!(seen.wrapper.as_deref(), Some("w1"));
        assert_eq!(seen.cwd.as_deref(), Some("/a"));
        assert_eq!(seen.state.as_deref(), Some("running"));
        assert!(recent_sessions(&paths, now + 7 * HOUR, 6 * HOUR).is_empty());
        // Pruning: nothing young goes; an old folder does, unless its lock is held.
        assert_eq!(prune_sessions(&paths, now, 30), 0);
        assert_eq!(
            acquire_lock(&paths, ID, &json!({ "pid": me(), "wrapper": "w1" })),
            Lock::Taken
        );
        assert_eq!(prune_sessions(&paths, now + 31 * DAY, 30), 1);
        assert!(session_dir(&paths, ID).unwrap().exists());
        assert!(!session_dir(&paths, ended).unwrap().exists());
    }
}
