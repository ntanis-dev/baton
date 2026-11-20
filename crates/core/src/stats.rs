//! Statistics from the append-only history: per account the usage over the last days (highest 5-hour
//! use per day, last weekly use per day), refusals and hops per day, sessions per day, and the weekly
//! headroom that was left when a weekly window reset (headroom = threshold - used; unused = 100 - used:
//! allowance that was lost), and the sessions of the period with their hops and per-turn cost
//! estimates.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Months, NaiveDate, Utc};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use serde_json::Value;

use crate::clock::{DAY, Ms, iso, local_date, ms_of, parse_ms};
use crate::config::{Config, Mark, Policy, name_of, thresholds};
use crate::fsutil::read_json_lines;
use crate::json::{Get, ser_num, ser_opt_num};
use crate::paths::Paths;
use crate::policy::turn_cost;
use crate::sessions::read_turn_samples;
use crate::usage::{history_file, same_period};
use crate::window::PerWindow;

/// The first moment of every month from the one holding `from` to `to`.
fn months_between(from: Ms, to: Ms) -> Vec<Ms> {
    let mut out = Vec::new();
    let Some(start) = DateTime::<Utc>::from_timestamp_millis(from) else {
        return out;
    };
    let Some(month) = NaiveDate::from_ymd_opt(start.year(), start.month(), 1) else {
        return out;
    };
    let mut next = Some(month);
    while let Some(first) = next {
        let at = first
            .and_time(chrono::NaiveTime::MIN)
            .and_utc()
            .timestamp_millis();
        if at > to {
            break;
        }
        out.push(at);
        next = first.checked_add_months(Months::new(1));
    }
    out
}

/// The history of `kind` between two moments, oldest first.
pub fn read_history(paths: &Paths, kind: &str, from: Ms, to: Ms) -> Vec<Value> {
    let mut out: Vec<(Ms, Value)> = months_between(from, to)
        .into_iter()
        .flat_map(|month| read_json_lines(&history_file(paths, kind, month)))
        .filter_map(|line| {
            let at = ms_of(line.at("at"))?;
            (at >= from && at <= to).then_some((at, line))
        })
        .collect();
    out.sort_by_key(|(at, _)| *at);
    out.into_iter().map(|(_, line)| line).collect()
}

/// A move of a conversation, for showing.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HopView {
    /// When.
    pub at: Option<String>,
    /// The conversation.
    pub session: Option<String>,
    /// The account it left, by its current name.
    pub from: String,
    /// The account it went to, by its current name.
    pub to: String,
    /// The id of the account it left.
    #[serde(rename = "fromId")]
    pub from_id: Option<String>,
    /// The id of the account it went to.
    #[serde(rename = "toId")]
    pub to_id: Option<String>,
    /// Why.
    pub reason: Option<String>,
}

/// One day of one account.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DayStats {
    /// The local date, `2026-11-15`.
    pub date: String,
    /// The highest 5-hour use read that day.
    #[serde(rename = "fiveHourMax", serialize_with = "ser_opt_num")]
    pub five_hour_max: Option<f64>,
    /// The last weekly use read that day.
    #[serde(rename = "weeklyLast", serialize_with = "ser_opt_num")]
    pub weekly_last: Option<f64>,
    /// Refusals that day.
    pub refusals: usize,
    /// Conversations that arrived that day.
    #[serde(rename = "hopsIn")]
    pub hops_in: usize,
    /// Conversations that left that day.
    #[serde(rename = "hopsOut")]
    pub hops_out: usize,
}

/// A weekly window that reset in the period, and what was left of it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Wasted {
    /// When it reset.
    #[serde(rename = "resetAt")]
    pub reset_at: String,
    /// The last use read before the reset.
    #[serde(rename = "lastUsed", serialize_with = "ser_num")]
    pub last_used: f64,
    /// threshold - used: what Baton could still have used.
    #[serde(rename = "headroomLeft", serialize_with = "ser_num")]
    pub headroom_left: f64,
    /// 100 - used: allowance that was lost.
    #[serde(serialize_with = "ser_num")]
    pub unused: f64,
}

/// One account over the period.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AccountStats {
    /// The account's id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its mark.
    pub mark: Mark,
    /// Each day of the period.
    pub daily: Vec<DayStats>,
    /// The weekly windows that reset in the period.
    pub wasted: Vec<Wasted>,
    /// Refusals over the period.
    pub refusals: usize,
    /// Conversations started on it.
    pub sessions: usize,
}

/// One conversation of the period.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionStats {
    /// The conversation's id.
    pub id: String,
    /// When it was first seen in the period.
    #[serde(rename = "firstSeen")]
    pub first_seen: Option<String>,
    /// The accounts it ran on, by their current names.
    pub accounts: Vec<String>,
    /// Its moves.
    pub hops: Vec<HopView>,
    /// Its refusals.
    pub refusals: usize,
    /// Its turns with a reading.
    pub turns: usize,
    /// Its estimated cost per turn in each window.
    #[serde(rename = "costPerTurn", serialize_with = "ser_cost")]
    pub cost_per_turn: PerWindow<Option<f64>>,
}

/// Serializes a cost per window with whole numbers written whole.
fn ser_cost<S: Serializer>(cost: &PerWindow<Option<f64>>, serializer: S) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(2))?;
    for (window, value) in cost.iter() {
        map.serialize_entry(window.as_str(), &crate::json::opt_num(*value))?;
    }
    map.end()
}

/// Serializes counts by name as an object, in their own order.
fn ser_counts<S: Serializer>(counts: &[(String, usize)], serializer: S) -> Result<S::Ok, S::Error> {
    let mut map = serializer.serialize_map(Some(counts.len()))?;
    for (name, count) in counts {
        map.serialize_entry(name, count)?;
    }
    map.end()
}

/// Counts per day of the period.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Totals {
    /// Moves per day.
    pub hops: BTreeMap<String, usize>,
    /// Refusals per day.
    pub refusals: BTreeMap<String, usize>,
    /// Conversations started per day.
    pub sessions: BTreeMap<String, usize>,
}

/// The statistics of a period.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stats {
    /// The start of the period.
    pub since: String,
    /// The end of the period.
    pub until: String,
    /// The local dates of the period, oldest first.
    pub days: Vec<String>,
    /// Counts per day.
    pub totals: Totals,
    /// Moves by reason, in the order the reasons first appeared.
    #[serde(rename = "hopReasons", serialize_with = "ser_counts")]
    pub hop_reasons: Vec<(String, usize)>,
    /// The last 20 moves.
    pub hops: Vec<HopView>,
    /// Each account.
    pub accounts: Vec<AccountStats>,
    /// Each conversation.
    pub sessions: Vec<SessionStats>,
}

/// The sources of a `session` event that start a conversation.
fn starts_a_session(event: &Value) -> bool {
    matches!(event.text("source"), Some("startup" | "clear"))
}

/// The statistics of the last `days` days up to `now`.
pub fn compute_stats(paths: &Paths, config: &Config, policy: &Policy, now: Ms, days: i64) -> Stats {
    let since = now - days * DAY;
    // The history keeps account ids; what it shows is their current names.
    let label = |id: Option<&str>| id.map(|id| name_of(config, id)).unwrap_or_default();
    let own = |value: Option<&str>| value.map(str::to_owned);
    let hop_view = |e: &Value| HopView {
        at: own(e.text("at")),
        session: own(e.text("session")),
        from: label(e.text("from")),
        to: label(e.text("to")),
        from_id: own(e.text("from")),
        to_id: own(e.text("to")),
        reason: own(e.text("reason")),
    };
    // Weekly windows that reset in the period started up to a week earlier.
    let readings = read_history(paths, "readings", since - 7 * DAY, now);
    let events = read_history(paths, "events", since, now);
    let day_of = |line: &Value| ms_of(line.at("at")).map(local_date).unwrap_or_default();
    let kind = |e: &Value, kind: &str| e.text("type") == Some(kind);
    let mut dates: Vec<String> = Vec::new();
    let mut t = now - (days - 1) * DAY;
    while t <= now + 1 {
        let date = local_date(t);
        if !dates.contains(&date) {
            dates.push(date);
        }
        t += DAY;
    }
    if !dates.contains(&local_date(now)) {
        dates.push(local_date(now));
    }
    let per_day = || {
        dates
            .iter()
            .map(|d| (d.clone(), 0usize))
            .collect::<BTreeMap<_, _>>()
    };
    let mut totals = Totals {
        hops: per_day(),
        refusals: per_day(),
        sessions: per_day(),
    };
    let mut hop_reasons: Vec<(String, usize)> = Vec::new();
    for e in &events {
        let date = day_of(e);
        if !totals.hops.contains_key(&date) {
            continue;
        }
        if kind(e, "hop") {
            *totals.hops.entry(date).or_default() += 1;
            let reason = e.text("reason").unwrap_or("undefined").to_owned();
            match hop_reasons.iter_mut().find(|(r, _)| *r == reason) {
                Some(entry) => entry.1 += 1,
                None => hop_reasons.push((reason, 1)),
            }
        } else if kind(e, "refusal") {
            *totals.refusals.entry(date).or_default() += 1;
        } else if kind(e, "session") && starts_a_session(e) {
            *totals.sessions.entry(date).or_default() += 1;
        }
    }
    let accounts = config
        .accounts
        .iter()
        .map(|account| {
            let limit = thresholds(account, policy);
            let id = Some(account.id.as_str());
            let mine: Vec<&Value> = readings.iter().filter(|r| r.text("account") == id).collect();
            let count = |date: &str, what: &str, field: &str| {
                events
                    .iter()
                    .filter(|e| kind(e, what) && e.text(field) == id && day_of(e) == date)
                    .count()
            };
            let daily: Vec<DayStats> = dates
                .iter()
                .map(|date| {
                    let day: Vec<&&Value> = mine.iter().filter(|r| &day_of(r) == date).collect();
                    let of = |window: &str| -> Vec<f64> {
                        day.iter()
                            .filter(|r| r.text("window") == Some(window))
                            .filter_map(|r| r.number("used"))
                            .collect()
                    };
                    DayStats {
                        date: date.clone(),
                        five_hour_max: of("five_hour").into_iter().reduce(f64::max),
                        weekly_last: of("seven_day").last().copied(),
                        refusals: count(date, "refusal", "account"),
                        hops_in: count(date, "hop", "to"),
                        hops_out: count(date, "hop", "from"),
                    }
                })
                .collect();
            // The last reading of each weekly window period.
            let mut windows: Vec<&Value> = Vec::new();
            for r in mine
                .iter()
                .filter(|r| r.text("window") == Some("seven_day") && r.word("resetsAt").is_some())
            {
                match windows
                    .iter()
                    .position(|w| same_period(w.text("resetsAt"), r.text("resetsAt")))
                {
                    Some(at) => windows[at] = r,
                    None => windows.push(r),
                }
            }
            let wasted = windows
                .iter()
                .filter_map(|r| {
                    let reset = r.text("resetsAt")?;
                    let at = parse_ms(reset)?;
                    let used = r.number("used")?;
                    (at <= now && at >= since).then(|| Wasted {
                        reset_at: reset.to_owned(),
                        last_used: used,
                        headroom_left: (limit.seven_day - used).max(0.0),
                        unused: (100.0 - used).max(0.0),
                    })
                })
                .collect();
            AccountStats {
                id: account.id.clone(),
                name: account.name.clone(),
                mark: account.mark,
                refusals: daily.iter().map(|d| d.refusals).sum(),
                sessions: events
                    .iter()
                    .filter(|e| kind(e, "session") && e.text("account") == id && starts_a_session(e))
                    .count(),
                daily,
                wasted,
            }
        })
        .collect();
    let mut ids: Vec<&str> = Vec::new();
    for e in &events {
        if let Some(session) = e.word("session")
            && matches!(e.text("type"), Some("session" | "hop" | "refusal"))
            && !ids.contains(&session)
        {
            ids.push(session);
        }
    }
    let sessions = ids
        .into_iter()
        .map(|id| {
            let mine: Vec<&Value> = events.iter().filter(|e| e.text("session") == Some(id)).collect();
            let samples = read_turn_samples(paths, id);
            let mut on: Vec<&str> = Vec::new();
            for e in &mine {
                for field in ["account", "from", "to"] {
                    if let Some(account) = e.word(field)
                        && !on.contains(&account)
                    {
                        on.push(account);
                    }
                }
            }
            SessionStats {
                id: id.to_owned(),
                first_seen: mine.first().and_then(|e| e.text("at")).map(str::to_owned),
                accounts: on.into_iter().map(|account| name_of(config, account)).collect(),
                hops: mine
                    .iter()
                    .filter(|e| kind(e, "hop"))
                    .map(|e| hop_view(e))
                    .collect(),
                refusals: mine.iter().filter(|e| kind(e, "refusal")).count(),
                turns: samples.len(),
                cost_per_turn: turn_cost(&samples, policy),
            }
        })
        .collect();
    let all_hops: Vec<&Value> = events.iter().filter(|e| kind(e, "hop")).collect();
    Stats {
        since: iso(since),
        until: iso(now),
        days: dates,
        totals,
        hop_reasons,
        hops: all_hops[all_hops.len().saturating_sub(20)..]
            .iter()
            .map(|e| hop_view(e))
            .collect(),
        accounts,
        sessions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{HOUR, local_ms};
    use crate::config::{effective_policy, normalize_config};
    use crate::fsutil::append_json_line;
    use crate::paths::baton_paths;
    use crate::sessions::append_turn_sample;
    use serde_json::json;

    #[test]
    fn months() {
        let nov = parse_ms("2026-11-15T10:00:00.000Z").unwrap();
        let jan = parse_ms("2027-01-02T00:00:00.000Z").unwrap();
        let firsts: Vec<String> = months_between(nov, jan).into_iter().map(iso).collect();
        assert_eq!(
            firsts,
            [
                "2026-11-01T00:00:00.000Z",
                "2026-12-01T00:00:00.000Z",
                "2027-01-01T00:00:00.000Z"
            ]
        );
        assert_eq!(months_between(nov, nov).len(), 1);
    }

    #[test]
    fn a_week_of_history() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let config = normalize_config(&json!({ "accounts": [
            { "id": "work", "name": "Work" }, { "id": "spare", "name": "Spare", "mark": "last-resort" }
        ] }));
        let policy = effective_policy(&config);
        let now = local_ms(2026, 3, 10, 12, 0).unwrap();
        let reading = |at: Ms, account: &str, window: &str, used: f64, resets: Ms| {
            append_json_line(
                &history_file(&paths, "readings", at),
                &json!({ "at": iso(at), "account": account, "window": window, "used": used, "resetsAt": iso(resets) }),
            )
            .unwrap();
        };
        let event = |at: Ms, fields: Value| crate::usage::record_event(&paths, fields, at);
        // Today: two 5-hour readings and the week so far.
        reading(now - 3 * HOUR, "work", "five_hour", 40.0, now + HOUR);
        reading(now - 2 * HOUR, "work", "five_hour", 70.5, now + HOUR);
        reading(now - 2 * HOUR, "work", "seven_day", 30.0, now + 3 * DAY);
        // A weekly window that reset two days ago with 60% used.
        reading(now - 4 * DAY, "work", "seven_day", 55.0, now - 2 * DAY);
        reading(now - 3 * DAY, "work", "seven_day", 60.0, now - 2 * DAY + 1000);
        event(
            now - 3 * HOUR,
            json!({ "type": "session", "session": "s1", "account": "work", "source": "startup" }),
        );
        event(
            now - 2 * HOUR,
            json!({ "type": "refusal", "session": "s1", "account": "work" }),
        );
        event(
            now - 2 * HOUR + 1000,
            json!({ "type": "hop", "session": "s1", "from": "work", "to": "spare", "reason": "refusal" }),
        );
        event(
            now - HOUR,
            json!({ "type": "session", "session": "s1", "account": "spare", "source": "resume" }),
        );
        append_turn_sample(&paths, "s1", &json!({ "at": iso(now - 3 * HOUR), "account": "work", "five_hour": { "used": 40, "resetsAt": iso(now + HOUR) } })).unwrap();
        append_turn_sample(&paths, "s1", &json!({ "at": iso(now - 2 * HOUR), "account": "work", "five_hour": { "used": 46, "resetsAt": iso(now + HOUR) } })).unwrap();

        let stats = compute_stats(&paths, &config, &policy, now, 7);
        assert_eq!(stats.days.len(), 7);
        assert_eq!(stats.days.last().unwrap(), "2026-03-10");
        assert_eq!(stats.totals.hops["2026-03-10"], 1);
        assert_eq!(stats.totals.refusals["2026-03-10"], 1);
        assert_eq!(
            stats.totals.sessions["2026-03-10"], 1,
            "a resume is not a new conversation"
        );
        assert_eq!(stats.hop_reasons, vec![("refusal".to_owned(), 1)]);
        assert_eq!(stats.hops.len(), 1);
        assert_eq!(
            (stats.hops[0].from.as_str(), stats.hops[0].to.as_str()),
            ("Work", "Spare")
        );
        let work = &stats.accounts[0];
        let today = work.daily.last().unwrap();
        assert_eq!((today.five_hour_max, today.weekly_last), (Some(70.5), Some(30.0)));
        assert_eq!((today.refusals, today.hops_in, today.hops_out), (1, 0, 1));
        assert_eq!(work.refusals, 1);
        assert_eq!(work.sessions, 1);
        assert_eq!(
            work.wasted,
            vec![Wasted {
                reset_at: iso(now - 2 * DAY + 1000),
                last_used: 60.0,
                headroom_left: 35.0,
                unused: 40.0
            }]
        );
        assert_eq!(stats.accounts[1].daily.last().unwrap().hops_in, 1);
        assert_eq!(stats.sessions.len(), 1);
        let session = &stats.sessions[0];
        assert_eq!(session.accounts, ["Work", "Spare"]);
        assert_eq!((session.refusals, session.turns, session.hops.len()), (1, 2, 1));
        assert_eq!(
            session.cost_per_turn,
            PerWindow {
                five_hour: Some(6.0),
                seven_day: None
            }
        );
        // As JSON: the shapes the --json output has always had.
        let out = serde_json::to_value(&stats).unwrap();
        assert_eq!(out["hopReasons"], json!({ "refusal": 1 }));
        assert_eq!(out["accounts"][0]["daily"][6]["fiveHourMax"], json!(70.5));
        assert_eq!(out["accounts"][0]["wasted"][0]["headroomLeft"], json!(35));
        assert_eq!(out["accounts"][1]["mark"], json!("last-resort"));
        assert_eq!(
            out["sessions"][0]["costPerTurn"],
            json!({ "five_hour": 6, "seven_day": null })
        );
        assert_eq!(out["hops"][0]["fromId"], json!("work"));
    }
}
