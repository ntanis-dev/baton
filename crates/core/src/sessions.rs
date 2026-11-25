//! Per-session state, written by the hooks, the status line, `baton hop` and the session wrapper. Every
//! file has one kind of writer, so a plain atomic replace is enough:
//!
//! ```text
//! sessions/<id>/start.json   SessionStart: { at, source, account, cwd, transcriptPath, model, wrapper }
//! sessions/<id>/turn.json    UserPromptSubmit / Stop / StopFailure:
//!                            { at, state: running|idle, event, account, permissionMode,
//!                              error, window, until, message, backgroundTasks }
//! sessions/<id>/live.json    status line: { at, account, model, effort, cwd, project, reading }
//! sessions/<id>/compact.json SessionStart source=compact: { at }
//! sessions/<id>/end.json     SessionEnd: { at, reason }
//! sessions/<id>/hop.json     baton hop / the /hop command: { at, to, by }
//! sessions/<id>/lock         the lock itself: held by the wrapper driving the session ([`crate::oslock`])
//! sessions/<id>/lock.json    who holds it, for `baton hop` to find: { pid, wrapper, at, account }
//! sessions/<id>/turns.jsonl  one line per turn end with the session's reading (cost estimates)
//! wrappers/<wrapper>.json    a running wrapper: { pid, wrapper, account, session, startedAt, cwd, state,
//!                              left, arrivedAt, endedWell } (the last three: its run of hops)
//! wrappers/<wrapper>.lock    held by the wrapper for as long as it runs ([`crate::oslock`])
//! wrappers/<wrapper>.session.json   SessionStart in a process the wrapper started: { session, at, source }
//! ```

use std::fs;
use std::io;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use crate::clock::{DAY, HOUR, Ms, iso, ms_of};
use crate::fsutil::{
    append_json_line, mtime_ms, read_json, read_json_lines, remove_file, remove_tree, write_json_atomic,
};
use crate::json::Get;
use crate::oslock::{OsLock, is_held};
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

/// A session's lock, held by the wrapper that drives it until this is dropped. Dropping it takes the
/// holder's record away first, then lets the lock go.
#[derive(Debug)]
pub struct SessionLock {
    /// `lock.json`, which names this holder.
    record: PathBuf,
    /// The wrapper that holds it.
    wrapper: Option<String>,
    /// The lock itself. Declared last: it goes after the record.
    _lock: OsLock,
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        if read_json(&self.record).is_some_and(|holder| holder.text("wrapper") == self.wrapper.as_deref()) {
            let _ = remove_file(&self.record);
        }
    }
}

/// What came of asking for a session's lock.
#[derive(Debug)]
pub enum Lock {
    /// The lock is the caller's for as long as it keeps this.
    Taken(SessionLock),
    /// Another wrapper holds it: its record, or null when it has not written one yet.
    Held(Value),
    /// It could not be taken.
    Failed(String),
}

impl Lock {
    /// The lock, when it was taken.
    pub fn taken(self) -> Option<SessionLock> {
        match self {
            Lock::Taken(lock) => Some(lock),
            _ => None,
        }
    }
}

/// The per-session lock: one wrapper drives a conversation at a time. The lock is the operating
/// system's, so a wrapper that died holds nothing; `owner` (`{ pid, wrapper, at, account }`) is written
/// beside it for `baton hop` to find the wrapper.
pub fn acquire_lock(paths: &Paths, id: &str, owner: &Value) -> Lock {
    let (Some(target), Some(record)) = (file(paths, id, "lock"), file(paths, id, "lock.json")) else {
        return Lock::Failed(format!("Not a session id: {id}"));
    };
    // Someone only asking whether it is held ([`lock_holder`]) has it for an instant: ask again.
    for attempt in 0..3 {
        match OsLock::try_take(&target) {
            Ok(Some(lock)) => {
                return match write_json_atomic(&record, owner) {
                    Ok(()) => Lock::Taken(SessionLock {
                        record,
                        wrapper: owner.text("wrapper").map(str::to_owned),
                        _lock: lock,
                    }),
                    Err(error) => Lock::Failed(error.to_string()),
                };
            }
            Ok(None) if attempt < 2 => thread::sleep(Duration::from_millis(15)),
            Ok(None) => {}
            Err(error) => return Lock::Failed(error.to_string()),
        }
    }
    Lock::Held(read_json(&record).unwrap_or(Value::Null))
}

/// Whether a wrapper holds the session's lock now.
pub fn is_locked(paths: &Paths, id: &str) -> bool {
    file(paths, id, "lock").is_some_and(|target| is_held(&target))
}

/// The holder of a session's lock: its record, while it holds the lock.
pub fn lock_holder(paths: &Paths, id: &str) -> Option<Value> {
    is_locked(paths, id)
        .then(|| read_session_file(paths, id, "lock.json"))
        .flatten()
}

/// A running wrapper's record.
pub fn wrapper_file(paths: &Paths, wrapper: &str) -> PathBuf {
    paths.wrappers.join(format!("{wrapper}.json"))
}

/// The session a process the wrapper started reported.
pub fn wrapper_session_file(paths: &Paths, wrapper: &str) -> PathBuf {
    paths.wrappers.join(format!("{wrapper}.session.json"))
}

/// The lock a wrapper holds for as long as it runs.
fn wrapper_lock_file(paths: &Paths, wrapper: &str) -> PathBuf {
    paths.wrappers.join(format!("{wrapper}.lock"))
}

/// Takes the lock that says a wrapper runs; it is kept until the wrapper ends. None when it could not
/// be taken: the wrapper then does not count as running, and cannot be asked to move its conversation.
pub fn hold_wrapper(paths: &Paths, wrapper: &str) -> Option<OsLock> {
    OsLock::try_take(&wrapper_lock_file(paths, wrapper))
        .ok()
        .flatten()
}

/// Whether the wrapper of a record runs: it holds its lock. A process id says nothing here: the
/// system gives it anew once its process is gone, so a dead wrapper's would name another program.
fn wrapper_runs(paths: &Paths, record: &Value) -> bool {
    record
        .text("wrapper")
        .filter(|wrapper| valid_id(wrapper))
        .is_some_and(|wrapper| is_held(&wrapper_lock_file(paths, wrapper)))
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
        .filter(|record| wrapper_runs(paths, record))
        .collect()
}

/// Forgets a wrapper (which has let go of its lock: nobody else ever takes that one).
pub fn remove_wrapper(paths: &Paths, wrapper: &str) {
    let _ = remove_file(&wrapper_file(paths, wrapper));
    let _ = remove_file(&wrapper_session_file(paths, wrapper));
    let _ = remove_file(&wrapper_lock_file(paths, wrapper));
}

/// How long a wrapper's record is left alone after it was written, whatever it says: a wrapper that
/// is starting has written it before anyone can tell that it runs.
const WRAPPER_GRACE: Ms = 60_000;

/// Removes the records of wrappers that no longer run (one that was killed never removed its own).
/// Returns how many wrappers were forgotten.
pub fn prune_wrappers(paths: &Paths, now: Ms) -> usize {
    let Ok(entries) = fs::read_dir(&paths.wrappers) else {
        return 0;
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| {
            [".session.json", ".json", ".lock"]
                .iter()
                .find_map(|end| name.strip_suffix(end))
                .map(str::to_owned)
        })
        .filter(|id| valid_id(id))
        .collect();
    ids.sort();
    ids.dedup();
    let mut removed = 0;
    for id in ids {
        let files = [
            wrapper_file(paths, &id),
            wrapper_session_file(paths, &id),
            wrapper_lock_file(paths, &id),
        ];
        let fresh = files
            .iter()
            .filter_map(|file| mtime_ms(file))
            .any(|at| now - at < WRAPPER_GRACE);
        let runs = match read_json(&files[0]) {
            Some(record) => wrapper_runs(paths, &record),
            None => is_held(&files[2]),
        };
        if !fresh && !runs {
            remove_wrapper(paths, &id);
            removed += 1;
        }
    }
    removed
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
        if now - mtime < days * DAY || is_locked(paths, &id) {
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
        assert!(!is_locked(&paths, ID));
        let owner = json!({ "pid": me(), "wrapper": "w1", "account": "work" });
        let held = acquire_lock(&paths, ID, &owner).taken().unwrap();
        let other = json!({ "pid": me(), "wrapper": "w2", "account": "work" });
        match acquire_lock(&paths, ID, &other) {
            Lock::Held(holder) => assert_eq!(holder, owner),
            other => panic!("{other:?}"),
        }
        assert_eq!(lock_holder(&paths, ID), Some(owner.clone()));
        drop(held);
        assert_eq!(lock_holder(&paths, ID), None);
        assert!(!session_dir(&paths, ID).unwrap().join("lock.json").exists());
        // What a wrapper that died left behind holds nothing, whatever its record says.
        write_session_file(
            &paths,
            ID,
            "lock.json",
            &json!({ "pid": me(), "wrapper": "dead" }),
        )
        .unwrap();
        assert_eq!(lock_holder(&paths, ID), None);
        // A record with no lock beside it holds nothing either, even when its process id is a live
        // process's: the system gives a number anew once its process is gone.
        let before = "0a1b2c3d-0000-4000-8000-000000000001";
        let earlier = json!({ "pid": me(), "wrapper": "w0", "at": iso(crate::clock::now_ms()) });
        write_session_file(&paths, before, "lock.json", &earlier).unwrap();
        assert_eq!(lock_holder(&paths, before), None);
        assert!(!is_locked(&paths, before));
        assert!(acquire_lock(&paths, before, &other).taken().is_some());
        let taken = acquire_lock(&paths, ID, &other).taken().unwrap();
        assert_eq!(lock_holder(&paths, ID).unwrap()["wrapper"], "w2");
        // A holder lets go of its own record only.
        write_session_file(&paths, ID, "lock.json", &owner).unwrap();
        drop(taken);
        assert!(session_dir(&paths, ID).unwrap().join("lock.json").exists());
        assert!(!is_locked(&paths, ID));
        assert!(matches!(acquire_lock(&paths, "no/id", &other), Lock::Failed(_)));
    }

    #[test]
    fn wrappers_that_run() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        assert!(list_wrappers(&paths).is_empty());
        // A record with no lock held: its wrapper is gone, whatever process has its number now.
        write_json_atomic(
            &wrapper_file(&paths, "w1"),
            &json!({ "pid": me(), "wrapper": "w1", "startedAt": iso(crate::clock::now_ms()) }),
        )
        .unwrap();
        write_json_atomic(
            &wrapper_file(&paths, "gone"),
            &json!({ "pid": 0, "wrapper": "gone" }),
        )
        .unwrap();
        write_json_atomic(&wrapper_session_file(&paths, "w1"), &json!({ "session": ID })).unwrap();
        assert!(list_wrappers(&paths).is_empty());
        remove_wrapper(&paths, "w1");
        assert!(!wrapper_session_file(&paths, "w1").exists());
        // A wrapper holds its lock while it runs: its record alone says nothing once the lock is free.
        let record = json!({ "pid": me(), "wrapper": "w2" });
        write_json_atomic(&wrapper_file(&paths, "w2"), &record).unwrap();
        let running = hold_wrapper(&paths, "w2").unwrap();
        assert_eq!(list_wrappers(&paths), vec![record]);
        drop(running);
        assert!(list_wrappers(&paths).is_empty());
        remove_wrapper(&paths, "w2");
        assert!(
            fs::read_dir(&paths.wrappers).unwrap().next().is_some(),
            "the earlier `gone`"
        );
        assert!(!wrapper_lock_file(&paths, "w2").exists());
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
        let _held = acquire_lock(&paths, ID, &json!({ "pid": me(), "wrapper": "w1" }))
            .taken()
            .unwrap();
        assert_eq!(prune_sessions(&paths, now + 31 * DAY, 30), 1);
        assert!(session_dir(&paths, ID).unwrap().exists());
        assert!(!session_dir(&paths, ended).unwrap().exists());
    }
}
