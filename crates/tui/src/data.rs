//! Everything the menu shows, read from ~/.baton in one go (the same core functions as the CLI): the
//! policy's snapshot with every account's state, where new work goes, the live sessions and recent
//! ones with their cost estimates and hops, the last 7 days of statistics, the copy conflicts, and the
//! usage poller (its state per account, which baton instance polls, the next poll). Sign-in status
//! comes separately (it asks `claude auth status`).

use std::collections::BTreeMap;

use baton_core::clock::{Ms, ms_of};
use baton_core::config::{Config, Policy, name_of};
use baton_core::json::Get;
use baton_core::paths::Paths;
use baton_core::policy::{AccountState, burn_rate, pick_start, turn_cost};
use baton_core::sessions::{read_session_file, read_turn_samples};
use baton_core::state::{PollSnapshot, Want, load_snapshot};
use baton_core::stats::{HopView, Stats, compute_stats};
use baton_core::usage::Reading;
use baton_core::window::PerWindow;
use baton_profile::copies::read_conflicts;
use baton_profile::sync::read_questions;
use serde_json::Value;

/// How many recent sessions the menu lists.
pub const RECENT_LIMIT: usize = 12;

/// A session, live or recent.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    /// The session's id.
    pub id: String,
    /// Whether a wrapper runs it now.
    pub live: bool,
    /// What its wrapper does: `running`, `waiting`, `countdown`.
    pub wrapper_state: Option<String>,
    /// The account its wrapper waits for or moves it to, by id.
    pub target: Option<String>,
    /// Until when its wrapper waits.
    pub until: Option<Ms>,
    /// The account it runs on, by id.
    pub account: Option<String>,
    /// Its working folder.
    pub cwd: Option<String>,
    /// What it is called: its folder's name, else the start of its id.
    pub name: String,
    /// When it started.
    pub started_at: Option<Ms>,
    /// When it was last seen.
    pub last_at: Option<Ms>,
    /// Whether it ended.
    pub ended: bool,
    /// Its turn: `running` or `idle`.
    pub turn_state: Option<String>,
    /// The hook that last wrote its turn.
    pub turn_event: Option<String>,
    /// The error its last turn ended on.
    pub error: Option<String>,
    /// Its model.
    pub model: Option<String>,
    /// What runs in its background.
    pub background_tasks: Vec<Value>,
    /// Its estimated cost per turn.
    pub cost: PerWindow<Option<f64>>,
    /// Its burn rate per hour.
    pub burn: PerWindow<Option<f64>>,
    /// Its turns with a reading.
    pub turns: usize,
    /// Its moves in the last 7 days.
    pub hops: Vec<HopView>,
    /// The name of the account it runs on.
    pub account_name: Option<String>,
    /// The name of the account it waits for or moves to.
    pub target_name: Option<String>,
}

/// Where new work goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Next {
    /// The account's id; None when nothing is open.
    pub account: Option<String>,
    /// Why it was picked.
    pub reason: String,
    /// When nothing is open: the account that is back first.
    pub earliest: Option<String>,
    /// When the chosen (or the earliest) account is available.
    pub available_at: Option<Ms>,
    /// Whether that time is known.
    pub available_known: bool,
}

/// Everything the menu shows.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuData {
    /// When it was read.
    pub now: Ms,
    /// The accounts and settings.
    pub config: Config,
    /// The policy in force.
    pub policy: Policy,
    /// Every account's state, in the operator's order.
    pub states: Vec<AccountState>,
    /// Where new work goes.
    pub next: Next,
    /// The latest reading of each account that has one.
    pub readings: BTreeMap<String, Reading>,
    /// The usage poll.
    pub poll: Option<PollSnapshot>,
    /// The sessions: the live ones, then the recent ones, newest first.
    pub sessions: Vec<SessionInfo>,
    /// The last 7 days.
    pub stats: Stats,
    /// How many shared files changed in two places at once.
    pub conflicts: usize,
    /// How many settings the accounts differ on and only the operator can settle (full sync).
    pub differ: usize,
}

/// The last part of a path, as the system splits paths.
fn base_name(path: &str) -> &str {
    let is_sep = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let trimmed = path.trim_end_matches(['/', '\\']);
    let trimmed = trimmed.trim_end_matches(is_sep);
    trimmed.rsplit(is_sep).next().unwrap_or(trimmed)
}

/// The first of these records that has `key`, as text.
fn first_text(key: &str, records: &[&Value]) -> Option<String> {
    records
        .iter()
        .find_map(|r| r.get(key).filter(|v| !v.is_null()))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// One session, from its files and its wrapper's record.
fn session_info(
    paths: &Paths,
    id: &str,
    now: Ms,
    policy: &Policy,
    config: &Config,
    wrapper: Option<&Value>,
    hops: Vec<HopView>,
) -> SessionInfo {
    let read = |name: &str| read_session_file(paths, id, name).unwrap_or(Value::Null);
    let (start, live, turn, end) = (
        read("start.json"),
        read("live.json"),
        read("turn.json"),
        read("end.json"),
    );
    let none = Value::Null;
    let wrapped = wrapper.unwrap_or(&none);
    let samples = read_turn_samples(paths, id);
    let cwd = first_text("cwd", &[&live, &start, wrapped]);
    let seen = |record: &Value| ms_of(record.at("at")).unwrap_or(0);
    let last = seen(&live).max(seen(&turn)).max(seen(&start)).max(seen(&end));
    let last_at = (last != 0).then_some(last);
    let name = cwd
        .as_deref()
        .map(base_name)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    let account = first_text("account", &[wrapped, &live, &turn, &start]);
    let target = wrapped.text("target").map(str::to_owned);
    SessionInfo {
        id: id.to_owned(),
        live: wrapper.is_some(),
        wrapper_state: wrapped.text("state").map(str::to_owned),
        until: ms_of(wrapped.at("until")),
        name: name.unwrap_or_else(|| id.chars().take(8).collect()),
        started_at: ms_of(wrapped.at("startedAt")).or_else(|| ms_of(start.at("at"))),
        last_at,
        ended: end.is_object() && seen(&end) >= last_at.unwrap_or(0),
        turn_state: turn.text("state").map(str::to_owned),
        turn_event: turn.text("event").map(str::to_owned),
        error: turn.text("error").map(str::to_owned),
        model: first_text("model", &[&live, &start]),
        background_tasks: turn.at("backgroundTasks").as_array().cloned().unwrap_or_default(),
        cost: turn_cost(&samples, policy),
        burn: burn_rate(&samples, policy, now),
        turns: samples.len(),
        hops,
        // Sessions name their account by id; what shows is its current name.
        account_name: account.as_deref().map(|account| name_of(config, account)),
        target_name: target.as_deref().map(|target| name_of(config, target)),
        account,
        target,
        cwd,
    }
}

/// Reads everything the menu shows, at `now`.
pub fn load_menu_data(paths: &Paths, now: Ms) -> MenuData {
    let snapshot = load_snapshot(
        paths,
        now,
        Want {
            sessions: true,
            poll: true,
        },
    );
    let stats = compute_stats(paths, &snapshot.config, &snapshot.policy, now, 7);
    let mut ids: Vec<String> = Vec::new();
    let mut wrappers: Vec<(String, &Value)> = Vec::new();
    for w in snapshot.wrappers.iter().flatten() {
        if let Some(session) = w.word("session") {
            // A later wrapper's record for the same session replaces an earlier one.
            match wrappers.iter_mut().find(|(id, _)| id == session) {
                Some(entry) => entry.1 = w,
                None => wrappers.push((session.to_owned(), w)),
            }
        }
    }
    let seen = snapshot.sessions.iter().flatten().map(|x| x.id.clone());
    let counted = stats.sessions.iter().map(|x| x.id.clone());
    for id in wrappers
        .iter()
        .map(|(id, _)| id.clone())
        .chain(seen)
        .chain(counted)
    {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut sessions: Vec<SessionInfo> = ids
        .iter()
        .map(|id| {
            let wrapper = wrappers
                .iter()
                .find(|(session, _)| session == id)
                .map(|(_, w)| *w);
            let hops = stats
                .sessions
                .iter()
                .find(|x| &x.id == id)
                .map(|x| x.hops.clone())
                .unwrap_or_default();
            session_info(paths, id, now, &snapshot.policy, &snapshot.config, wrapper, hops)
        })
        .collect();
    sessions.sort_by(|a, b| {
        b.live
            .cmp(&a.live)
            .then_with(|| b.last_at.unwrap_or(0).cmp(&a.last_at.unwrap_or(0)))
    });
    let mut recent = 0;
    sessions.retain(|x| {
        if x.live {
            return true;
        }
        recent += 1;
        recent <= RECENT_LIMIT
    });
    let pick = pick_start(&snapshot.states, &snapshot.policy, &[]);
    let next = Next {
        account: pick.account.clone(),
        reason: pick.reason.to_owned(),
        earliest: pick.earliest.clone(),
        available_at: pick.available_at,
        available_known: pick.available_known,
    };
    MenuData {
        now,
        next,
        conflicts: read_conflicts(paths).len(),
        differ: if snapshot.policy.full_sync {
            read_questions(paths).len()
        } else {
            0
        },
        config: snapshot.config,
        policy: snapshot.policy,
        states: snapshot.states,
        readings: snapshot.readings,
        poll: snapshot.poll,
        sessions,
        stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_of_a_folder() {
        assert_eq!(base_name("/home/op/project"), "project");
        assert_eq!(base_name("/home/op/project///"), "project");
        if cfg!(windows) {
            assert_eq!(base_name("E:\\GitHub\\ntanis-dev\\pelican"), "pelican");
            assert_eq!(base_name("E:\\GitHub\\ntanis-dev\\pelican\\"), "pelican");
        }
        assert_eq!(base_name(""), "");
    }
}
