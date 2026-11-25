//! The policy end to end, over plain data: when an account is available, where new work goes, what a
//! turn costs, and what the session does at the end of a turn.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot build its data stops the test at once, as the tests do"
)]

use std::collections::BTreeMap;

use baton_core::clock::{DAY as D, HOUR as H, MINUTE as M, Ms, iso};
use baton_core::config::{Account, Mark, Mode, Policy};
use baton_core::policy::{
    AccountState, Decision, SessionFacts, TurnEnd, WaitDecision, account_state, account_states, burn_rate,
    decide_turn_end, decide_wait, earliest, hop_run, hours_to_block, pick_hop, pick_spread, pick_start,
    states_for, turn_cost,
};
use baton_core::usage::{Others, Reading, Rejection, Rejections, Rise, TurnSample};
use baton_core::window::{Limit, PerWindow, Window};
use serde_json::{Value, json};

/// The moment every test runs at: 2026-10-25T12:00:00Z.
const NOW: Ms = 1_792_929_600_000;

/// One window of a reading: percent used, and how long from now it resets (None: unknown).
type Win = Option<(f64, Option<Ms>)>;

/// An account with nothing special about it; its id is its name.
fn account(name: &str) -> Account {
    marked(name, Mark::Normal)
}

/// An account with a mark.
fn marked(name: &str, mark: Mark) -> Account {
    Account {
        id: name.to_owned(),
        name: name.to_owned(),
        mark,
        five_hour: None,
        weekly: None,
    }
}

/// A reading of both windows taken `ago` before now.
fn reading_of(five: Win, week: Win, ago: Ms) -> Reading {
    let window = |v: Win| {
        v.map(|(used, resets_in)| {
            json!({ "used": used, "resetsAt": resets_in.map(|r| iso(NOW + r)), "readAt": iso(NOW - ago) })
        })
    };
    serde_json::from_value(
        json!({ "at": iso(NOW - ago), "five_hour": window(five), "seven_day": window(week) }),
    )
    .unwrap()
}

/// A reading taken a minute ago: each window as percent used and how long from now it resets.
fn reading(five: (f64, Ms), week: (f64, Ms)) -> Reading {
    read_ago(five, week, M)
}

/// [`reading`], taken `ago` before now.
fn read_ago(five: (f64, Ms), week: (f64, Ms), ago: Ms) -> Reading {
    reading_of(Some((five.0, Some(five.1))), Some((week.0, Some(week.1))), ago)
}

/// A reading taken a minute ago whose 5-hour window has no known reset.
fn five_unknown(used: f64, week: (f64, Ms)) -> Reading {
    reading_of(Some((used, None)), Some((week.0, Some(week.1))), M)
}

/// A refusal for `limit` at `at`, lifting at `until` when that is known.
fn rejection(limit: Limit, at: Ms, until: Option<Ms>) -> Rejections {
    let mut out = Rejections::default();
    out.set(
        limit,
        Rejection {
            window: limit.as_str().to_owned(),
            at: Some(iso(at)),
            until: until.map(iso),
            ..Rejection::default()
        },
    );
    out
}

/// An account with what is known of it.
struct Case {
    /// The account.
    account: Account,
    /// Its latest reading.
    reading: Option<Reading>,
    /// Its last refusals.
    rejections: Rejections,
}

/// An account with a reading.
fn on(account: Account, reading: Reading) -> Case {
    Case {
        account,
        reading: Some(reading),
        rejections: Rejections::default(),
    }
}

/// An account nothing is known of.
fn bare(account: Account) -> Case {
    Case {
        account,
        reading: None,
        rejections: Rejections::default(),
    }
}

/// An account with refusals, and a reading when there is one.
fn refused(account: Account, reading: Option<Reading>, rejections: Rejections) -> Case {
    Case {
        account,
        reading,
        rejections,
    }
}

/// The states of these accounts under a policy.
fn states_under(list: Vec<Case>, policy: &Policy) -> Vec<AccountState> {
    let accounts: Vec<Account> = list.iter().map(|case| case.account.clone()).collect();
    let mut readings = BTreeMap::new();
    let mut rejections = BTreeMap::new();
    for case in list {
        if let Some(reading) = case.reading {
            readings.insert(case.account.id.clone(), reading);
        }
        rejections.insert(case.account.id, case.rejections);
    }
    account_states(&accounts, &readings, &rejections, policy, NOW)
}

/// The states of these accounts under the default policy.
fn states(list: Vec<Case>) -> Vec<AccountState> {
    states_under(list, &Policy::default())
}

/// The state of one account, "a", with this reading.
fn one(reading: Reading) -> AccountState {
    states(vec![on(account("a"), reading)]).remove(0)
}

/// The state of one account, "a", with these refusals and maybe a reading.
fn one_refused(reading: Option<Reading>, rejections: Rejections) -> AccountState {
    states(vec![refused(account("a"), reading, rejections)]).remove(0)
}

/// Whether two numbers are the same but for rounding.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// The id of the account new work goes to, and why.
fn picked(states: &[AccountState], policy: &Policy) -> (Option<String>, &'static str) {
    let pick = pick_start(states, policy, &[]);
    (pick.account, pick.reason)
}

/// A cost or a burn rate in both windows.
fn both(five_hour: f64, seven_day: f64) -> PerWindow<Option<f64>> {
    PerWindow {
        five_hour: Some(five_hour),
        seven_day: Some(seven_day),
    }
}

/// A hop for `reason`, with a countdown unless it is manual.
fn hop(to: &str, reason: &'static str, windows: Vec<Window>) -> Decision {
    Decision::Hop {
        to: to.to_owned(),
        reason,
        countdown: reason != "manual",
        windows,
    }
}

#[test]
fn available_at_a_five_hour_block_alone() {
    let s = one(reading((92.0, 2 * H), (50.0, 3 * D)));
    assert_eq!(s.blocked_by, [Limit::FiveHour]);
    assert_eq!(s.available_at, Some(NOW + 2 * H));
    assert!(s.available_known);
    assert_eq!(s.windows.five_hour.headroom, -2.0);
}

#[test]
fn available_at_a_weekly_block_alone() {
    let s = one(reading((10.0, 2 * H), (96.0, 3 * D)));
    assert_eq!(s.blocked_by, [Limit::SevenDay]);
    assert_eq!(s.available_at, Some(NOW + 3 * D));
}

#[test]
fn available_at_both_windows_blocked_the_weekly_reset_dominates() {
    let s = one(reading((95.0, 2 * H), (97.0, 3 * D)));
    assert_eq!(s.blocked_by, [Limit::FiveHour, Limit::SevenDay]);
    assert_eq!(s.available_at, Some(NOW + 3 * D));
}

#[test]
fn available_at_under_the_thresholds_is_available_now() {
    let s = one(reading((89.9, 2 * H), (94.0, 3 * D)));
    assert!(!s.blocked);
    assert_eq!(s.available_at, Some(NOW));
    assert!(close(s.five_hour_headroom, 0.1));
}

#[test]
fn available_at_a_refusal_blocks_until_its_known_end_and_a_later_reading_with_room_supersedes_it() {
    let blocked = one_refused(
        Some(read_ago((60.0, 2 * H), (40.0, 3 * D), 5 * M)),
        rejection(Limit::FiveHour, NOW - M, Some(NOW + H)),
    );
    assert_eq!(blocked.blocked_by, [Limit::FiveHour]);
    assert_eq!(blocked.available_at, Some(NOW + H));
    assert!(blocked.windows.five_hour.rejected);
    let superseded = one_refused(
        Some(read_ago((30.0, 2 * H), (40.0, 3 * D), 0)),
        rejection(Limit::FiveHour, NOW - 5 * M, Some(NOW + H)),
    );
    assert!(!superseded.blocked);
    let expired = one_refused(None, rejection(Limit::FiveHour, NOW - 2 * H, Some(NOW - M)));
    assert!(!expired.blocked);
}

#[test]
fn available_at_a_reading_past_its_reset_or_a_five_hour_reading_older_than_five_hours_counts_as_reset() {
    let past = one(reading((99.0, -M), (97.0, -M)));
    assert!(!past.blocked);
    assert_eq!(past.windows.five_hour.used, None);
    assert!(past.windows.five_hour.reset);
    assert_eq!(past.windows.five_hour.headroom, 90.0);
    let old = one(read_ago((99.0, H), (10.0, 3 * D), 5 * H + M));
    assert!(old.windows.five_hour.stale);
    assert!(!old.blocked);
    let recent = one(read_ago((99.0, H), (10.0, 3 * D), 4 * H));
    assert!(recent.blocked);
}

#[test]
fn available_at_an_unknown_reset_of_a_blocking_window_is_unknown_and_an_unknown_refusal_expires() {
    let s = one(five_unknown(95.0, (10.0, 3 * D)));
    assert!(s.blocked);
    assert!(!s.available_known);
    assert_eq!(s.available_at, None);
    let refused = one_refused(None, rejection(Limit::FiveHour, NOW - H, None));
    assert!(refused.blocked);
    assert!(!refused.available_known);
    let lapsed = one_refused(None, rejection(Limit::FiveHour, NOW - 5 * H - M, None));
    assert!(!lapsed.blocked);
    let any_window = one_refused(None, rejection(Limit::Unknown, NOW - M, None));
    assert_eq!(any_window.blocked_by, [Limit::Unknown]);
    assert!(!any_window.available_known);
    let answered = one_refused(
        Some(read_ago((20.0, 2 * H), (20.0, 3 * D), 0)),
        rejection(Limit::Unknown, NOW - M, None),
    );
    assert!(!answered.blocked);
}

#[test]
fn per_account_thresholds_override_the_policy() {
    let own = Account {
        five_hour: Some(50.0),
        ..account("a")
    };
    let s = states(vec![on(own, reading((55.0, H), (10.0, D)))]).remove(0);
    assert!(s.blocked);
    assert_eq!(s.windows.five_hour.threshold, 50.0);
}

/// A model's own weekly limit is a limit beside the week every model draws on. The fuller of the two
/// is the weekly window the policy works from, and each is kept by itself to be shown: here the week
/// has room, the model's own limit is used up, and that is what blocks the account.
#[test]
fn a_models_own_weekly_limit_is_kept_apart_from_the_week_it_is_part_of() {
    let mut with_own = reading((10.0, H), (69.0, 3 * D));
    with_own.limits_at = Some(iso(NOW - M));
    with_own.limits = serde_json::from_value(json!([
        { "kind": "weekly_all", "group": "weekly", "label": null, "used": 69, "resetsAt": iso(NOW + 3 * D) },
        { "kind": "weekly_scoped", "group": "weekly", "label": "Fable", "used": 100, "resetsAt": iso(NOW + 3 * D) },
    ]))
    .unwrap();
    let state = one(with_own.clone());
    let week = &state.windows.seven_day;
    assert_eq!((week.used, week.label.as_deref()), (Some(100.0), Some("Fable")));
    assert!(state.blocked && week.blocked);
    assert_eq!(
        (state.weekly_all.used, state.weekly_all.label.as_deref()),
        (Some(69.0), None)
    );
    assert!(!state.weekly_all.blocked);
    let own: Vec<_> = state
        .weekly_own
        .iter()
        .map(|w| (w.label.as_deref(), w.used, w.blocked))
        .collect();
    assert_eq!(own, [(Some("Fable"), Some(100.0), true)]);
    // The other way around, the week is the one in force, and the model's own limit is still listed.
    with_own.limits = serde_json::from_value(json!([
        { "kind": "weekly_scoped", "group": "weekly", "label": "Fable", "used": 4, "resetsAt": iso(NOW + 3 * D) },
    ]))
    .unwrap();
    let state = one(with_own);
    assert_eq!(
        (
            state.windows.seven_day.used,
            state.windows.seven_day.label.as_deref()
        ),
        (Some(69.0), None)
    );
    assert_eq!(state.weekly_all, state.windows.seven_day);
    let own: Vec<_> = state
        .weekly_own
        .iter()
        .map(|w| (w.label.as_deref(), w.used, w.blocked))
        .collect();
    assert_eq!(own, [(Some("Fable"), Some(4.0), false)]);
    // With no poll there is the week alone.
    let plain = one(reading((10.0, H), (69.0, 3 * D)));
    assert!(plain.weekly_own.is_empty());
    assert_eq!(plain.weekly_all, plain.windows.seven_day);
}

/// A reading with the week at `week` percent and Fable's own weekly limit at `fable`, both polled
/// a minute ago and resetting in three days; the poll lists a session meter when `session` is set.
fn with_fable(week: f64, fable: f64, session: bool) -> Reading {
    let mut reading = reading((10.0, H), (week, 3 * D));
    reading.limits_at = Some(iso(NOW - M));
    let mut limits = vec![
        json!({ "kind": "weekly_all", "group": "weekly", "label": null, "used": week, "resetsAt": iso(NOW + 3 * D) }),
        json!({ "kind": "weekly_scoped", "group": "weekly", "label": "Fable", "used": fable, "resetsAt": iso(NOW + 3 * D) }),
    ];
    if session {
        limits.push(json!({ "kind": "session", "group": "session", "label": null, "used": 10, "resetsAt": iso(NOW + H) }));
    }
    reading.limits = serde_json::from_value(json!(limits)).unwrap();
    reading
}

/// A model's own weekly limit blocks the chats that run that model, and no other: with Fable's
/// used up and the week at 69%, the account is said to be limited for Fable, a chat on Fable goes
/// elsewhere, and a chat on another model is placed here as if nothing were wrong.
#[test]
fn a_models_own_limit_blocks_the_chats_on_that_model_only() {
    let policy = Policy::default();
    let list = states(vec![
        on(marked("full", Mark::Preferred), with_fable(69.0, 100.0, true)),
        on(account("spare"), with_fable(20.0, 10.0, true)),
    ]);
    // With no model known, the strictest limit holds: blocked, but only by Fable's own.
    let full = &list[0];
    assert!(full.blocked);
    assert_eq!(full.limited_models, ["Fable"]);
    assert!(list[1].limited_models.is_empty());
    assert_eq!(picked(&list, &policy).0.as_deref(), Some("spare"));
    // A chat on Fable meets the same.
    let fable = states_for(&list, Some("claude-fable-5-2"), NOW);
    assert!(fable[0].blocked);
    assert_eq!(picked(&fable, &policy).0.as_deref(), Some("spare"));
    // A chat on another model does not: the week has room, and the preferred account takes it.
    let opus = states_for(&list, Some("claude-opus-5-5"), NOW);
    assert!(!opus[0].blocked && opus[0].blocked_by.is_empty());
    assert_eq!(opus[0].windows.seven_day.used, Some(69.0));
    assert_eq!(opus[0].windows.seven_day.label, None);
    assert!(close(opus[0].weekly_headroom, 95.0 - 69.0));
    assert_eq!(picked(&opus, &policy).0.as_deref(), Some("full"));
    // At the end of a turn on that account, a chat on another model stays; one on Fable, which
    // Anthropic just refused, moves.
    let end = |states: &[AccountState]| {
        decide_turn_end(&TurnEnd {
            now: NOW,
            policy: &policy,
            states,
            current: "full",
            event: "stop-failure",
            failure_error: Some("rate_limit"),
            hop_request: None,
            session: SessionFacts::default(),
        })
    };
    assert!(matches!(end(&fable), Decision::Hop { ref to, reason: "refusal", .. } if to == "spare"));
    // The week itself used up blocks every model.
    let over = states(vec![on(account("a"), with_fable(96.0, 100.0, true))]);
    assert!(over[0].blocked && over[0].limited_models.is_empty());
    assert!(states_for(&over, Some("claude-opus-5-5"), NOW)[0].blocked);
}

/// A refusal that names a model ("You've hit your Fable limit") is that model's limit, whatever
/// the readings say: it blocks the chats on that model and leaves the week open to the others. One
/// that names the week blocks them all.
#[test]
fn a_refusal_that_names_a_model_is_that_models_alone() {
    let refusal = |words: &str| {
        let mut out = rejection(Limit::SevenDay, NOW - M, Some(NOW + 2 * D));
        out.seven_day.as_mut().unwrap().message = Some(words.to_owned());
        out
    };
    // No poll has listed Fable's limit: the refusal is all there is to go by.
    let named = one_refused(
        Some(reading((10.0, H), (40.0, 3 * D))),
        refusal("You've hit your Fable limit \u{b7} resets Nov 22, 7am"),
    );
    assert!(named.blocked);
    assert_eq!(named.limited_models, ["Fable"]);
    assert_eq!(named.available_at, Some(NOW + 2 * D));
    assert!(!named.weekly_all.blocked);
    assert!(!named.for_model(Some("claude-opus-5-5"), NOW).blocked);
    assert!(named.for_model(Some("fable"), NOW).blocked);
    // With its meter polled, the refusal stays with that meter.
    let polled = one_refused(
        Some(with_fable(40.0, 100.0, true)),
        refusal("You've hit your Fable limit"),
    );
    assert_eq!(polled.limited_models, ["Fable"]);
    assert_eq!(polled.weekly_own.len(), 1);
    assert!(polled.weekly_own[0].rejected && !polled.weekly_all.rejected);
    // The week's own refusal is everyone's.
    let week = one_refused(
        Some(reading((10.0, H), (40.0, 3 * D))),
        refusal("You've hit your weekly limit \u{b7} resets Nov 22, 7am"),
    );
    assert!(week.blocked && week.limited_models.is_empty());
    assert!(week.for_model(Some("claude-opus-5-5"), NOW).blocked);
}

/// A plan whose poll lists no session meter has no 5-hour limit: nothing of it is measured or
/// blocks, however full the last reading of it was. Before any poll, the reading stands.
#[test]
fn a_plan_with_no_five_hour_limit_is_not_held_back_by_one() {
    let mut old = with_fable(20.0, 10.0, false);
    old.five_hour = reading((95.0, H), (20.0, 3 * D)).five_hour;
    old.five_hour.as_mut().unwrap().read_at = Some(iso(NOW - 2 * M));
    let none = one(old.clone());
    let five = &none.windows.five_hour;
    assert!(five.absent && !five.blocked && five.used.is_none());
    assert!(!none.blocked);
    assert!(close(none.five_hour_headroom, five.threshold));
    // A session that reports the window after the poll: it is there after all.
    old.five_hour.as_mut().unwrap().read_at = Some(iso(NOW));
    let back = one(old);
    assert!(!back.windows.five_hour.absent && back.blocked);
    // A poll that lists it, and no poll at all.
    assert!(!one(with_fable(20.0, 10.0, true)).windows.five_hour.absent);
    assert!(!one(reading((95.0, H), (20.0, 3 * D))).windows.five_hour.absent);
}

#[test]
fn weekly_urgency_is_weekly_headroom_per_hour_left_before_the_weekly_reset() {
    let s = one(reading((10.0, H), (35.0, 24 * H)));
    assert!(close(s.urgency, 60.0 / 24.0));
    let unmeasured = states(vec![bare(account("a"))]).remove(0);
    assert!(close(unmeasured.urgency, 95.0 / 168.0));
}

#[test]
fn start_pick_an_open_preferred_account_with_the_five_hour_reserve_comes_first() {
    let list = states(vec![
        on(account("a"), reading((10.0, H), (80.0, 10 * H))),
        on(marked("b", Mark::Preferred), reading((50.0, H), (10.0, 6 * D))),
    ]);
    assert_eq!(
        picked(&list, &Policy::default()),
        (Some("b".to_owned()), "preferred")
    );
}

#[test]
fn start_pick_a_preferred_account_below_the_reserve_gives_way_to_the_most_urgent_normal_account() {
    let list = states(vec![
        on(marked("pref", Mark::Preferred), reading((75.0, H), (10.0, 6 * D))), // 15 < 20
        on(account("slow"), reading((10.0, H), (50.0, 5 * D))),                 // 45/120 = 0.375
        on(account("soon"), reading((10.0, H), (60.0, 24 * H))),                // 35/24 = 1.46
        bare(account("fresh")),                                                 // 95/168 = 0.57
    ]);
    assert_eq!(
        picked(&list, &Policy::default()),
        (Some("soon".to_owned()), "urgency")
    );
}

#[test]
fn start_pick_equal_urgency_goes_by_order() {
    let list = states(vec![bare(account("x")), bare(account("y"))]);
    assert_eq!(picked(&list, &Policy::default()).0.as_deref(), Some("x"));
}

#[test]
fn start_pick_below_the_reserve_everywhere_the_most_five_hour_headroom_and_last_resort_only_when_nothing_else_is_open()
 {
    let list = states(vec![
        on(account("a"), reading((85.0, H), (10.0, D))), // headroom 5
        on(account("b"), reading((80.0, H), (10.0, D))), // headroom 10
        bare(marked("lr", Mark::LastResort)),
    ]);
    assert_eq!(
        picked(&list, &Policy::default()),
        (Some("b".to_owned()), "low-reserve")
    );
    let blocked = states(vec![
        on(account("a"), reading((95.0, H), (10.0, D))),
        bare(marked("lr", Mark::LastResort)),
    ]);
    assert_eq!(
        picked(&blocked, &Policy::default()),
        (Some("lr".to_owned()), "last-resort")
    );
}

#[test]
fn start_pick_in_order_mode_first_unblocked_in_order_marks_other_than_last_resort_ignored() {
    let p = Policy {
        mode: Mode::Order,
        ..Policy::default()
    };
    let list = states_under(
        vec![
            bare(marked("lr", Mark::LastResort)),
            on(account("a"), reading((95.0, H), (10.0, D))),
            on(account("b"), reading((85.0, H), (10.0, D))),
            bare(marked("pref", Mark::Preferred)),
        ],
        &p,
    );
    assert_eq!(picked(&list, &p), (Some("b".to_owned()), "order"));
    let all_but_lr = states_under(
        vec![
            on(account("a"), reading((95.0, H), (10.0, D))),
            bare(marked("lr", Mark::LastResort)),
        ],
        &p,
    );
    assert_eq!(picked(&all_but_lr, &p), (Some("lr".to_owned()), "last-resort"));
}

#[test]
fn all_blocked_the_earliest_available_at_wins_last_resort_included_ties_by_order_unknown_last() {
    let policy = Policy::default();
    let list = states(vec![
        on(account("a"), reading((95.0, 2 * H), (10.0, D))),
        on(account("b"), five_unknown(95.0, (10.0, D))),
        on(marked("lr", Mark::LastResort), reading((95.0, 30 * M), (10.0, D))),
        on(account("c"), reading((10.0, H), (99.0, 3 * D))),
    ]);
    let p = pick_start(&list, &policy, &[]);
    assert_eq!(p.account, None);
    assert_eq!(p.reason, "all-blocked");
    assert!(p.all_blocked);
    assert_eq!(p.earliest.as_deref(), Some("lr"));
    assert_eq!(p.available_at, Some(NOW + 30 * M));
    assert!(p.available_known);
    assert_eq!(p.state.map(|s| s.id.as_str()), Some("lr"));
    let tie = states(vec![
        on(account("x"), reading((95.0, H), (10.0, D))),
        on(account("y"), reading((95.0, H), (10.0, D))),
    ]);
    assert_eq!(earliest(&tie).map(|s| s.name.as_str()), Some("x"));
    let unknown = states(vec![
        on(account("u1"), five_unknown(95.0, (10.0, D))),
        on(account("u2"), five_unknown(95.0, (10.0, D))),
    ]);
    let pu = pick_start(&unknown, &policy, &[]);
    assert_eq!(pu.earliest.as_deref(), Some("u1"));
    assert!(!pu.available_known);
    assert_eq!(pu.available_at, None);
    assert!(earliest(&[]).is_none());
}

#[test]
fn start_pick_leaves_out_the_excluded_accounts_but_the_earliest_is_over_every_account() {
    let policy = Policy::default();
    let list = states(vec![bare(account("cur")), bare(account("other"))]);
    assert_eq!(
        pick_start(&list, &policy, &["cur"]).account.as_deref(),
        Some("other")
    );
    let alone = states(vec![
        bare(account("cur")),
        on(account("other"), reading((95.0, H), (10.0, D))),
    ]);
    let p = pick_start(&alone, &policy, &["cur"]);
    assert_eq!(p.account, None);
    assert_eq!(
        p.earliest.as_deref(),
        Some("cur"),
        "the excluded account is still the first back"
    );
    assert_eq!(p.available_at, Some(NOW));
}

#[test]
fn background_spread_effective_headroom_minus_running_cost_weighted_by_weekly_urgency() {
    let policy = Policy::default();
    let none = BTreeMap::new();
    let list = states(vec![
        on(account("a"), reading((20.0, H), (40.0, 5 * D))), // room 55, urgency 55/120
        on(account("b"), reading((30.0, H), (45.0, 24 * H))), // room 50, urgency 50/24 (top)
        bare(marked("lr", Mark::LastResort)),
    ]);
    // b: 50 x 1 = 50; a: 55 x (0.5 + 0.5 x 0.22) = 33.6
    let spread = pick_spread(&list, &policy, &none);
    assert_eq!(
        (spread.account.as_deref(), spread.reason, spread.queue),
        (Some("b"), "spread", false)
    );
    assert!(close(spread.score.unwrap(), 50.0));
    // Agents already on b cost 30: b 20 x 1 = 20 < a 33.6
    let running = BTreeMap::from([("b".to_owned(), 30.0)]);
    assert_eq!(
        pick_spread(&list, &policy, &running).account.as_deref(),
        Some("a")
    );
    let full = states(vec![
        on(account("a"), reading((95.0, H), (10.0, D))),
        bare(marked("lr", Mark::LastResort)),
    ]);
    let last = pick_spread(&full, &policy, &none);
    assert_eq!(
        (last.account.as_deref(), last.reason),
        (Some("lr"), "last-resort")
    );
    let blocked = states(vec![
        on(account("a"), reading((95.0, 2 * H), (10.0, D))),
        on(account("b"), reading((95.0, H), (10.0, D))),
    ]);
    let q = pick_spread(&blocked, &policy, &none);
    assert_eq!(
        (q.account.as_deref(), q.queue, q.at),
        (Some("b"), true, Some(NOW + H))
    );
    assert_eq!((q.reason, q.available_known), ("all-blocked", true));
    let preferred = states(vec![
        on(account("a"), reading((0.0, H), (0.0, 2 * H))),
        on(marked("p", Mark::Preferred), reading((60.0, H), (60.0, 6 * D))), // room 30 >= 20
    ]);
    assert_eq!(
        pick_spread(&preferred, &policy, &none).account.as_deref(),
        Some("p")
    );
}

/// A session's reading at a turn end `ago` before now: its 5-hour use and that window's reset.
fn sample(ago: Ms, account: &str, five: f64, resets: &str) -> TurnSample {
    TurnSample::from_value(&json!({
        "at": iso(NOW - ago),
        "account": account,
        "five_hour": { "used": five, "resetsAt": resets },
        "seven_day": { "used": 10.0 + five / 10.0, "resetsAt": "W" },
    }))
}

#[test]
fn turn_cost_moving_average_of_per_turn_deltas_skipping_resets_and_other_accounts() {
    let policy = Policy::default();
    let samples = vec![
        sample(50 * M, "a", 10.0, "R1"),
        sample(40 * M, "a", 14.0, "R1"),
        sample(30 * M, "a", 20.0, "R1"),
        sample(25 * M, "a", 5.0, "R2"),
        sample(20 * M, "a", 9.0, "R2"),
        sample(10 * M, "b", 50.0, "R1"),
        sample(5 * M, "b", 52.0, "R1"),
    ];
    let cost = turn_cost(&samples, &Others::NONE, &policy);
    // a: 4, 6, (reset), 4; then b: 2 -> last five: 4, 6, 4, 2
    assert_eq!(cost.five_hour, Some((4.0 + 6.0 + 4.0 + 2.0) / 4.0));
    assert_eq!(
        turn_cost(&[sample(0, "a", 10.0, "R1")], &Others::NONE, &policy).five_hour,
        None
    );
    assert_eq!(
        turn_cost(&[], &Others::NONE, &policy),
        PerWindow {
            five_hour: None,
            seven_day: None
        }
    );
    let last_two = Policy {
        cost_samples: 2.0,
        ..Policy::default()
    };
    assert_eq!(turn_cost(&samples, &Others::NONE, &last_two).five_hour, Some(3.0));
}

#[test]
fn turn_cost_and_burn_rate_leave_out_what_other_sessions_added_to_the_account_meanwhile() {
    let policy = Policy::default();
    // The account's 5-hour meter went from 10 to 20 over two turns of this session.
    let samples = vec![
        sample(50 * M, "a", 10.0, "R1"),
        sample(40 * M, "a", 14.0, "R1"),
        sample(30 * M, "a", 20.0, "R1"),
    ];
    let rise = |ago: Ms, by: f64, session: Option<&str>, account: &str| Rise {
        at: NOW - ago,
        account: account.to_owned(),
        window: Window::FiveHour,
        by,
        session: session.map(str::to_owned),
    };
    let rises = vec![
        // Another session added 1 during the first turn and 4 during the second.
        rise(45 * M, 1.0, Some("other"), "a"),
        rise(35 * M, 4.0, Some("other"), "a"),
        // What this session reported itself, what a poll reported, and what happened on another
        // account or outside the turns are not taken off.
        rise(44 * M, 2.0, Some("mine"), "a"),
        rise(43 * M, 1.0, None, "a"),
        rise(42 * M, 9.0, Some("other"), "b"),
        rise(55 * M, 9.0, Some("other"), "a"),
    ];
    // Oldest first, as the history gives them.
    let mut rises = rises;
    rises.sort_by_key(|rise| rise.at);
    let others = Others {
        rises: &rises,
        session: "mine",
    };
    // 4 - 1 and 6 - 4, where the meter alone says 4 and 6.
    assert_eq!(
        turn_cost(&samples, &others, &policy).five_hour,
        Some((3.0 + 2.0) / 2.0)
    );
    assert_eq!(turn_cost(&samples, &Others::NONE, &policy).five_hour, Some(5.0));
    // 10 in twenty minutes is 30 an hour; 5 of them were this session's: 15 an hour.
    assert_eq!(burn_rate(&samples, &others, &policy, NOW).five_hour, Some(15.0));
    // More than the meter rose is no negative cost.
    let all = [rise(45 * M, 30.0, Some("other"), "a")];
    let more = Others {
        rises: &all,
        session: "mine",
    };
    assert_eq!(turn_cost(&samples[..2], &more, &policy).five_hour, Some(0.0));
}

/// A session's reading `ago` before now on account "a", its 5-hour window resetting `jitter` after
/// `resets`.
fn jittered(ago: Ms, used: f64, resets: Ms, jitter: Ms) -> Value {
    json!({
        "at": iso(NOW - ago),
        "account": "a",
        "five_hour": { "used": used, "resetsAt": iso(resets + jitter) },
        "seven_day": { "used": 10, "resetsAt": iso(NOW + 3 * D) },
    })
}

#[test]
fn turn_cost_and_burn_rate_a_reset_time_moving_by_seconds_between_readings_is_the_same_window() {
    // The status line's resets_at can differ by a second from one reading to the next; only a real
    // reset (hours later) starts a new period.
    let policy = Policy::default();
    let resets = NOW + 3 * H;
    let samples: Vec<TurnSample> = [
        jittered(30 * M, 50.0, resets, 0),
        jittered(20 * M, 62.0, resets, 1000),
        jittered(10 * M, 76.0, resets, 2000),
    ]
    .iter()
    .map(TurnSample::from_value)
    .collect();
    assert_eq!(turn_cost(&samples, &Others::NONE, &policy).five_hour, Some(13.0));
    assert_eq!(
        burn_rate(&samples, &Others::NONE, &policy, NOW).five_hour,
        Some(26.0 / (20.0 / 60.0))
    );
    let reset: Vec<TurnSample> = [
        jittered(30 * M, 90.0, resets, 0),
        jittered(20 * M, 5.0, resets, 5 * H),
    ]
    .iter()
    .map(TurnSample::from_value)
    .collect();
    assert_eq!(turn_cost(&reset, &Others::NONE, &policy).five_hour, None);
}

#[test]
fn burn_rate_and_hours_to_block() {
    let policy = Policy::default();
    let samples: Vec<TurnSample> = [
        json!({
            "at": iso(NOW - 30 * M), "account": "a",
            "five_hour": { "used": 10, "resetsAt": "R" }, "seven_day": { "used": 50, "resetsAt": "W" },
        }),
        json!({
            "at": iso(NOW), "account": "a",
            "five_hour": { "used": 25, "resetsAt": "R" }, "seven_day": { "used": 51, "resetsAt": "W" },
        }),
    ]
    .iter()
    .map(TurnSample::from_value)
    .collect();
    let burn = burn_rate(&samples, &Others::NONE, &policy, NOW);
    assert_eq!(burn.five_hour, Some(30.0));
    assert_eq!(burn.seven_day, Some(2.0));
    let s = one(reading((70.0, 3 * H), (51.0, 3 * D)));
    // 20 points of 5-hour headroom at 30 %/h: 0.67 h, before the 3 h reset
    assert!(close(hours_to_block(&s, Some(&burn), NOW), 20.0 / 30.0));
    let resets_first = one(reading((70.0, 20 * M), (51.0, 3 * D)));
    // 0.67 h > 0.33 h to the reset: the 5-hour window does not block; weekly 44/2 = 22 h
    assert_eq!(hours_to_block(&resets_first, Some(&burn), NOW), 22.0);
    // Nothing burns: never.
    assert_eq!(hours_to_block(&s, None, NOW), f64::INFINITY);
    assert_eq!(
        hours_to_block(
            &s,
            Some(&PerWindow {
                five_hour: None,
                seven_day: Some(0.0)
            }),
            NOW
        ),
        f64::INFINITY
    );
    // Readings older than the burn window say nothing.
    assert_eq!(
        burn_rate(&samples, &Others::NONE, &policy, NOW + 2 * H),
        PerWindow {
            five_hour: None,
            seven_day: None
        }
    );
}

/// The states of the current account, "cur", and one other, "other".
fn two_accounts(cur: Option<Reading>, other: Option<Reading>) -> Vec<AccountState> {
    let case = |name: &str, reading: Option<Reading>| Case {
        account: account(name),
        reading,
        rejections: Rejections::default(),
    };
    states(vec![case("cur", cur), case("other", other)])
}

/// What the session on "cur" does when its turn was refused for a rate limit.
fn refuse(states: &[AccountState], session: SessionFacts) -> Decision {
    decide_turn_end(&TurnEnd {
        now: NOW,
        policy: &Policy::default(),
        states,
        current: "cur",
        event: "stop-failure",
        failure_error: Some("rate_limit"),
        hop_request: None,
        session,
    })
}

/// What the session on "cur" does when its turn ended, under a policy.
fn stop_under(states: &[AccountState], policy: &Policy, session: SessionFacts) -> Decision {
    decide_turn_end(&TurnEnd {
        now: NOW,
        policy,
        states,
        current: "cur",
        event: "stop",
        failure_error: None,
        hop_request: None,
        session,
    })
}

/// What the session on "cur" does when its turn ended, under the default policy.
fn stop(states: &[AccountState], session: SessionFacts) -> Decision {
    stop_under(states, &Policy::default(), session)
}

/// A session known only by its cost per turn.
fn costing(five_hour: f64, seven_day: f64) -> SessionFacts {
    SessionFacts {
        cost: both(five_hour, seven_day),
        ..SessionFacts::default()
    }
}

#[test]
fn refusal_a_five_hour_block_resetting_within_twenty_minutes_waits_in_place() {
    let s = two_accounts(Some(reading((100.0, 10 * M), (20.0, D))), None);
    assert_eq!(
        refuse(&s, SessionFacts::default()),
        Decision::WaitInPlace {
            reason: "reset-soon",
            at: Some(NOW + 10 * M)
        }
    );
}

#[test]
fn refusal_a_later_five_hour_reset_or_a_weekly_block_hops_to_the_best_open_account() {
    let later = two_accounts(Some(reading((100.0, 2 * H), (20.0, D))), None);
    assert_eq!(
        refuse(&later, SessionFacts::default()),
        hop("other", "refusal", Vec::new())
    );
    let weekly = two_accounts(Some(reading((10.0, 10 * M), (100.0, D))), None);
    assert_eq!(refuse(&weekly, SessionFacts::default()).kind(), "hop");
}

#[test]
fn refusal_with_every_account_blocked_wait_in_place_when_the_current_one_is_first_back_on_a_five_hour_block()
{
    let s = two_accounts(
        Some(reading((100.0, H), (20.0, D))),
        Some(reading((100.0, 2 * H), (20.0, D))),
    );
    assert_eq!(
        refuse(&s, SessionFacts::default()),
        Decision::WaitInPlace {
            reason: "all-blocked-current-first",
            at: Some(NOW + H)
        }
    );
}

#[test]
fn refusal_with_every_account_blocked_close_and_wait_for_another_account_that_is_first_back() {
    let s = two_accounts(
        Some(reading((100.0, 2 * H), (20.0, D))),
        Some(reading((100.0, 42 * M), (20.0, D))),
    );
    assert_eq!(
        refuse(&s, SessionFacts::default()),
        Decision::Wait {
            to: "other".to_owned(),
            at: NOW + 42 * M,
            reason: "all-blocked-wait"
        }
    );
}

#[test]
fn refusal_with_every_account_blocked_a_weekly_block_on_the_current_account_is_waited_out_with_claude_closed()
{
    let s = two_accounts(
        Some(reading((10.0, H), (100.0, 10 * H))),
        Some(reading((10.0, H), (100.0, 20 * H))),
    );
    assert_eq!(
        refuse(&s, SessionFacts::default()),
        Decision::Wait {
            to: "cur".to_owned(),
            at: NOW + 10 * H,
            reason: "all-blocked-wait"
        }
    );
    let unknown = two_accounts(
        Some(five_unknown(100.0, (10.0, D))),
        Some(five_unknown(100.0, (10.0, D))),
    );
    assert_eq!(
        refuse(&unknown, SessionFacts::default()),
        Decision::WaitInPlace {
            reason: "all-blocked-unknown",
            at: None
        }
    );
    assert_eq!(
        refuse(&[], SessionFacts::default()),
        Decision::WaitInPlace {
            reason: "no-accounts",
            at: None
        }
    );
}

#[test]
fn refusal_a_stay_keeps_the_session_in_place_and_repeated_refusals_right_after_resuming_back_off() {
    let s = two_accounts(Some(reading((100.0, 2 * H), (20.0, D))), None);
    let stayed = SessionFacts {
        stay_until: Some(NOW + H),
        ..SessionFacts::default()
    };
    assert_eq!(refuse(&s, stayed).reason(), "stayed");
    let backoff = refuse(
        &s,
        SessionFacts {
            reblocks: 3.0,
            ..SessionFacts::default()
        },
    );
    assert_eq!(
        backoff,
        Decision::Wait {
            to: "other".to_owned(),
            at: NOW + 15 * M,
            reason: "reblocked"
        }
    );
    assert_eq!(backoff.kind(), "wait");
    // An error that is not a limit moves nothing.
    let other_error = decide_turn_end(&TurnEnd {
        now: NOW,
        policy: &Policy::default(),
        states: &s,
        current: "cur",
        event: "stop-failure",
        failure_error: Some("server_error"),
        hop_request: None,
        session: SessionFacts::default(),
    });
    assert_eq!(other_error.reason(), "other-error");
    assert_eq!(other_error.kind(), "none");
}

#[test]
fn planned_hop_headroom_below_one_and_a_half_times_the_next_turn_cost_moves_at_this_turn_end() {
    let s = two_accounts(
        Some(reading((80.0, 2 * H), (20.0, 3 * D))),
        Some(reading((10.0, H), (10.0, 3 * D))),
    );
    // 5-hour headroom 10 < 1.5 x 8 = 12
    assert_eq!(
        stop(&s, costing(8.0, 1.0)),
        hop("other", "planned", vec![Window::FiveHour])
    );
    // 10 >= 1.5 x 6 = 9: stays
    assert_eq!(stop(&s, costing(6.0, 1.0)).reason(), "stay");
}

#[test]
fn planned_hop_never_while_subagents_or_a_workflow_run_nor_within_min_stay_nor_in_order_mode() {
    let s = two_accounts(Some(reading((80.0, 2 * H), (20.0, 3 * D))), None);
    let with_task = |kind: &str| SessionFacts {
        background_tasks: json!([{ "type": kind, "status": "running" }]),
        ..costing(8.0, 1.0)
    };
    assert_eq!(stop(&s, with_task("subagent")).reason(), "busy");
    assert_eq!(stop(&s, with_task("workflow")).reason(), "busy");
    assert_eq!(stop(&s, with_task("shell")).kind(), "hop");
    let arrived = |ago: Ms| SessionFacts {
        arrived_at: Some(NOW - ago),
        ..costing(8.0, 1.0)
    };
    assert_eq!(stop(&s, arrived(10 * M)).reason(), "min-stay");
    assert_eq!(stop(&s, arrived(31 * M)).kind(), "hop");
    let vetoed = SessionFacts {
        veto_until: Some(NOW + M),
        ..costing(8.0, 1.0)
    };
    assert_eq!(stop(&s, vetoed).reason(), "vetoed");
    let order = Policy {
        mode: Mode::Order,
        ..Policy::default()
    };
    assert_eq!(stop_under(&s, &order, costing(8.0, 1.0)).reason(), "order-mode");
}

#[test]
fn an_account_over_its_threshold_is_left_at_the_turn_end_while_another_is_open() {
    // 92% of the 5-hour window, over the 90 that is set: no turn was refused, no cost is known yet.
    let over = || Some(reading((92.0, 2 * H), (20.0, 3 * D)));
    let s = two_accounts(over(), Some(reading((10.0, H), (10.0, 3 * D))));
    let moved = hop("other", "threshold", vec![Window::FiveHour]);
    assert_eq!(stop(&s, SessionFacts::default()), moved);
    // The minimum stay holds back a forecast, not this; and it holds in order mode too.
    let arrived = SessionFacts {
        arrived_at: Some(NOW - 10 * M),
        ..SessionFacts::default()
    };
    assert_eq!(stop(&s, arrived), moved);
    let order = Policy {
        mode: Mode::Order,
        ..Policy::default()
    };
    assert_eq!(stop_under(&s, &order, SessionFacts::default()), moved);
    // What the operator chose and work in the background still hold it.
    let stayed = SessionFacts {
        stay_until: Some(NOW + H),
        ..SessionFacts::default()
    };
    assert_eq!(stop(&s, stayed).kind(), "none");
    let vetoed = SessionFacts {
        veto_until: Some(NOW + M),
        ..SessionFacts::default()
    };
    assert_eq!(stop(&s, vetoed).reason(), "vetoed");
    let working = SessionFacts {
        background_tasks: json!([{ "type": "subagent", "status": "running" }]),
        ..SessionFacts::default()
    };
    assert_eq!(stop(&s, working).reason(), "busy");
    // Nowhere open to go: it stays, and so does a run of hops that has only left accounts behind it
    // that are over theirs too.
    let nowhere = two_accounts(over(), over());
    assert_eq!(stop(&nowhere, SessionFacts::default()).kind(), "none");
    // Under its threshold nothing of this applies.
    let under = two_accounts(Some(reading((89.0, 2 * H), (20.0, 3 * D))), None);
    assert_eq!(stop(&under, SessionFacts::default()).reason(), "stay");
}

#[test]
fn planned_hop_no_target_with_room_for_the_next_turn_means_staying() {
    let s = two_accounts(
        Some(reading((80.0, 2 * H), (20.0, 3 * D))),
        Some(reading((82.0, H), (10.0, 3 * D))),
    );
    assert_eq!(
        stop(&s, costing(8.0, 1.0)),
        Decision::Stay {
            reason: "planned-no-target",
            windows: vec![Window::FiveHour]
        }
    );
}

#[test]
fn compaction_rebalance_when_the_account_would_block_within_an_hour_and_another_is_clearly_better() {
    let s = two_accounts(
        Some(reading((70.0, 3 * H), (30.0, 3 * D))),
        Some(reading((10.0, H), (10.0, 3 * D))),
    );
    let after = |compacted: bool, five_hour_burn: f64| SessionFacts {
        compacted,
        burn: Some(both(five_hour_burn, 1.0)),
        ..SessionFacts::default()
    };
    assert_eq!(
        stop(&s, after(true, 30.0)),
        hop("other", "compaction", Vec::new())
    );
    assert_eq!(stop(&s, after(false, 30.0)).reason(), "stay");
    assert_eq!(stop(&s, after(true, 10.0)).reason(), "stay"); // 2 h away
    let near = two_accounts(
        Some(reading((70.0, 3 * H), (30.0, 3 * D))),
        Some(reading((55.0, H), (10.0, 3 * D))),
    );
    assert_eq!(stop(&near, after(true, 30.0)).reason(), "stay"); // 35 < 20 + 20
    let off = Policy {
        compaction_rebalance: false,
        ..Policy::default()
    };
    assert_eq!(
        stop_under(&s, &off, after(true, 30.0)).reason(),
        "stay",
        "switched off"
    );
}

#[test]
fn manual_hop_to_the_named_account_to_the_best_other_one_or_not_at_all() {
    let policy = Policy::default();
    let s = two_accounts(None, None);
    let manual = |states: &[AccountState], event: &str, to: Option<&str>| {
        decide_turn_end(&TurnEnd {
            now: NOW,
            policy: &policy,
            states,
            current: "cur",
            event,
            failure_error: None,
            hop_request: Some(to),
            session: SessionFacts::default(),
        })
    };
    assert_eq!(
        manual(&s, "stop", Some("other")),
        hop("other", "manual", Vec::new())
    );
    assert_eq!(manual(&s, "stop", None), hop("other", "manual", Vec::new()));
    assert_eq!(manual(&s, "stop", Some("cur")).reason(), "manual-same-account");
    assert_eq!(
        manual(&s, "stop", Some("nobody")).reason(),
        "manual-unknown-account"
    );
    let blocked = two_accounts(None, Some(reading((99.0, H), (1.0, D))));
    assert_eq!(manual(&blocked, "idle", None).reason(), "manual-no-account");
    // An account that is named is where it goes, blocked or not.
    assert_eq!(
        manual(&blocked, "idle", Some("other")),
        hop("other", "manual", Vec::new())
    );
    // Neither a turn end nor a request: nothing.
    let idle = decide_turn_end(&TurnEnd {
        now: NOW,
        policy: &policy,
        states: &s,
        current: "cur",
        event: "idle",
        failure_error: None,
        hop_request: None,
        session: SessionFacts::default(),
    });
    assert_eq!(idle.reason(), "not-a-turn-end");
}

#[test]
fn waiting_resume_as_soon_as_any_account_opens_otherwise_follow_the_earliest() {
    let policy = Policy::default();
    let target = Some("other");
    let at = Some(NOW + 42 * M);
    let blocked = two_accounts(
        Some(reading((100.0, 2 * H), (20.0, D))),
        Some(reading((100.0, 42 * M), (20.0, D))),
    );
    assert_eq!(
        decide_wait(&blocked, &policy, target, at),
        WaitDecision::Wait {
            to: Some("other".to_owned()),
            at: Some(NOW + 42 * M)
        }
    );
    let earlier = two_accounts(
        Some(reading((100.0, 10 * M), (20.0, D))),
        Some(reading((100.0, 42 * M), (20.0, D))),
    );
    assert_eq!(
        decide_wait(&earlier, &policy, target, at),
        WaitDecision::Wait {
            to: Some("cur".to_owned()),
            at: Some(NOW + 10 * M)
        }
    );
    let open = two_accounts(
        Some(reading((100.0, 2 * H), (20.0, D))),
        Some(read_ago((5.0, 5 * H), (20.0, D), 0)),
    );
    assert_eq!(
        decide_wait(&open, &policy, target, at),
        WaitDecision::Resume {
            to: "other".to_owned(),
            reason: "urgency"
        }
    );
    // Nothing known of when anyone is back: the target it had.
    let unknown = two_accounts(
        Some(five_unknown(100.0, (10.0, D))),
        Some(five_unknown(100.0, (10.0, D))),
    );
    assert_eq!(
        decide_wait(&unknown, &policy, target, at),
        WaitDecision::Wait {
            to: Some("other".to_owned()),
            at
        }
    );
}

#[test]
fn account_state_is_pure_and_keeps_the_accounts_order_index() {
    let policy = Policy::default();
    let z = account("z");
    let s = account_state(&z, None, None, &policy, NOW, 4);
    assert_eq!(s.index, 4);
    assert!(!s.blocked);
    assert_eq!(s, account_state(&z, None, None, &policy, NOW, 4));
    assert_eq!((s.id.as_str(), s.name.as_str(), s.mark), ("z", "z", Mark::Normal));
    assert_eq!(
        (s.five_hour_headroom, s.weekly_headroom, s.effective_headroom),
        (90.0, 95.0, 90.0)
    );
    assert_eq!(
        (s.available_at, s.available_known, s.reading_at),
        (Some(NOW), true, None)
    );
}

/// The ids of accounts, as a run of hops keeps them.
fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

#[test]
fn a_run_of_hops_lasts_until_a_turn_ends_well_on_an_account_the_conversation_has_stayed_on() {
    let policy = Policy::default();
    let left = ids(&["a", "b"]);
    let run =
        |arrived: Option<Ms>, ended_well: bool| hop_run(&left, arrived, ended_well, &policy, NOW).to_vec();
    // Just arrived, or no turn ended well yet however long ago that was: the run goes on.
    assert_eq!(run(Some(NOW - M), true), left);
    assert_eq!(run(Some(NOW - 5 * H), false), left);
    assert_eq!(run(None, false), left);
    // Settled: a turn ended well, and the minimum stay (30 minutes) is over.
    assert_eq!(run(Some(NOW - 29 * M), true), left);
    assert!(run(Some(NOW - 30 * M), true).is_empty());
    assert!(run(None, true).is_empty());
    let none = Policy {
        min_stay_minutes: 0.0,
        ..Policy::default()
    };
    assert!(hop_run(&left, Some(NOW), true, &none, NOW).is_empty());
    assert_eq!(hop_run(&left, Some(NOW), false, &none, NOW), left);
}

#[test]
fn a_run_of_hops_does_not_go_back_to_an_account_it_left_while_another_is_open() {
    let policy = Policy::default();
    let three = states(vec![
        bare(account("cur")),
        bare(account("other")),
        bare(account("third")),
    ]);
    let to = |states: &[AccountState], left: &[&str]| pick_hop(states, &policy, "cur", &ids(left)).account;
    // No run: the best other account, as new work.
    assert_eq!(to(&three, &[]).as_deref(), Some("other"));
    // It came from there: on to the next one, not back.
    assert_eq!(to(&three, &["other"]).as_deref(), Some("third"));
    // Through all of them: back to the one it left longest ago.
    assert_eq!(to(&three, &["other", "third"]).as_deref(), Some("other"));
    assert_eq!(to(&three, &["third", "other"]).as_deref(), Some("third"));
    // A last-resort account it has not tried comes before one it just left.
    let spare = states(vec![
        bare(account("cur")),
        bare(account("other")),
        bare(marked("spare", Mark::LastResort)),
    ]);
    assert_eq!(to(&spare, &["other"]).as_deref(), Some("spare"));
    // The one it left longest ago is at its limit: the next one it left.
    let full = states(vec![
        bare(account("cur")),
        on(account("other"), reading((99.0, H), (1.0, D))),
        bare(account("third")),
    ]);
    assert_eq!(to(&full, &["other", "third"]).as_deref(), Some("third"));
    // Nothing else is open at all: no account, and the first one back as ever.
    let alone = states(vec![
        bare(account("cur")),
        on(account("other"), reading((99.0, H), (1.0, D))),
    ]);
    let pick = pick_hop(&alone, &policy, "cur", &ids(&["other"]));
    assert_eq!((pick.account, pick.earliest.as_deref()), (None, Some("cur")));
    // An account that is gone, or the current one, in the run changes nothing.
    assert_eq!(to(&three, &["gone", "cur"]).as_deref(), Some("other"));
}

#[test]
fn every_kind_of_hop_keeps_to_the_run() {
    let policy = Policy::default();
    let came_from_other = || SessionFacts {
        left: ids(&["other"]),
        ..SessionFacts::default()
    };
    let three = states(vec![
        bare(account("cur")),
        bare(account("other")),
        bare(account("third")),
    ]);
    // Asked for, with no account named: the next one. A named account is where it goes all the same.
    let manual = |to: Option<&str>| {
        decide_turn_end(&TurnEnd {
            now: NOW,
            policy: &policy,
            states: &three,
            current: "cur",
            event: "stop-failure",
            failure_error: Some("oauth_org_not_allowed"),
            hop_request: Some(to),
            session: came_from_other(),
        })
    };
    assert_eq!(manual(None), hop("third", "manual", Vec::new()));
    assert_eq!(manual(Some("other")), hop("other", "manual", Vec::new()));
    // Refused where it arrived: on, not back.
    assert_eq!(
        refuse(&three, came_from_other()),
        hop("third", "refusal", Vec::new())
    );
    // With two accounts there is nowhere else: back it goes.
    let two = two_accounts(None, None);
    assert_eq!(
        refuse(&two, came_from_other()),
        hop("other", "refusal", Vec::new())
    );
    // A planned move: to the account with room that it did not just leave.
    let short = states(vec![
        on(account("cur"), reading((80.0, 2 * H), (20.0, 3 * D))),
        bare(account("other")),
        bare(account("third")),
    ]);
    let planned = SessionFacts {
        left: ids(&["other"]),
        ..costing(8.0, 1.0)
    };
    assert_eq!(
        stop(&short, planned),
        hop("third", "planned", vec![Window::FiveHour])
    );
}
