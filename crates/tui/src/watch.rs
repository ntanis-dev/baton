//! Noticing that ~/.baton changed, so that the menu reads it again at once.
//!
//! Every file Baton keeps is replaced whole (a rename into its folder) or appended to, so a change
//! shows in the time and size of a handful of files and folders. Looking at those is cheap enough to
//! do several times a second, and needs nothing from the system but what every file system gives.

use std::fs;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use baton_core::clock::Ms;
use baton_core::paths::Paths;
use baton_core::poll::{lease_file, poll_dir, poll_file};
use baton_core::usage::history_file;

use crate::state::State;

/// When a file or folder was last changed and how large it is; nothing for one that is not there.
type Stamp = Option<(u128, u64)>;

/// What ~/.baton looked like at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Glance {
    /// The stamp of every file and folder looked at, in a fixed order.
    stamps: Vec<Stamp>,
}

/// The files and folders whose change the menu shows: the config, the usage and refusals, the poll,
/// the wrappers, the sessions on screen, and this month's history.
fn watched(paths: &Paths, state: &State, now: Ms) -> Vec<PathBuf> {
    let mut list = vec![
        paths.config.clone(),
        paths.install.clone(),
        paths.root.join("sync.json"),
        paths.root.join("shared"),
        poll_file(paths),
        lease_file(paths),
        poll_dir(paths),
        paths.usage.clone(),
        paths.rejected.clone(),
        paths.wrappers.clone(),
        paths.sessions.clone(),
        paths.history.clone(),
        history_file(paths, "readings", now),
        history_file(paths, "events", now),
    ];
    if let Some(data) = state.data.as_deref() {
        list.extend(
            data.config
                .accounts
                .iter()
                .map(|account| paths.usage.join(format!("{}.json", account.id))),
        );
        list.extend(
            data.sessions
                .iter()
                .map(|session| paths.sessions.join(&session.id)),
        );
    }
    list
}

/// What ~/.baton looks like now, for the state the menu shows.
pub fn glance(paths: &Paths, state: &State, now: Ms) -> Glance {
    let stamps = watched(paths, state, now)
        .iter()
        .map(|path| {
            let meta = fs::metadata(path).ok()?;
            let changed = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
            Some((changed, meta.len()))
        })
        .collect();
    Glance { stamps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Init, initial_state};
    use baton_core::fsutil::{append_json_line, write_json_atomic};
    use baton_core::paths::baton_paths;
    use serde_json::json;

    #[test]
    fn a_change_under_the_home_shows() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let state = initial_state(Init::default());
        let now = 1_780_000_000_000;
        let empty = glance(&paths, &state, now);
        assert_eq!(glance(&paths, &state, now), empty);
        write_json_atomic(&paths.usage.join("work.json"), &json!({ "used": 1 })).unwrap();
        let usage = glance(&paths, &state, now);
        assert_ne!(usage, empty);
        append_json_line(&history_file(&paths, "events", now), &json!({ "type": "hop" })).unwrap();
        let history = glance(&paths, &state, now);
        assert_ne!(history, usage);
        write_json_atomic(&paths.config, &json!({ "accounts": [] })).unwrap();
        assert_ne!(glance(&paths, &state, now), history);
    }
}
