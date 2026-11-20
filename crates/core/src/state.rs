//! A snapshot of everything Baton knows, read from disk in one go: what the CLI's status, the session
//! wrapper's decisions and the menu work from.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::clock::Ms;
use crate::config::{Config, Policy, effective_policy, load_config};
use crate::paths::Paths;
use crate::policy::{AccountState, Pick, account_states, pick_start};
use crate::poll::{Holder, PollState, next_poll_at, poll_holder, read_poll_state};
use crate::sessions::{RecentSession, list_wrappers, recent_sessions_now};
use crate::usage::{Reading, Rejections, load_readings, load_rejections};

/// The usage poll, as a snapshot sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct PollSnapshot {
    /// The poller's state.
    pub state: PollState,
    /// Which baton instance polls; None when none runs.
    pub holder: Option<Holder>,
    /// When the next scheduled poll runs.
    pub next_at: Option<Ms>,
}

/// Everything Baton knows at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    /// The moment.
    pub now: Ms,
    /// Where it was read from.
    pub paths: Paths,
    /// The accounts and settings.
    pub config: Config,
    /// The policy in force.
    pub policy: Policy,
    /// The latest reading of each account that has one.
    pub readings: BTreeMap<String, Reading>,
    /// The last refusals of each account.
    pub rejections: BTreeMap<String, Rejections>,
    /// Every account's state, in the operator's order.
    pub states: Vec<AccountState>,
    /// The running wrappers, when asked for.
    pub wrappers: Option<Vec<Value>>,
    /// The sessions seen recently, when asked for.
    pub sessions: Option<Vec<RecentSession>>,
    /// The usage poll, when asked for.
    pub poll: Option<PollSnapshot>,
}

/// What a snapshot reads beyond the accounts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Want {
    /// The running wrappers and the recent sessions.
    pub sessions: bool,
    /// The usage poller's state, which baton instance polls and the next poll.
    pub poll: bool,
}

/// Reads a snapshot at `now`.
pub fn load_snapshot(paths: &Paths, now: Ms, want: Want) -> Snapshot {
    let config = load_config(paths);
    let policy = effective_policy(&config);
    let ids: Vec<String> = config.accounts.iter().map(|a| a.id.clone()).collect();
    let readings = load_readings(paths, &ids);
    let rejections = load_rejections(paths, &ids);
    let states = account_states(&config.accounts, &readings, &rejections, &policy, now);
    let (wrappers, sessions) = if want.sessions {
        (Some(list_wrappers(paths)), Some(recent_sessions_now(paths, now)))
    } else {
        (None, None)
    };
    let poll = want.poll.then(|| {
        let state = read_poll_state(paths);
        let holder = poll_holder(paths, now);
        let next_at = next_poll_at(&state, &policy, holder.is_some(), now);
        PollSnapshot {
            state,
            holder,
            next_at,
        }
    });
    Snapshot {
        now,
        paths: paths.clone(),
        config,
        policy,
        readings,
        rejections,
        states,
        wrappers,
        sessions,
        poll,
    }
}

/// Where new work goes now.
pub fn next_work(snapshot: &Snapshot) -> Pick<'_> {
    pick_start(&snapshot.states, &snapshot.policy, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{HOUR, iso};
    use crate::fsutil::write_json_atomic;
    use crate::paths::baton_paths;
    use crate::usage::{RecordOptions, Sample, record_reading};
    use crate::window::PerWindow;
    use serde_json::json;

    #[test]
    fn a_snapshot_reads_everything_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let now = 1_780_000_000_000;
        let empty = load_snapshot(&paths, now, Want::default());
        assert!(empty.states.is_empty());
        assert_eq!(next_work(&empty).account, None);
        assert_eq!(
            (
                empty.wrappers.is_none(),
                empty.sessions.is_none(),
                empty.poll.is_none()
            ),
            (true, true, true)
        );

        write_json_atomic(
            &paths.config,
            &json!({ "accounts": [{ "id": "work", "name": "Work" }, { "id": "spare", "name": "Spare" }] }),
        )
        .unwrap();
        let full = PerWindow {
            five_hour: Some(Sample {
                used: 95.0,
                resets_at: Some(iso(now + HOUR)),
            }),
            seven_day: Some(Sample {
                used: 10.0,
                resets_at: None,
            }),
        };
        record_reading(&paths, "work", &full, RecordOptions::at(now - 1000));
        let snapshot = load_snapshot(
            &paths,
            now,
            Want {
                sessions: true,
                poll: true,
            },
        );
        assert_eq!(snapshot.states.len(), 2);
        assert!(snapshot.states[0].blocked);
        assert_eq!(snapshot.states[0].available_at, Some(now + HOUR));
        assert!(!snapshot.states[1].blocked);
        assert_eq!(next_work(&snapshot).account.as_deref(), Some("spare"));
        assert_eq!(snapshot.wrappers, Some(Vec::new()));
        assert_eq!(snapshot.sessions, Some(Vec::new()));
        let poll = snapshot.poll.unwrap();
        assert_eq!((poll.holder, poll.next_at), (None, None));
    }
}
