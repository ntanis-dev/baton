//! Baton's own records do not grow for ever: what the policy says is old goes when a wrapper or the
//! menu starts. Only what Baton wrote under `~/.baton` is ever looked at; Claude Code's transcripts and
//! memory are never read for this, moved or removed.
//!
//! | policy        | default | removes                                              |
//! | ------------- | ------- | ---------------------------------------------------- |
//! | `historyDays` | 180     | the history's months that ended longer ago (0: none) |
//! | `sessionDays` | 30      | the records of conversations not seen for that long  |
//! | `logDays`     | 14      | the wrappers' logs not written for that long (0: none) |
//! | `backupsKept` | 50      | all but the newest backups (0: none)                 |
//!
//! The records of wrappers that no longer run go too, whatever the policy.

use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::clock::{DAY, Ms, parse_ms};
use crate::config::Policy;
use crate::fsutil::{mtime_ms, remove_file, remove_tree};
use crate::paths::Paths;
use crate::sessions::{list_wrappers, prune_sessions, prune_wrappers};

/// What a clean-up removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Cleaned {
    /// Months of history.
    pub history: usize,
    /// Conversations' records.
    pub sessions: usize,
    /// Wrappers' logs.
    pub logs: usize,
    /// Backups.
    pub backups: usize,
    /// Records of wrappers that no longer run.
    pub wrappers: usize,
}

impl Cleaned {
    /// How many things went, in all.
    pub fn total(&self) -> usize {
        self.history + self.sessions + self.logs + self.backups + self.wrappers
    }
}

/// The names of what is in a folder, in order; none when it cannot be read.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// The moment the month after the one a history file holds begins: `events-2026-11.jsonl` ends where
/// December starts. None for a name that is no history file's.
fn month_end(name: &str) -> Option<Ms> {
    let (year, month) = name.strip_suffix(".jsonl")?.rsplit_once('-')?;
    let year: i32 = year.rsplit_once('-')?.1.parse().ok()?;
    let month: u32 = month.parse().ok()?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let (year, month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    parse_ms(&format!("{year:04}-{month:02}-01"))
}

/// Removes the months of history that ended more than `days` ago.
fn clean_history(paths: &Paths, days: f64, now: Ms) -> usize {
    if days <= 0.0 {
        return 0;
    }
    let before = now - (days * DAY as f64) as Ms;
    names(&paths.history)
        .iter()
        .filter(|name| month_end(name).is_some_and(|end| end <= before))
        .filter(|name| remove_file(&paths.history.join(name)).is_ok())
        .count()
}

/// Removes the logs of wrappers that no longer run and that were not written for more than `days`.
fn clean_logs(paths: &Paths, days: f64, now: Ms) -> usize {
    if days <= 0.0 {
        return 0;
    }
    let before = now - (days * DAY as f64) as Ms;
    let running: Vec<String> = list_wrappers(paths)
        .iter()
        .filter_map(|record| record.get("wrapper")?.as_str().map(|id| format!("lead-{id}.log")))
        .collect();
    names(&paths.logs)
        .iter()
        .filter(|name| name.ends_with(".log") && !running.contains(name))
        .filter(|name| mtime_ms(&paths.logs.join(name)).is_some_and(|at| at < before))
        .filter(|name| remove_file(&paths.logs.join(name)).is_ok())
        .count()
}

/// Removes all but the newest `kept` backups. Their folders are named by the moment they were made,
/// so their order by name is their order in time.
fn clean_backups(paths: &Paths, kept: f64) -> usize {
    if kept <= 0.0 {
        return 0;
    }
    let all: Vec<String> = names(&paths.backups)
        .into_iter()
        .filter(|name| paths.backups.join(name).is_dir())
        .collect();
    let extra = all.len().saturating_sub(kept as usize);
    all[..extra]
        .iter()
        .filter(|name| remove_tree(&paths.backups.join(name)).is_ok())
        .count()
}

/// Removes what the policy says is old. Best effort: what cannot be removed stays for the next time.
pub fn clean_up(paths: &Paths, policy: &Policy, now: Ms) -> Cleaned {
    Cleaned {
        history: clean_history(paths, policy.history_days, now),
        sessions: prune_sessions(paths, now, policy.session_days.max(1.0) as i64),
        logs: clean_logs(paths, policy.log_days, now),
        backups: clean_backups(paths, policy.backups_kept),
        wrappers: prune_wrappers(paths, now),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::iso;
    use crate::fsutil::{append_json_line, write_json_atomic};
    use crate::paths::baton_paths;
    use crate::sessions::{hold_wrapper, wrapper_file};
    use serde_json::json;

    /// 2026-11-15T12:00:00Z.
    const NOW: Ms = 1_794_744_000_000;

    #[test]
    fn a_history_file_ends_where_the_next_month_begins() {
        assert_eq!(
            month_end("events-2026-11.jsonl").map(iso).unwrap(),
            "2026-12-01T00:00:00.000Z"
        );
        assert_eq!(
            month_end("readings-2026-12.jsonl").map(iso).unwrap(),
            "2027-01-01T00:00:00.000Z"
        );
        for other in [
            "events-2026-13.jsonl",
            "events.jsonl",
            "notes.txt",
            "events-2026-11.json",
        ] {
            assert_eq!(month_end(other), None, "{other}");
        }
    }

    #[test]
    fn what_the_policy_says_is_old_goes_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let policy = Policy::default();
        assert_eq!(
            clean_up(&paths, &policy, NOW),
            Cleaned::default(),
            "nothing there yet"
        );
        // History: 180 days back from mid November is mid May; April ended before that, May did not.
        for month in ["2026-04", "2026-05", "2026-11"] {
            for kind in ["events", "readings"] {
                let file = paths.history.join(format!("{kind}-{month}.jsonl"));
                append_json_line(&file, &json!({ "at": format!("{month}-10T00:00:00.000Z") })).unwrap();
            }
        }
        fs::write(paths.history.join("notes.txt"), "mine").unwrap();
        // Backups: 52 of them.
        for n in 0..52 {
            let folder = paths.backups.join(format!("2026-10-01T10-00-{n:02}-000Z"));
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("settings.json"), "{}").unwrap();
        }
        // Wrappers: one that runs, one that is gone; and a log of each, both just written.
        write_json_atomic(
            &wrapper_file(&paths, "alive"),
            &json!({ "pid": 1, "wrapper": "alive" }),
        )
        .unwrap();
        let _running = hold_wrapper(&paths, "alive").unwrap();
        write_json_atomic(
            &wrapper_file(&paths, "gone"),
            &json!({ "pid": 0, "wrapper": "gone" }),
        )
        .unwrap();
        fs::create_dir_all(&paths.logs).unwrap();
        for wrapper in ["alive", "gone"] {
            fs::write(paths.logs.join(format!("lead-{wrapper}.log")), "{}\n").unwrap();
        }
        assert_eq!(clean_history(&paths, policy.history_days, NOW), 2);
        assert_eq!(clean_backups(&paths, policy.backups_kept), 2);
        // The logs and the wrappers' records are as old as the disk says: nothing goes while they
        // are fresh, and seen from a month later what belongs to no running wrapper does.
        let today = crate::clock::now_ms();
        assert_eq!(clean_logs(&paths, policy.log_days, today), 0);
        assert_eq!(prune_wrappers(&paths, today), 0);
        let later = today + 31 * DAY;
        assert_eq!(clean_logs(&paths, policy.log_days, later), 1);
        assert_eq!(prune_wrappers(&paths, later), 1);
        let left = names(&paths.history);
        assert_eq!(left.len(), 5, "{left:?}");
        assert!(left.contains(&"notes.txt".to_owned()) && !left.iter().any(|name| name.contains("2026-04")));
        let backups = names(&paths.backups);
        assert_eq!(backups.len(), 50);
        assert_eq!(backups[0], "2026-10-01T10-00-02-000Z", "the oldest two went");
        assert_eq!(
            names(&paths.logs),
            ["lead-alive.log"],
            "a running wrapper keeps its log"
        );
        assert!(wrapper_file(&paths, "alive").exists() && !wrapper_file(&paths, "gone").exists());
        // Zero keeps for ever.
        let forever = Policy {
            history_days: 0.0,
            log_days: 0.0,
            backups_kept: 0.0,
            ..Policy::default()
        };
        assert_eq!(clean_up(&paths, &forever, later + 3650 * DAY), Cleaned::default());
        // And everything at once, a long time on: the rest of the history and the last log stay
        // only as far as the policy keeps them.
        let all = clean_up(&paths, &policy, later + 3650 * DAY);
        assert_eq!((all.history, all.backups, all.logs, all.wrappers), (4, 0, 0, 0));
        assert_eq!(all.total(), 4);
    }
}
