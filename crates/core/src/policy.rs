//! The policy, as pure functions over plain data: no files, no clock, no output. Everything takes `now`
//! (epoch ms) and the effective policy settings ([`Policy`]); the session wrapper, the CLI and the menu
//! all decide through here.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde_json::Value;

use crate::clock::{HOUR, MINUTE, Ms, parse_ms};
use crate::config::{Account, Mark, Mode, Policy, thresholds};
use crate::json::Get;
use crate::usage::{
    LIMIT_ERRORS, Reading, Rejection, Rejections, TurnSample, WindowReading, all_weekly, covers, limit_scope,
    no_five_hour, own_weekly, same_model, same_period,
};
use crate::window::{Limit, PerWindow, WINDOWS, Window};

/// The background tasks a conversation is never moved under.
const BUSY_TASKS: [&str; 3] = ["subagent", "workflow", "teammate"];

/// One window of one account.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    /// The window.
    pub window: Window,
    /// Percent used; None when not measured or reset.
    pub used: Option<f64>,
    /// When the window resets; None when unknown or already reset.
    pub resets_at: Option<Ms>,
    /// When the reading was taken.
    pub read_at: Option<Ms>,
    /// Where the reading came from (`statusline`, `poll`).
    pub source: Option<String>,
    /// For a weekly limit of one model, its label.
    pub label: Option<String>,
    /// Whether the reading counts as reset.
    pub reset: bool,
    /// Whether it counts as reset because it is older than the window itself.
    pub stale: bool,
    /// Whether the plan has no such limit at all, as far as the last poll tells (the 5-hour window
    /// of a plan that has none): nothing is measured, and nothing of it blocks.
    pub absent: bool,
    /// The threshold in force.
    pub threshold: f64,
    /// threshold - used.
    pub headroom: f64,
    /// Whether the window blocks the account.
    pub blocked: bool,
    /// Blocked by its usage.
    pub usage_blocked: bool,
    /// Blocked by a refusal still in force.
    pub rejected: bool,
    /// When that refusal lifts, when known.
    pub rejected_until: Option<Ms>,
    /// The latest known end of what blocks it; None when a blocking part has no known end.
    pub block_until: Option<Ms>,
    /// Whether every blocking part has a known end.
    pub block_known: bool,
}

/// One window of one account. A reading past its resets_at, or older than the window itself (5 h,
/// 7 d), counts as reset: used 0. headroom = threshold - used. Blocked when headroom <= 0 or a refusal
/// for the window is still in force; block_until is the latest known end of what blocks it (None when
/// a blocking part has no known end).
pub fn window_state(
    window: Window,
    reading: Option<&WindowReading>,
    rejection: Option<&Rejection>,
    threshold: f64,
    now: Ms,
    policy: &Policy,
) -> WindowState {
    let resets_at = reading.and_then(|r| r.resets_at.as_deref()).and_then(parse_ms);
    let read_at = reading.and_then(|r| r.read_at.as_deref()).and_then(parse_ms);
    let mut used = None;
    let mut reset = false;
    let mut stale = false;
    if let Some(reading) = reading.filter(|r| r.used.is_finite()) {
        if resets_at.is_some_and(|at| at <= now) {
            reset = true;
        } else if read_at.is_some_and(|at| now - at > window.length()) {
            reset = true;
            stale = true;
        }
        if !reset {
            used = Some(reading.used);
        }
    }
    let headroom = threshold - used.unwrap_or(0.0);
    let usage_blocked = used.is_some() && headroom <= 0.0;
    let mut rejected = false;
    let mut rejected_until = None;
    if let Some(rejection) = rejection {
        let at = rejection.at.as_deref().and_then(parse_ms).unwrap_or(0);
        let until = rejection.until.as_deref().and_then(parse_ms);
        let max_age = match window {
            Window::SevenDay => Window::SevenDay.length(),
            Window::FiveHour => (policy.unknown_rejection_hours * HOUR as f64) as Ms,
        };
        let expiry = until.unwrap_or(at + max_age);
        // A reading taken after the refusal that shows room again supersedes it.
        let superseded = used.is_some_and(|used| used < threshold) && read_at.is_some_and(|read| read > at);
        if expiry > now && !superseded {
            rejected = true;
            rejected_until = until;
        }
    }
    let blocked = usage_blocked || rejected;
    let mut block_until = None;
    let mut block_known = true;
    if blocked {
        let mut ends = Vec::new();
        if usage_blocked {
            ends.push(resets_at);
        }
        if rejected {
            ends.push(rejected_until);
        }
        block_known = ends.iter().all(Option::is_some);
        block_until = if block_known {
            ends.into_iter().flatten().max()
        } else {
            None
        };
    }
    WindowState {
        window,
        used,
        resets_at: if reset { None } else { resets_at },
        read_at,
        source: reading.and_then(|r| r.source.clone()),
        label: reading.and_then(|r| r.label.clone()),
        reset,
        stale,
        absent: false,
        threshold,
        headroom,
        blocked,
        usage_blocked,
        rejected,
        rejected_until,
        block_until,
        block_known,
    }
}

/// A refusal Baton could not tie to a window, still in force.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnknownRejection {
    /// When it happened.
    pub at: Ms,
    /// When it lifts, when known.
    pub until: Option<Ms>,
}

/// A refusal Baton could not tie to a window blocks the whole account until its known end, or for
/// unknownRejectionHours, or until a later reading shows every window under its threshold.
fn unknown_rejection(
    rejection: Option<&Rejection>,
    windows: &PerWindow<WindowState>,
    now: Ms,
    policy: &Policy,
) -> Option<UnknownRejection> {
    let rejection = rejection?;
    let at = rejection.at.as_deref().and_then(parse_ms).unwrap_or(0);
    let until = rejection.until.as_deref().and_then(parse_ms);
    let expiry = until.unwrap_or(at + (policy.unknown_rejection_hours * HOUR as f64) as Ms);
    let answered_since = WINDOWS
        .iter()
        .any(|w| windows[*w].read_at.is_some_and(|read| read > at))
        && WINDOWS.iter().all(|w| !windows[*w].usage_blocked);
    if expiry <= now || answered_since {
        return None;
    }
    Some(UnknownRejection { at, until })
}

/// An account's state.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountState {
    /// Identity: every pick and decision names accounts by id.
    pub id: String,
    /// The label, for display.
    pub name: String,
    /// How it is used when new work is placed.
    pub mark: Mark,
    /// Its place in the operator's order, from 0.
    pub index: usize,
    /// Its windows. The weekly one is the strictest weekly limit a chat can meet here, which may be
    /// one model's own (it has that model's label then): what holds while the model of a chat is
    /// not known. [`AccountState::for_model`] gives the account as a chat on one model meets it.
    pub windows: PerWindow<WindowState>,
    /// The all-models weekly window by itself: the week every model draws on.
    pub weekly_all: WindowState,
    /// Each model's own weekly limit that has not reset yet, labelled with its model.
    pub weekly_own: Vec<WindowState>,
    /// When nothing blocks it but models' own weekly limits: those models. A chat on any other
    /// model can still run here. Empty when it is open, or blocked for every model.
    pub limited_models: Vec<String>,
    /// A refusal with no window, still in force.
    pub unknown_rejection: Option<UnknownRejection>,
    /// Whether it can take no work now.
    pub blocked: bool,
    /// What blocks it.
    pub blocked_by: Vec<Limit>,
    /// When it is available: now when open; the latest end among its blocks; None when unknown.
    pub available_at: Option<Ms>,
    /// Whether that time is known.
    pub available_known: bool,
    /// Weekly headroom per hour left before the weekly reset: headroom not used by then is lost.
    pub urgency: f64,
    /// The 5-hour window's headroom.
    pub five_hour_headroom: f64,
    /// The weekly window's headroom.
    pub weekly_headroom: f64,
    /// The smaller of the two.
    pub effective_headroom: f64,
    /// When its reading was last written.
    pub reading_at: Option<Ms>,
}

/// An account's state: its windows, whether it is blocked and by what, when it is available (now; the
/// latest end among its blocks, a weekly block dominating; None = unknown), its weekly urgency (weekly
/// headroom per hour left before the weekly reset: headroom not used by then is lost) and its
/// effective headroom (the smaller of the two).
pub fn account_state(
    account: &Account,
    reading: Option<&Reading>,
    rejections: Option<&Rejections>,
    policy: &Policy,
    now: Ms,
    index: usize,
) -> AccountState {
    let limits = thresholds(account, policy);
    let none = Rejections::default();
    let rejections = rejections.unwrap_or(&none);
    // The 5-hour window: none at all on a plan whose poll lists no such meter.
    let no_five = reading.is_some_and(no_five_hour);
    let five_reading = reading.filter(|_| !no_five).and_then(|r| {
        let own = r.five_hour.as_ref()?;
        Some(WindowReading {
            source: own.source.clone().or_else(|| r.source.clone()),
            ..own.clone()
        })
    });
    let mut five_hour = window_state(
        Window::FiveHour,
        five_reading.as_ref(),
        rejections.get(Limit::FiveHour),
        limits[Window::FiveHour],
        now,
        policy,
    );
    five_hour.absent = no_five;
    // The week: what every model draws on, and each model's own limit beside it. A refusal that
    // names a model ("You've hit your Fable limit") is that model's, and leaves the week open.
    let refusal = rejections.get(Limit::SevenDay);
    let scope = refusal
        .and_then(|refusal| refusal.message.as_deref())
        .and_then(limit_scope);
    let week_of = |own: Option<&WindowReading>, refused: bool| {
        window_state(
            Window::SevenDay,
            own,
            refusal.filter(|_| refused),
            limits[Window::SevenDay],
            now,
            policy,
        )
    };
    let named = |own: &WindowReading| match (&scope, &own.label) {
        (Some(scope), Some(label)) => same_model(scope, label),
        _ => false,
    };
    let weekly_all = week_of(all_weekly(reading).as_ref(), scope.is_none());
    let own = own_weekly(reading, now);
    let mut weekly_own: Vec<WindowState> = own.iter().map(|own| week_of(Some(own), named(own))).collect();
    // A refusal that names a model no poll has listed a limit for: that model's limit all the same.
    if let Some(scope) = scope.as_ref().filter(|_| !own.iter().any(named)) {
        let mut refused = week_of(None, true);
        if refused.blocked {
            refused.label = Some(scope.clone());
            weekly_own.push(refused);
        }
    }
    let seven_day = weekly_own.iter().fold(weekly_all.clone(), |strictest, own| {
        if stricter(own, &strictest) {
            own.clone()
        } else {
            strictest
        }
    });
    let windows = PerWindow { five_hour, seven_day };
    let other = unknown_rejection(rejections.unknown.as_ref(), &windows, now, policy);
    let mut state = AccountState {
        id: account.id.clone(),
        name: account.name.clone(),
        mark: account.mark,
        index,
        unknown_rejection: other,
        blocked: false,
        blocked_by: Vec::new(),
        available_at: Some(now),
        available_known: true,
        urgency: 0.0,
        five_hour_headroom: 0.0,
        weekly_headroom: 0.0,
        effective_headroom: 0.0,
        reading_at: reading.and_then(|r| r.at.as_deref()).and_then(parse_ms),
        windows,
        weekly_all,
        weekly_own,
        limited_models: Vec::new(),
    };
    state.settle(now);
    state
}

/// Whether window `a` holds a chat back more than `b`: one that blocks before one that does not,
/// then the fuller one.
fn stricter(a: &WindowState, b: &WindowState) -> bool {
    if a.blocked != b.blocked {
        return a.blocked;
    }
    a.used.unwrap_or(-1.0) > b.used.unwrap_or(-1.0)
}

impl AccountState {
    /// Works out, from its windows, whether it is blocked and by what, when it is available, and
    /// how much room it has.
    fn settle(&mut self, now: Ms) {
        let windows = &self.windows;
        let other = self.unknown_rejection;
        let mut blocked_by: Vec<Limit> = WINDOWS
            .iter()
            .filter(|w| windows[**w].blocked)
            .map(|w| Limit::from(*w))
            .collect();
        if other.is_some() {
            blocked_by.push(Limit::Unknown);
        }
        let blocked = !blocked_by.is_empty();
        let mut available_at = Some(now);
        let mut available_known = true;
        if blocked {
            let ends: Vec<Option<Ms>> = blocked_by
                .iter()
                .map(|limit| match limit.window() {
                    Some(w) => windows[w].block_until,
                    None => other.and_then(|o| o.until),
                })
                .collect();
            available_known = ends.iter().all(Option::is_some);
            available_at = if available_known {
                ends.into_iter().flatten().max()
            } else {
                None
            };
        }
        let week = &windows.seven_day;
        let hours_left = match (week.used, week.resets_at) {
            (Some(_), Some(resets)) => ((resets - now) as f64 / HOUR as f64).max(1.0 / 60.0),
            _ => 168.0,
        };
        // Blocked by nothing but a model's own limit, the week itself still open: the models
        // whose limit it is.
        self.limited_models =
            if blocked_by == [Limit::SevenDay] && week.label.is_some() && !self.weekly_all.blocked {
                self.weekly_own
                    .iter()
                    .filter(|own| own.blocked)
                    .filter_map(|own| own.label.clone())
                    .collect()
            } else {
                Vec::new()
            };
        self.urgency = week.headroom.max(0.0) / hours_left;
        self.five_hour_headroom = windows.five_hour.headroom;
        self.weekly_headroom = week.headroom;
        self.effective_headroom = windows.five_hour.headroom.min(week.headroom);
        self.blocked = blocked;
        self.blocked_by = blocked_by;
        self.available_at = available_at;
        self.available_known = available_known;
    }

    /// The account as a chat that runs `model` meets it: its weekly window is the week every model
    /// draws on, or that model's own limit when it is the stricter. The limit of a model the chat
    /// does not run holds nothing back. With no model known, the account as it is: the strictest
    /// limit any chat can meet.
    pub fn for_model(&self, model: Option<&str>, now: Ms) -> AccountState {
        let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) else {
            return self.clone();
        };
        let mut state = self.clone();
        state.windows.seven_day = self
            .weekly_own
            .iter()
            .filter(|own| own.label.as_deref().is_some_and(|label| covers(label, model)))
            .fold(self.weekly_all.clone(), |strictest, own| {
                if stricter(own, &strictest) {
                    own.clone()
                } else {
                    strictest
                }
            });
        state.settle(now);
        state
    }
}

/// Every account as a chat that runs `model` meets it (see [`AccountState::for_model`]).
pub fn states_for(states: &[AccountState], model: Option<&str>, now: Ms) -> Vec<AccountState> {
    states.iter().map(|state| state.for_model(model, now)).collect()
}

/// The state of every account, in the operator's order.
pub fn account_states(
    accounts: &[Account],
    readings: &BTreeMap<String, Reading>,
    rejections: &BTreeMap<String, Rejections>,
    policy: &Policy,
    now: Ms,
) -> Vec<AccountState> {
    accounts
        .iter()
        .enumerate()
        .map(|(index, account)| {
            account_state(
                account,
                readings.get(&account.id),
                rejections.get(&account.id),
                policy,
                now,
                index,
            )
        })
        .collect()
}

/// Larger first, for sorting numbers that are never NaN.
fn descending(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

/// The account that is available first: known times before unknown ones, earliest first, ties by order.
pub fn earliest(states: &[AccountState]) -> Option<&AccountState> {
    let mut sorted: Vec<&AccountState> = states.iter().collect();
    sorted.sort_by(|a, b| {
        b.available_known
            .cmp(&a.available_known)
            .then_with(|| {
                if a.available_known {
                    a.available_at.cmp(&b.available_at)
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| a.index.cmp(&b.index))
    });
    sorted.first().copied()
}

/// Whether an account is blocked by its 5-hour window alone.
pub fn five_hour_only(state: Option<&AccountState>) -> bool {
    state.is_some_and(|s| !s.blocked_by.is_empty() && s.blocked_by.iter().all(|w| *w == Limit::FiveHour))
}

/// Where new work goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick<'a> {
    /// The account's id; None when nothing is open.
    pub account: Option<String>,
    /// Why: `preferred`, `urgency`, `low-reserve`, `last-resort`, `order`, `all-blocked`.
    pub reason: &'static str,
    /// Whether nothing is open.
    pub all_blocked: bool,
    /// When nothing is open: the account that is back first.
    pub earliest: Option<String>,
    /// When the chosen (or the earliest) account is available.
    pub available_at: Option<Ms>,
    /// Whether that time is known.
    pub available_known: bool,
    /// The chosen (or the earliest) account's state.
    pub state: Option<&'a AccountState>,
}

/// Where new work (a session start, a resume, a hop) goes.
///
/// smart: an unblocked preferred account with at least startReserve 5-hour headroom (first in order);
/// else the normal account with the highest weekly urgency among those with that reserve (ties by
/// order); else the unblocked non-last-resort account with the most 5-hour headroom; else a
/// last-resort account (first in order). order: the first unblocked account in order, last resort only
/// when every other is blocked. When nothing is open: `account: None, all_blocked: true`, with the
/// earliest account over every account, excluded ones included.
pub fn pick_start<'a>(states: &'a [AccountState], policy: &Policy, exclude: &[&str]) -> Pick<'a> {
    let mut open: Vec<&AccountState> = states
        .iter()
        .filter(|s| !exclude.contains(&s.id.as_str()) && !s.blocked)
        .collect();
    open.sort_by_key(|s| s.index);
    let chosen = |state: &'a AccountState, reason: &'static str| Pick {
        account: Some(state.id.clone()),
        reason,
        all_blocked: false,
        earliest: None,
        available_at: state.available_at,
        available_known: true,
        state: Some(state),
    };
    let last_resort = open.iter().copied().find(|s| s.mark == Mark::LastResort);
    if policy.mode == Mode::Order {
        if let Some(first) = open.iter().copied().find(|s| s.mark != Mark::LastResort) {
            return chosen(first, "order");
        }
        if let Some(last) = last_resort {
            return chosen(last, "last-resort");
        }
    } else {
        let reserve = policy.start_reserve;
        if let Some(preferred) = open
            .iter()
            .copied()
            .find(|s| s.mark == Mark::Preferred && s.five_hour_headroom >= reserve)
        {
            return chosen(preferred, "preferred");
        }
        let mut normal: Vec<&AccountState> = open
            .iter()
            .copied()
            .filter(|s| s.mark == Mark::Normal && s.five_hour_headroom >= reserve)
            .collect();
        normal.sort_by(|a, b| descending(a.urgency, b.urgency).then_with(|| a.index.cmp(&b.index)));
        if let Some(normal) = normal.first().copied() {
            return chosen(normal, "urgency");
        }
        let mut rest: Vec<&AccountState> = open
            .iter()
            .copied()
            .filter(|s| s.mark != Mark::LastResort)
            .collect();
        rest.sort_by(|a, b| {
            descending(a.five_hour_headroom, b.five_hour_headroom).then_with(|| a.index.cmp(&b.index))
        });
        if let Some(rest) = rest.first().copied() {
            return chosen(rest, "low-reserve");
        }
        if let Some(last) = last_resort {
            return chosen(last, "last-resort");
        }
    }
    let first = earliest(states);
    Pick {
        account: None,
        reason: "all-blocked",
        all_blocked: true,
        earliest: first.map(|s| s.id.clone()),
        available_at: first.and_then(|s| s.available_at),
        available_known: first.is_some_and(|s| s.available_known),
        state: first,
    }
}

/// The accounts a run of hops has left, oldest first, while the run lasts. Hops follow one another
/// until the conversation settles: a turn ended well on the account it is on (`ended_well`), and it
/// has been there for minStayMinutes. `left` is every account it left since it last settled; once it
/// has settled again the run is over and nothing is held against them.
pub fn hop_run<'a>(
    left: &'a [String],
    arrived_at: Option<Ms>,
    ended_well: bool,
    policy: &Policy,
    now: Ms,
) -> &'a [String] {
    let stayed = arrived_at.is_none_or(|at| (now - at) as f64 >= policy.min_stay_minutes * MINUTE as f64);
    if ended_well && stayed { &[] } else { left }
}

/// Where a conversation goes from `current`: as new work does ([`pick_start`]), but not back to an
/// account it left in this run of hops (`left`, oldest first; see [`hop_run`]) while another is open.
/// Two accounts that each fail in a way Baton cannot see (a sign-in that is refused, say) would
/// otherwise hand the conversation back and forth past every other account. When it has been
/// through all of them, it goes back to the one it left longest ago.
pub fn pick_hop<'a>(states: &'a [AccountState], policy: &Policy, current: &str, left: &[String]) -> Pick<'a> {
    let mut kept = left;
    loop {
        let mut exclude = vec![current];
        exclude.extend(kept.iter().map(String::as_str));
        let pick = pick_start(states, policy, &exclude);
        match kept.split_first() {
            Some((_, later)) if pick.account.is_none() => kept = later,
            _ => return pick,
        }
    }
}

/// Where a background agent goes.
#[derive(Clone, Debug, PartialEq)]
pub struct Spread {
    /// The account's id; None when there is no account at all.
    pub account: Option<String>,
    /// Why: `spread`, `last-resort`, `all-blocked`.
    pub reason: &'static str,
    /// The account's score, for `spread`.
    pub score: Option<f64>,
    /// Whether the work waits for the account.
    pub queue: bool,
    /// When it is available, for a queued one.
    pub at: Option<Ms>,
    /// Whether that time is known.
    pub available_known: bool,
}

/// Background agents: spread new agents across the unblocked accounts by effective headroom minus the
/// estimated cost of the agents already running there, weighted by weekly urgency (score = headroom x
/// (0.5 + 0.5 x urgency / highest urgency)), so the accounts run out together. Preferred accounts with
/// startReserve left are filled first; last resort only when no other account has room; when nothing
/// is open the work is queued for the earliest account at its available_at.
pub fn pick_spread(states: &[AccountState], policy: &Policy, running_cost: &BTreeMap<String, f64>) -> Spread {
    let open: Vec<&AccountState> = states.iter().filter(|s| !s.blocked).collect();
    let room = |s: &AccountState| s.effective_headroom - running_cost.get(&s.id).copied().unwrap_or(0.0);
    let regular: Vec<&AccountState> = open
        .iter()
        .copied()
        .filter(|s| s.mark != Mark::LastResort && room(s) > 0.0)
        .collect();
    let preferred: Vec<&AccountState> = regular
        .iter()
        .copied()
        .filter(|s| s.mark == Mark::Preferred && room(s) >= policy.start_reserve)
        .collect();
    let mut pool = if preferred.is_empty() { regular } else { preferred };
    if !pool.is_empty() {
        let top = pool.iter().map(|s| s.urgency).fold(f64::NEG_INFINITY, f64::max);
        let score = |s: &AccountState| room(s) * (0.5 + 0.5 * if top > 0.0 { s.urgency / top } else { 1.0 });
        pool.sort_by(|a, b| descending(score(a), score(b)).then_with(|| a.index.cmp(&b.index)));
        let best = pool[0];
        return Spread {
            account: Some(best.id.clone()),
            reason: "spread",
            score: Some(score(best)),
            queue: false,
            at: None,
            available_known: true,
        };
    }
    let mut last: Vec<&AccountState> = open
        .iter()
        .copied()
        .filter(|s| s.mark == Mark::LastResort)
        .collect();
    last.sort_by_key(|s| s.index);
    if let Some(last) = last.first() {
        return Spread {
            account: Some(last.id.clone()),
            reason: "last-resort",
            score: None,
            queue: false,
            at: None,
            available_known: true,
        };
    }
    let first = earliest(states);
    Spread {
        account: first.map(|s| s.id.clone()),
        reason: "all-blocked",
        score: None,
        queue: true,
        at: first.and_then(|s| s.available_at),
        available_known: first.is_some_and(|s| s.available_known),
    }
}

/// A session's cost per turn in each window: the moving average (last costSamples) of the used-%
/// deltas between consecutive turn-end readings of that session on the same account and window period
/// (a reset in between, or a drop, is skipped). None where there is no sample yet.
pub fn turn_cost(samples: &[TurnSample], policy: &Policy) -> PerWindow<Option<f64>> {
    PerWindow::from_fn(|w| {
        let mut deltas = Vec::new();
        for pair in samples.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            let (Some(x), Some(y)) = (&a.windows[w], &b.windows[w]) else {
                continue;
            };
            if a.account != b.account {
                continue;
            }
            if !same_period(x.resets_at.as_deref(), y.resets_at.as_deref()) || y.used < x.used {
                continue;
            }
            deltas.push(y.used - x.used);
        }
        let keep = (policy.cost_samples.max(0.0) as usize).min(deltas.len());
        let recent = &deltas[deltas.len() - keep..];
        (!recent.is_empty()).then(|| recent.iter().sum::<f64>() / recent.len() as f64)
    })
}

/// The session's recent burn rate (% per hour) in each window: from the first to the last of its
/// readings within burnWindowMinutes on the latest account and window period.
pub fn burn_rate(samples: &[TurnSample], policy: &Policy, now: Ms) -> PerWindow<Option<f64>> {
    let since = now - (policy.burn_window_minutes * MINUTE as f64) as Ms;
    PerWindow::from_fn(|w| {
        let points: Vec<&TurnSample> = samples
            .iter()
            .filter(|s| s.at.unwrap_or(0) >= since && s.windows[w].is_some())
            .collect();
        let last = *points.last()?;
        let last_window = last.windows[w].as_ref()?;
        let first = points.iter().copied().find(|p| {
            p.account == last.account
                && same_period(
                    p.windows[w].as_ref().and_then(|x| x.resets_at.as_deref()),
                    last_window.resets_at.as_deref(),
                )
        })?;
        let hours = (last.at? - first.at?) as f64 / HOUR as f64;
        (hours > 0.0).then(|| {
            ((last_window.used - first.windows[w].as_ref().map_or(0.0, |x| x.used)) / hours).max(0.0)
        })
    })
}

/// Hours until the account is blocked at the given burn rate (infinity when a window resets first or
/// nothing burns).
pub fn hours_to_block(state: &AccountState, burn: Option<&PerWindow<Option<f64>>>, now: Ms) -> f64 {
    let mut best = f64::INFINITY;
    for w in WINDOWS {
        let Some(rate) = burn.and_then(|b| b[w]).filter(|rate| *rate > 0.0) else {
            continue;
        };
        let ws = &state.windows[w];
        let hours = ws.headroom.max(0.0) / rate;
        let reset_in = match (ws.used, ws.resets_at) {
            (Some(_), Some(resets)) => (resets - now) as f64 / HOUR as f64,
            _ => f64::INFINITY,
        };
        if hours < reset_in {
            best = best.min(hours);
        }
    }
    best
}

/// The background tasks (of a turn's `backgroundTasks`) a conversation is not moved under.
pub fn busy_tasks(tasks: &Value) -> Vec<&Value> {
    tasks
        .as_array()
        .map(|tasks| {
            tasks
                .iter()
                .filter(|t| t.text("type").is_some_and(|kind| BUSY_TASKS.contains(&kind)))
                .collect()
        })
        .unwrap_or_default()
}

/// What the wrapper knows of the session at a turn end.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionFacts {
    /// When the conversation arrived on the current account.
    pub arrived_at: Option<Ms>,
    /// The operator chose to stay on a limited account until then.
    pub stay_until: Option<Ms>,
    /// The operator declined a planned move until then.
    pub veto_until: Option<Ms>,
    /// Refusals right after a resume, in a row.
    pub reblocks: f64,
    /// The accounts this run of hops has left, oldest first (see [`hop_run`]): the conversation
    /// does not go back to one of them while another account is open.
    pub left: Vec<String>,
    /// Whether the conversation was just compacted.
    pub compacted: bool,
    /// The turn's background tasks, as the Stop hook reported them.
    pub background_tasks: Value,
    /// The session's cost per turn.
    pub cost: PerWindow<Option<f64>>,
    /// The session's burn rate.
    pub burn: Option<PerWindow<Option<f64>>>,
}

/// The end of a turn, as the wrapper sees it.
#[derive(Clone, Debug)]
pub struct TurnEnd<'a> {
    /// The time now.
    pub now: Ms,
    /// The policy in force.
    pub policy: &'a Policy,
    /// Every account's state.
    pub states: &'a [AccountState],
    /// The account the conversation runs on.
    pub current: &'a str,
    /// `stop` or `stop-failure`.
    pub event: &'a str,
    /// A failed turn's error.
    pub failure_error: Option<&'a str>,
    /// A hop the operator asked for: `Some(None)` to the best account, `Some(Some(id))` to that one.
    pub hop_request: Option<Option<&'a str>>,
    /// What the wrapper knows of the session.
    pub session: SessionFacts,
}

/// What the session does at the end of a turn.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Nothing: the conversation stays.
    Stay {
        /// Why.
        reason: &'static str,
        /// For a planned move with no target: the windows that are short.
        windows: Vec<Window>,
    },
    /// Leave claude running (Claude Code's own wait-at-limit).
    WaitInPlace {
        /// Why.
        reason: &'static str,
        /// When the limit lifts, when known.
        at: Option<Ms>,
    },
    /// End claude at this turn end and resume on `to`.
    Hop {
        /// The account to resume on.
        to: String,
        /// Why: `manual`, `refusal`, `planned`, `compaction`.
        reason: &'static str,
        /// Whether to count down first.
        countdown: bool,
        /// For a planned move: the windows that are short.
        windows: Vec<Window>,
    },
    /// End claude, wait until `at`, then resume on `to`.
    Wait {
        /// The account to resume on.
        to: String,
        /// When.
        at: Ms,
        /// Why.
        reason: &'static str,
    },
}

impl Decision {
    /// Staying, for `reason`.
    fn stay(reason: &'static str) -> Decision {
        Decision::Stay {
            reason,
            windows: Vec::new(),
        }
    }

    /// The decision's kind: `none`, `wait-in-place`, `hop`, `wait`.
    pub fn kind(&self) -> &'static str {
        match self {
            Decision::Stay { .. } => "none",
            Decision::WaitInPlace { .. } => "wait-in-place",
            Decision::Hop { .. } => "hop",
            Decision::Wait { .. } => "wait",
        }
    }

    /// Why.
    pub fn reason(&self) -> &'static str {
        match self {
            Decision::Stay { reason, .. }
            | Decision::WaitInPlace { reason, .. }
            | Decision::Hop { reason, .. }
            | Decision::Wait { reason, .. } => reason,
        }
    }
}

/// What the session does at the end of a turn.
pub fn decide_turn_end(input: &TurnEnd<'_>) -> Decision {
    let TurnEnd {
        now,
        policy,
        states,
        current,
        event,
        ..
    } = *input;
    let session = &input.session;
    let cur = states.iter().find(|s| s.id == current);
    // Where the conversation goes from here: not back to an account this run of hops has left.
    let elsewhere = || pick_hop(states, policy, current, &session.left);

    if let Some(request) = input.hop_request {
        if let Some(to) = request.filter(|to| !to.is_empty()) {
            if to == current {
                return Decision::stay("manual-same-account");
            }
            if !states.iter().any(|s| s.id == to) {
                return Decision::stay("manual-unknown-account");
            }
            return Decision::Hop {
                to: to.to_owned(),
                reason: "manual",
                countdown: false,
                windows: Vec::new(),
            };
        }
        return match elsewhere().account {
            Some(to) => Decision::Hop {
                to,
                reason: "manual",
                countdown: false,
                windows: Vec::new(),
            },
            None => Decision::stay("manual-no-account"),
        };
    }

    if event == "stop-failure" {
        if !input
            .failure_error
            .is_some_and(|error| LIMIT_ERRORS.contains(&error))
        {
            return Decision::stay("other-error");
        }
        if session.stay_until.is_some_and(|until| until > now) {
            return Decision::stay("stayed");
        }
        if session.reblocks >= policy.max_reblocks {
            let pick = pick_start(states, policy, &[]);
            let ready = match (&pick.account, pick.available_known, pick.available_at) {
                (None, true, Some(at)) => at,
                _ => now,
            };
            let target = pick
                .account
                .or(pick.earliest)
                .unwrap_or_else(|| current.to_owned());
            return Decision::Wait {
                to: target,
                at: ready.max(now + (policy.reblock_backoff_minutes * MINUTE as f64) as Ms),
                reason: "reblocked",
            };
        }
        if let Some(cur) = cur.filter(|cur| five_hour_only(Some(cur)) && cur.available_known)
            && let Some(at) = cur.available_at
            && (at - now) as f64 <= policy.wait_in_place_minutes * MINUTE as f64
        {
            return Decision::WaitInPlace {
                reason: "reset-soon",
                at: Some(at),
            };
        }
        let pick = elsewhere();
        if let Some(to) = pick.account {
            return Decision::Hop {
                to,
                reason: "refusal",
                countdown: true,
                windows: Vec::new(),
            };
        }
        let Some(first) = pick.state else {
            return Decision::WaitInPlace {
                reason: "no-accounts",
                at: None,
            };
        };
        let Some(first_at) = first.available_at.filter(|_| first.available_known) else {
            return Decision::WaitInPlace {
                reason: "all-blocked-unknown",
                at: None,
            };
        };
        if first.id == current
            && five_hour_only(cur)
            && (first_at - now) as f64 <= policy.wait_in_place_max_hours * HOUR as f64
        {
            return Decision::WaitInPlace {
                reason: "all-blocked-current-first",
                at: Some(first_at),
            };
        }
        return Decision::Wait {
            to: first.id.clone(),
            at: first_at,
            reason: "all-blocked-wait",
        };
    }

    let Some(cur) = cur.filter(|_| event == "stop") else {
        return Decision::stay("not-a-turn-end");
    };
    if policy.mode != Mode::Smart {
        return Decision::stay("order-mode");
    }
    if session.veto_until.is_some_and(|until| until > now) {
        return Decision::stay("vetoed");
    }
    if !busy_tasks(&session.background_tasks).is_empty() {
        return Decision::stay("busy");
    }
    if session
        .arrived_at
        .is_some_and(|at| at != 0 && ((now - at) as f64) < policy.min_stay_minutes * MINUTE as f64)
    {
        return Decision::stay("min-stay");
    }

    let cost = &session.cost;
    let short: Vec<Window> = WINDOWS
        .into_iter()
        .filter(|w| cost[*w].is_some_and(|c| cur.windows[*w].headroom < policy.plan_factor * c))
        .collect();
    if !short.is_empty() {
        let pick = elsewhere();
        let roomy = pick
            .state
            .filter(|_| pick.account.is_some())
            .is_some_and(|state| {
                WINDOWS
                    .iter()
                    .all(|w| cost[*w].is_none_or(|c| state.windows[*w].headroom >= policy.plan_factor * c))
            });
        return match pick.account {
            Some(to) if roomy => Decision::Hop {
                to,
                reason: "planned",
                countdown: true,
                windows: short,
            },
            _ => Decision::Stay {
                reason: "planned-no-target",
                windows: short,
            },
        };
    }

    if session.compacted && policy.compaction_rebalance {
        let hours = hours_to_block(cur, session.burn.as_ref(), now);
        if hours <= policy.rebalance_horizon_minutes / 60.0 {
            let pick = elsewhere();
            if let (Some(to), Some(state)) = (pick.account, pick.state)
                && state.effective_headroom >= cur.effective_headroom + policy.rebalance_margin
            {
                return Decision::Hop {
                    to,
                    reason: "compaction",
                    countdown: true,
                    windows: Vec::new(),
                };
            }
        }
    }
    Decision::stay("stay")
}

/// What the wrapper does while it waits for an account.
#[derive(Clone, Debug, PartialEq)]
pub enum WaitDecision {
    /// An account is open: resume there.
    Resume {
        /// The account.
        to: String,
        /// Why it was picked.
        reason: &'static str,
    },
    /// Keep waiting.
    Wait {
        /// The account waited for.
        to: Option<String>,
        /// Until when.
        at: Option<Ms>,
    },
}

/// While the wrapper waits for an account (all blocked): resume as soon as any account is open, else
/// keep the earliest known target (re-picked from fresh readings every time).
pub fn decide_wait(
    states: &[AccountState],
    policy: &Policy,
    target: Option<&str>,
    at: Option<Ms>,
) -> WaitDecision {
    let pick = pick_start(states, policy, &[]);
    if let Some(to) = pick.account {
        return WaitDecision::Resume {
            to,
            reason: pick.reason,
        };
    }
    if let Some(state) = pick.state.filter(|s| s.available_known) {
        return WaitDecision::Wait {
            to: Some(state.id.clone()),
            at: state.available_at,
        };
    }
    WaitDecision::Wait {
        to: target.map(str::to_owned),
        at,
    }
}
