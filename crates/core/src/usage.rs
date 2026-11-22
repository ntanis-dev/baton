//! Usage readings and refusals per account and window.
//!
//! ```text
//! usage/<account>.json      latest reading: { account, at, session, source,
//!                           five_hour: { used, resetsAt, readAt } | null,
//!                           seven_day: { ... } | null }
//! rejected/<account>.<window>.json   the last refusal for that window
//!                           (window five_hour, seven_day or unknown)
//! history/readings-YYYY-MM.jsonl     every change of a reading
//! history/events-YYYY-MM.jsonl       refusals, hops, sessions
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::clock::{HOUR, MINUTE, Ms, iso, local, local_ms, parse_ms};
use crate::config::loose_f64;
use crate::fsutil::{append_json_line, read_json, read_json_as, write_json_atomic};
use crate::json::{Get, Object, lenient, ser_num, ser_opt_num};
use crate::oslock::OsLock;
use crate::paths::{Paths, is_account_id};
use crate::window::{Limit, PerWindow, WINDOWS, Window};

/// The errors of a turn that mean a usage limit.
pub const LIMIT_ERRORS: [&str; 2] = ["rate_limit", "billing_error"];

/// Two readings belong to the same window period when their resets_at are within a few minutes: a
/// reading's resets_at can move by a second between readings, while a real reset moves it by at least
/// the window's length (5 hours, 7 days).
pub const PERIOD_TOLERANCE_MS: Ms = 10 * MINUTE;

/// Whether two reset times are the same window period.
pub fn same_period(a: Option<&str>, b: Option<&str>) -> bool {
    let (Some(a), Some(b)) = (a, b) else {
        return a.is_none() && b.is_none();
    };
    match (parse_ms(a), parse_ms(b)) {
        (Some(x), Some(y)) => (x - y).abs() <= PERIOD_TOLERANCE_MS,
        _ => a == b,
    }
}

/// The month of a moment, `2026-11` (UTC).
fn month(at: Ms) -> String {
    iso(at).chars().take(7).collect()
}

/// The history file of `kind` (`readings`, `events`) for the month of `at`.
pub fn history_file(paths: &Paths, kind: &str, at: Ms) -> PathBuf {
    paths.history.join(format!("{kind}-{}.jsonl", month(at)))
}

/// Appends an event (an object) to the history, stamped `at`.
pub fn record_event(paths: &Paths, event: Value, at: Ms) {
    let mut line = Object::new();
    line.insert("at".to_owned(), json!(iso(at)));
    if let Value::Object(fields) = event {
        line.extend(fields);
    }
    let _ = append_json_line(&history_file(paths, "events", at), &line);
}

/// What one reading says of one window.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// Percent of the window's limit used.
    pub used: f64,
    /// When the window resets.
    pub resets_at: Option<String>,
}

/// The status line JSON's `rate_limits` as a reading (`resets_at` is epoch seconds there). None when
/// there is nothing to record.
pub fn reading_from_status_line(input: &Value) -> Option<PerWindow<Option<Sample>>> {
    let limits = input.get("rate_limits").filter(|limits| limits.is_object())?;
    let out = PerWindow::from_fn(|window| {
        let w = limits.get(window.as_str()).filter(|w| w.is_object())?;
        let used = w
            .get("used_percentage")
            .map(loose_f64)
            .filter(|n| n.is_finite())?;
        let seconds = loose_f64(w.at("resets_at"));
        let resets_at = (seconds.is_finite() && seconds > 0.0).then(|| iso((seconds * 1000.0) as Ms));
        Some(Sample { used, resets_at })
    });
    (out.five_hour.is_some() || out.seven_day.is_some()).then_some(out)
}

/// One window of a stored reading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowReading {
    /// Percent of the window's limit used.
    #[serde(serialize_with = "ser_num")]
    pub used: f64,
    /// When the window resets.
    #[serde(rename = "resetsAt", default, deserialize_with = "lenient")]
    pub resets_at: Option<String>,
    /// When this was read.
    #[serde(rename = "readAt", default, deserialize_with = "lenient")]
    pub read_at: Option<String>,
    /// Where it came from: `statusline` (a session) or `poll` (the usage poller).
    #[serde(default, deserialize_with = "lenient")]
    pub source: Option<String>,
    /// For a weekly limit of one model: its name. Never stored.
    #[serde(
        default,
        deserialize_with = "lenient",
        skip_serializing_if = "Option::is_none"
    )]
    pub label: Option<String>,
}

/// A meter of the usage endpoint as Baton keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meter {
    /// What the meter counts: `session`, `weekly_all`, a model's own weekly kind.
    pub kind: String,
    /// The group it belongs to: `session`, `weekly`.
    #[serde(default, deserialize_with = "lenient")]
    pub group: Option<String>,
    /// The model or surface it is scoped to.
    #[serde(default, deserialize_with = "lenient")]
    pub label: Option<String>,
    /// Percent used.
    #[serde(serialize_with = "ser_num")]
    pub used: f64,
    /// When it resets.
    #[serde(rename = "resetsAt", default, deserialize_with = "lenient")]
    pub resets_at: Option<String>,
    /// How close to the limit the endpoint says it is.
    #[serde(default, deserialize_with = "lenient")]
    pub severity: Option<String>,
    /// Whether it is the limit in force.
    #[serde(default)]
    pub active: bool,
}

/// The plan's extra-usage spend, from a poll.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtraUsage {
    /// Whether extra usage is on.
    #[serde(default)]
    pub enabled: bool,
    /// The monthly cap.
    #[serde(
        rename = "monthlyLimit",
        default,
        deserialize_with = "lenient",
        serialize_with = "ser_opt_num"
    )]
    pub monthly_limit: Option<f64>,
    /// What was spent.
    #[serde(
        rename = "usedCredits",
        default,
        deserialize_with = "lenient",
        serialize_with = "ser_opt_num"
    )]
    pub used_credits: Option<f64>,
    /// Percent of the cap used.
    #[serde(default, deserialize_with = "lenient", serialize_with = "ser_opt_num")]
    pub utilization: Option<f64>,
    /// The currency of the amounts.
    #[serde(default, deserialize_with = "lenient")]
    pub currency: Option<String>,
}

/// Reads a list, leaving out the rows that are not what they should be; None when it is not a list.
fn lenient_rows<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: serde::de::DeserializeOwned,
{
    let Value::Array(rows) = Value::deserialize(deserializer)? else {
        return Ok(None);
    };
    Ok(Some(
        rows.into_iter()
            .filter_map(|row| serde_json::from_value(row).ok())
            .collect(),
    ))
}

/// An account's latest reading, as `usage/<account>.json` holds it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reading {
    /// The account's id.
    #[serde(default)]
    pub account: String,
    /// When the file was last written.
    #[serde(default, deserialize_with = "lenient")]
    pub at: Option<String>,
    /// The session whose status line it came from.
    #[serde(default, deserialize_with = "lenient")]
    pub session: Option<String>,
    /// Where the last write came from.
    #[serde(default, deserialize_with = "lenient")]
    pub source: Option<String>,
    /// The 5-hour window.
    #[serde(default, deserialize_with = "lenient")]
    pub five_hour: Option<WindowReading>,
    /// The weekly window (all models).
    #[serde(default, deserialize_with = "lenient")]
    pub seven_day: Option<WindowReading>,
    /// Every meter of the plan, from the last poll.
    #[serde(default, deserialize_with = "lenient_rows")]
    pub limits: Option<Vec<Meter>>,
    /// When the meters were read.
    #[serde(rename = "limitsAt", default, deserialize_with = "lenient")]
    pub limits_at: Option<String>,
    /// The extra-usage spend, from the last poll.
    #[serde(rename = "extraUsage", default, deserialize_with = "lenient")]
    pub extra_usage: Option<ExtraUsage>,
}

impl Reading {
    /// The stored reading of `window`.
    pub fn window(&self, window: Window) -> Option<&WindowReading> {
        match window {
            Window::FiveHour => self.five_hour.as_ref(),
            Window::SevenDay => self.seven_day.as_ref(),
        }
    }
}

/// The file of an account's latest reading.
fn usage_file(paths: &Paths, account: &str) -> PathBuf {
    paths.usage.join(format!("{account}.json"))
}

/// An account's latest reading.
pub fn read_reading(paths: &Paths, account: &str) -> Option<Reading> {
    read_json_as(&usage_file(paths, account))
}

/// How a reading is recorded.
#[derive(Clone, Debug)]
pub struct RecordOptions {
    /// When it was read.
    pub at: Ms,
    /// The session it came from.
    pub session: Option<String>,
    /// `statusline` from a session, `poll` from the usage poller.
    pub source: String,
    /// An unchanged reading is written again only when the stored one is older than this.
    pub refresh_ms: Ms,
    /// A poll's meters (`Some(None)`: the poll brought none); None keeps the last ones.
    pub limits: Option<Option<Vec<Meter>>>,
    /// A poll's extra-usage spend; None keeps the last one.
    pub extra_usage: Option<Option<ExtraUsage>>,
}

impl RecordOptions {
    /// A session's status line reading at `at`.
    pub fn at(at: Ms) -> RecordOptions {
        RecordOptions {
            at,
            session: None,
            source: "statusline".to_owned(),
            refresh_ms: 30_000,
            limits: None,
            extra_usage: None,
        }
    }
}

/// How long a reading waits for another process that is recording one for the same account.
const RECORD_WAIT: Duration = Duration::from_millis(250);

/// What [`record_reading`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Recorded {
    /// Whether a window's value changed.
    pub changed: bool,
    /// Whether the file was written.
    pub written: bool,
    /// The reading as written.
    pub reading: Option<Reading>,
}

/// Records a reading (windows absent from it keep their previous value: Claude Code drops a window
/// from its status line once it has reset). Writes only when a value changed or the stored reading is
/// older than `refresh_ms`, and appends each changed window to the history. Each window keeps where it
/// came from (source: `statusline` from a session, `poll` from the usage poller) and when (readAt). A
/// poll also brings the plan's other meters (limits, e.g. a model's own weekly limit) and the
/// extra-usage spend; a status line reading keeps the last ones.
///
/// Every session's status line and the poller record here, so one process at a time reads the stored
/// reading and writes the next: what another wrote in between would be lost otherwise. A status line
/// that only repeats what a poll read leaves the poll's reading as it is: the status line shows what
/// its session last heard, which may be older than the poll.
pub fn record_reading(
    paths: &Paths,
    account: &str,
    reading: &PerWindow<Option<Sample>>,
    options: RecordOptions,
) -> Recorded {
    if !is_account_id(account) {
        return Recorded::default();
    }
    let RecordOptions {
        at,
        session,
        source,
        refresh_ms,
        limits,
        extra_usage,
    } = options;
    // The wait is short, and a lock that cannot be had never stops a reading from being recorded.
    let _lock = OsLock::take(&paths.run.join(format!("usage-{account}.lock")), RECORD_WAIT)
        .ok()
        .flatten();
    let previous = read_reading(paths, account);
    let before = |window: Window| previous.as_ref().and_then(|p| p.window(window));
    let polled = limits.is_some();
    let mut next = Reading {
        account: account.to_owned(),
        at: Some(iso(at)),
        session: session.clone(),
        source: Some(source.clone()),
        five_hour: before(Window::FiveHour).cloned(),
        seven_day: before(Window::SevenDay).cloned(),
        limits_at: if polled {
            Some(iso(at))
        } else {
            previous.as_ref().and_then(|p| p.limits_at.clone())
        },
        limits: match limits {
            Some(limits) => limits,
            None => previous.as_ref().and_then(|p| p.limits.clone()),
        },
        extra_usage: match extra_usage {
            Some(extra) => extra,
            None => previous.as_ref().and_then(|p| p.extra_usage.clone()),
        },
    };
    let mut changed = Vec::new();
    for window in WINDOWS {
        let Some(sample) = &reading[window] else {
            continue;
        };
        let same = before(window).is_some_and(|b| {
            b.used == sample.used && same_period(b.resets_at.as_deref(), sample.resets_at.as_deref())
        });
        if !same {
            changed.push(window);
        } else if source != "poll" && before(window).is_some_and(|b| b.source.as_deref() == Some("poll")) {
            continue;
        }
        let stored = WindowReading {
            used: sample.used,
            resets_at: sample.resets_at.clone(),
            read_at: Some(iso(at)),
            source: Some(source.clone()),
            label: None,
        };
        match window {
            Window::FiveHour => next.five_hour = Some(stored),
            Window::SevenDay => next.seven_day = Some(stored),
        }
    }
    let old = previous.as_ref().and_then(|p| p.at.as_deref()).and_then(parse_ms);
    if changed.is_empty() && !polled && old.is_some_and(|old| at - old < refresh_ms) {
        return Recorded::default();
    }
    if write_json_atomic(&usage_file(paths, account), &next).is_err() {
        return Recorded::default();
    }
    for window in &changed {
        let Some(stored) = next.window(*window) else {
            continue;
        };
        let _ = append_json_line(
            &history_file(paths, "readings", at),
            &json!({
                "at": iso(at),
                "account": account,
                "window": window.as_str(),
                "used": crate::json::num(stored.used),
                "resetsAt": stored.resets_at,
                "session": session,
                "source": source,
            }),
        );
    }
    Recorded {
        changed: !changed.is_empty(),
        written: true,
        reading: Some(next),
    }
}

/// The latest reading of each of these accounts that has one, by id.
pub fn load_readings(paths: &Paths, ids: &[String]) -> BTreeMap<String, Reading> {
    ids.iter()
        .filter_map(|id| Some((id.clone(), read_reading(paths, id)?)))
        .collect()
}

/// A refusal as `rejected/<account>.<window>.json` holds it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rejection {
    /// The account's id.
    #[serde(default)]
    pub account: String,
    /// The limit it is about: `five_hour`, `seven_day`, `unknown`.
    #[serde(default)]
    pub window: String,
    /// When it happened.
    #[serde(default, deserialize_with = "lenient")]
    pub at: Option<String>,
    /// When the limit lifts, when known.
    #[serde(default, deserialize_with = "lenient")]
    pub until: Option<String>,
    /// Where `until` came from: `message`, `reading`, `none`.
    #[serde(rename = "untilSource", default, deserialize_with = "lenient")]
    pub until_source: Option<String>,
    /// The error Claude Code named.
    #[serde(default, deserialize_with = "lenient")]
    pub error: Option<String>,
    /// The session it happened in.
    #[serde(default, deserialize_with = "lenient")]
    pub session: Option<String>,
    /// What Claude Code said.
    #[serde(default, deserialize_with = "lenient")]
    pub message: Option<String>,
}

/// The file of an account's last refusal for a limit.
fn rejection_file(paths: &Paths, account: &str, limit: Limit) -> PathBuf {
    paths.rejected.join(format!("{account}.{limit}.json"))
}

/// Records a refusal: the last one per account and limit is kept.
pub fn record_rejection(paths: &Paths, account: &str, rejection: &Rejection) {
    if !is_account_id(account) {
        return;
    }
    let limit = Limit::parse(&rejection.window);
    let stored = Rejection {
        account: account.to_owned(),
        window: limit.as_str().to_owned(),
        until_source: Some(
            rejection
                .until_source
                .clone()
                .unwrap_or_else(|| "none".to_owned()),
        ),
        ..rejection.clone()
    };
    let _ = write_json_atomic(&rejection_file(paths, account, limit), &stored);
}

/// An account's last refusal per limit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rejections {
    /// The 5-hour window's.
    pub five_hour: Option<Rejection>,
    /// The weekly window's.
    pub seven_day: Option<Rejection>,
    /// One Baton could not tie to a window.
    pub unknown: Option<Rejection>,
}

impl Rejections {
    /// The last refusal for `limit`.
    pub fn get(&self, limit: Limit) -> Option<&Rejection> {
        match limit {
            Limit::FiveHour => self.five_hour.as_ref(),
            Limit::SevenDay => self.seven_day.as_ref(),
            Limit::Unknown => self.unknown.as_ref(),
        }
    }

    /// Sets the last refusal for `limit`.
    pub fn set(&mut self, limit: Limit, rejection: Rejection) {
        match limit {
            Limit::FiveHour => self.five_hour = Some(rejection),
            Limit::SevenDay => self.seven_day = Some(rejection),
            Limit::Unknown => self.unknown = Some(rejection),
        }
    }
}

/// The last refusals of each of these accounts, by id.
pub fn load_rejections(paths: &Paths, ids: &[String]) -> BTreeMap<String, Rejections> {
    let mut out: BTreeMap<String, Rejections> =
        ids.iter().map(|id| (id.clone(), Rejections::default())).collect();
    let Ok(files) = fs::read_dir(&paths.rejected) else {
        return out;
    };
    for file in files.flatten() {
        let name = file.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let Some((account, limit)) = stem.rsplit_once('.') else {
            continue;
        };
        if !["five_hour", "seven_day", "unknown"].contains(&limit) {
            continue;
        }
        let Some(slot) = out.get_mut(account) else {
            continue;
        };
        // A file that is JSON but not an object (null, false) is no refusal.
        let stored = read_json(&file.path()).filter(Value::is_object);
        if let Some(value) = stored.and_then(|raw| serde_json::from_value::<Rejection>(raw).ok()) {
            slot.set(Limit::parse(limit), value);
        }
    }
    out
}

/// The months as Claude Code abbreviates them.
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// The reset time Claude Code prints in a limit message ("resets 3pm", "resets 3:20pm", "resets at
/// 15:20", "resets Oct 27, 9am", "resets in 2h 30m"), read as local time. None when there is none.
pub fn parse_reset_time(text: &str, at: Ms) -> Option<Ms> {
    parse_time_after(text, "resets", at)
}

/// The same after another word ("rate limited until 3:14am": "until").
pub fn parse_time_after(text: &str, word: &str, at: Ms) -> Option<Ms> {
    if word.is_empty() || !word.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let number = |m: Option<regex::Match<'_>>| m.and_then(|m| m.as_str().parse::<i64>().ok());
    let rel = Regex::new(&format!(
        r"(?i)\b{word} in\s+(?:(\d+)\s*h(?:ours?)?)?\s*(?:(\d+)\s*m(?:in(?:utes?)?)?)?"
    ))
    .ok()?;
    if let Some(found) = rel.captures(text)
        && (found.get(1).is_some() || found.get(2).is_some())
    {
        let hours = number(found.get(1)).unwrap_or(0);
        let minutes = number(found.get(2)).unwrap_or(0);
        return Some(at + (hours * 60 + minutes) * MINUTE);
    }
    let abs = Regex::new(&format!(
        r"(?i)\b{word}\s+(?:at\s+)?(?:([A-Za-z]{{3}})[a-z]*\.?\s+(\d{{1,2}}),?\s+(?:at\s+)?)?(\d{{1,2}})(?::(\d{{2}}))?\s*([ap]\.?m\.?)?"
    ))
    .ok()?;
    let found = abs.captures(text)?;
    let mut hour = number(found.get(3))?;
    let minute = number(found.get(4)).unwrap_or(0);
    let ampm = found.get(5).map(|m| m.as_str().to_lowercase().replace('.', ""));
    if found.get(4).is_none() && ampm.is_none() {
        return None; // a bare number is not a time
    }
    if ampm.as_deref() == Some("pm") && hour < 12 {
        hour += 12;
    }
    if ampm.as_deref() == Some("am") && hour == 12 {
        hour = 0;
    }
    if hour > 23 || minute > 59 {
        return None;
    }
    let (hour, minute) = (hour as u32, minute as u32);
    let base = local(at);
    if let Some(name) = found.get(1) {
        let month = MONTHS.iter().position(|m| *m == name.as_str().to_lowercase())? as u32 + 1;
        let day = number(found.get(2))? as u32;
        let date = local_ms(base.year, month, day, hour, minute)?;
        if date < at - 24 * HOUR {
            return local_ms(base.year + 1, month, day, hour, minute);
        }
        return Some(date);
    }
    let date = local_ms(base.year, base.month, base.day, hour, minute)?;
    if date <= at {
        return local_ms(base.year, base.month, base.day + 1, hour, minute);
    }
    Some(date)
}

/// Which limit a refusal is about and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inferred {
    /// The limit.
    pub window: Limit,
    /// When it lifts, when known.
    pub until: Option<String>,
    /// Where `until` came from: `message`, `reading`, `none`.
    pub until_source: &'static str,
}

/// Which window a refusal is about and until when, from what the StopFailure hook knows: the error
/// text (Claude Code names the "session limit" (5-hour) or a "weekly"/per-model weekly limit and
/// usually prints the reset) and the account's latest reading.
pub fn infer_rejection(
    error: Option<&str>,
    message: Option<&str>,
    details: Option<&str>,
    reading: Option<&Reading>,
    at: Ms,
) -> Inferred {
    let text = [message, details]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let lower = text.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    let mut window = None;
    if has(&[
        "weekly",
        "7-day",
        "seven-day",
        "seven day",
        "opus limit",
        "sonnet limit",
        "fable limit",
    ]) || limit_scope(&text).is_some()
    {
        window = Some(Limit::SevenDay);
    } else if has(&["session limit", "5-hour", "five-hour", "five hour"]) {
        window = Some(Limit::FiveHour);
    }
    // A window's reading, unless it has reset since.
    let current = |w: Window| {
        let r = reading?.window(w)?;
        let resets = r.resets_at.as_deref().and_then(parse_ms);
        resets.is_none_or(|resets| resets > at).then_some(r)
    };
    if window.is_none() && error == Some("rate_limit") {
        let week = current(Window::SevenDay);
        let five = current(Window::FiveHour);
        if week.is_some_and(|w| w.used >= 99.5) {
            window = Some(Limit::SevenDay);
        } else if five.is_some_and(|f| f.used >= 99.5) {
            window = Some(Limit::FiveHour);
        } else if five.is_some() || week.is_some() {
            let five_used = five.map_or(-1.0, |f| f.used);
            let week_used = week.map_or(-1.0, |w| w.used);
            window = Some(if five_used >= week_used {
                Limit::FiveHour
            } else {
                Limit::SevenDay
            });
        }
    }
    let window = window.unwrap_or(Limit::Unknown);
    if let Some(from_message) = parse_reset_time(&text, at).filter(|t| *t > at) {
        return Inferred {
            window,
            until: Some(iso(from_message)),
            until_source: "message",
        };
    }
    let resets = window
        .window()
        .and_then(current)
        .and_then(|r| r.resets_at.as_deref())
        .and_then(parse_ms);
    if let Some(resets) = resets.filter(|r| *r > at) {
        return Inferred {
            window,
            until: Some(iso(resets)),
            until_source: "reading",
        };
    }
    Inferred {
        window,
        until: None,
        until_source: "none",
    }
}

/// The model a refusal's words say has used up its own weekly limit: `Fable` for "You've hit your
/// Fable limit". None for the week's limit, the session's, or words that name no limit of a model.
pub fn limit_scope(text: &str) -> Option<String> {
    // What stands before "limit" without being a model.
    const OTHER: [&str; 22] = [
        "weekly", "session", "usage", "rate", "your", "the", "a", "this", "daily", "hour", "day", "spend",
        "spending", "credit", "monthly", "extra", "token", "context", "output", "request", "message", "api",
    ];
    let lower = text.to_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find(" limit") {
        let end = from + at;
        let word = lower[..end]
            .rsplit(|c: char| !c.is_alphanumeric() && c != '-')
            .next()
            .unwrap_or("");
        if !word.is_empty() && word.chars().all(char::is_alphabetic) && !OTHER.contains(&word) {
            let mut letters = word.chars();
            let first = letters.next()?;
            return Some(first.to_uppercase().chain(letters).collect());
        }
        from = end + " limit".len();
    }
    None
}

/// Whether a limit labelled `label` (a model's name, as the usage endpoint or a refusal gives it:
/// `Fable`) is the limit of the model a session runs (`claude-fable-5-2`, `fable`).
pub fn covers(label: &str, model: &str) -> bool {
    let name = label.split_whitespace().next().unwrap_or("").to_lowercase();
    !name.is_empty() && model.to_lowercase().contains(&name)
}

/// Whether two labels name the same model: `Fable` and `fable`.
pub fn same_model(a: &str, b: &str) -> bool {
    let first = |label: &str| label.split_whitespace().next().unwrap_or("").to_lowercase();
    !first(a).is_empty() && first(a) == first(b)
}

/// Whether the plan has no 5-hour limit, as far as the account's last poll tells: the poll listed
/// the plan's meters and none of them is the session's, and no session has reported that window
/// since. Before the first poll nothing is known, so it counts as there.
pub fn no_five_hour(reading: &Reading) -> bool {
    let (Some(limits), Some(polled)) = (&reading.limits, reading.limits_at.as_deref().and_then(parse_ms))
    else {
        return false;
    };
    if limits.is_empty()
        || limits
            .iter()
            .any(|row| row.kind == "session" || row.group.as_deref() == Some("session"))
    {
        return false;
    }
    reading
        .five_hour
        .as_ref()
        .and_then(|window| window.read_at.as_deref())
        .and_then(parse_ms)
        .is_none_or(|read| read <= polled)
}

/// Whether a meter is a weekly one.
pub fn is_weekly_meter(row: &Meter) -> bool {
    row.group.as_deref() == Some("weekly") || row.kind.starts_with("weekly")
}

/// The plan's weekly meters other than the all-models one (a model's own weekly limit, from a poll)
/// are kept in the reading's limits. The policy works from the strictest weekly limit in force: the
/// all-models window, or a scoped one that is fuller and has not reset yet (a poll's value is a floor:
/// use only grows until the reset).
pub fn strictest_weekly(reading: Option<&Reading>, now: Ms) -> Option<WindowReading> {
    let mut best = all_weekly(reading);
    for own in own_weekly(reading, now) {
        if best.as_ref().is_none_or(|b| b.used < own.used) {
            best = Some(own);
        }
    }
    best
}

/// The all-models weekly window by itself: what every model draws on.
pub fn all_weekly(reading: Option<&Reading>) -> Option<WindowReading> {
    let reading = reading?;
    reading.seven_day.as_ref().map(|base| WindowReading {
        source: base.source.clone().or_else(|| reading.source.clone()),
        label: None,
        ..base.clone()
    })
}

/// Each model's own weekly limit that has not reset yet, from the last poll, labelled with its model.
pub fn own_weekly(reading: Option<&Reading>, now: Ms) -> Vec<WindowReading> {
    let Some(reading) = reading else {
        return Vec::new();
    };
    let (Some(limits), Some(read_at)) = (&reading.limits, reading.limits_at.as_deref().and_then(parse_ms))
    else {
        return Vec::new();
    };
    limits
        .iter()
        .filter(|row| is_weekly_meter(row) && row.kind != "weekly_all")
        .filter(|row| match row.resets_at.as_deref().and_then(parse_ms) {
            Some(resets) => resets > now,
            None => now - read_at <= Window::SevenDay.length(),
        })
        .map(|row| WindowReading {
            used: row.used,
            resets_at: row.resets_at.clone(),
            read_at: reading.limits_at.clone(),
            source: Some("poll".to_owned()),
            label: Some(row.label.clone().unwrap_or_else(|| row.kind.clone())),
        })
        .collect()
}

/// A session's reading at one turn end, as `turns.jsonl` holds it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnSample {
    /// When the turn ended.
    pub at: Option<Ms>,
    /// The account the session ran on.
    pub account: Option<String>,
    /// What the session's status line last said of each window.
    pub windows: PerWindow<Option<Sample>>,
}

impl TurnSample {
    /// A sample from a line of `turns.jsonl`.
    pub fn from_value(value: &Value) -> TurnSample {
        TurnSample {
            at: crate::clock::ms_of(value.at("at")),
            account: value.text("account").map(str::to_owned),
            windows: PerWindow::from_fn(|w| {
                let window = value.get(w.as_str())?;
                Some(Sample {
                    used: window.number("used")?,
                    resets_at: window.text("resetsAt").map(str::to_owned),
                })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::DAY;
    use crate::paths::baton_paths;

    const T0: Ms = 1_780_000_000_000;

    fn sample(used: f64, resets: Option<Ms>) -> Option<Sample> {
        Some(Sample {
            used,
            resets_at: resets.map(iso),
        })
    }

    #[test]
    fn periods() {
        let a = iso(T0);
        assert!(same_period(Some(&a), Some(&iso(T0 + 5 * MINUTE))));
        assert!(!same_period(Some(&a), Some(&iso(T0 + 11 * MINUTE))));
        assert!(same_period(None, None));
        assert!(!same_period(Some(&a), None));
        assert!(same_period(Some("soon"), Some("soon")));
        assert!(!same_period(Some("soon"), Some(&a)));
    }

    #[test]
    fn a_status_line_reading() {
        let input = json!({ "rate_limits": {
            "five_hour": { "used_percentage": 42.5, "resets_at": 1_780_000_000 },
            "seven_day": { "used_percentage": "63" }
        } });
        let reading = reading_from_status_line(&input).unwrap();
        assert_eq!(
            reading.five_hour,
            Some(Sample {
                used: 42.5,
                resets_at: Some(iso(T0))
            })
        );
        assert_eq!(
            reading.seven_day,
            Some(Sample {
                used: 63.0,
                resets_at: None
            })
        );
        assert_eq!(reading_from_status_line(&json!({})), None);
        assert_eq!(reading_from_status_line(&json!({ "rate_limits": {} })), None);
        assert_eq!(
            reading_from_status_line(
                &json!({ "rate_limits": { "five_hour": { "used_percentage": "many" } } })
            ),
            None
        );
    }

    #[test]
    fn readings_are_recorded_when_they_change() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let first = PerWindow {
            five_hour: sample(10.0, Some(T0 + HOUR)),
            seven_day: sample(20.0, Some(T0 + DAY)),
        };
        let done = record_reading(
            &paths,
            "work",
            &first,
            RecordOptions {
                session: Some("s1".into()),
                ..RecordOptions::at(T0)
            },
        );
        assert!(done.changed && done.written);
        let stored = read_reading(&paths, "work").unwrap();
        assert_eq!(stored, done.reading.unwrap());
        assert_eq!(stored.five_hour.as_ref().unwrap().used, 10.0);
        assert_eq!(
            stored.five_hour.as_ref().unwrap().source.as_deref(),
            Some("statusline")
        );
        assert_eq!(stored.session.as_deref(), Some("s1"));
        // The file's shape, as it has always been written.
        let raw = read_json(&usage_file(&paths, "work")).unwrap();
        assert_eq!(
            raw.as_object().unwrap().keys().collect::<Vec<_>>(),
            [
                "account",
                "at",
                "session",
                "source",
                "five_hour",
                "seven_day",
                "limits",
                "limitsAt",
                "extraUsage"
            ]
        );
        assert_eq!(
            raw["five_hour"],
            json!({ "used": 10, "resetsAt": iso(T0 + HOUR), "readAt": iso(T0), "source": "statusline" })
        );
        // Unchanged and recent: nothing is written. A second moved by a second is the same period.
        let again = PerWindow {
            five_hour: sample(10.0, Some(T0 + HOUR + 1000)),
            seven_day: None,
        };
        assert_eq!(
            record_reading(&paths, "work", &again, RecordOptions::at(T0 + 10_000)),
            Recorded::default()
        );
        // Unchanged but old: written, not changed.
        let later = record_reading(&paths, "work", &again, RecordOptions::at(T0 + 40_000));
        assert!(later.written && !later.changed);
        // A window absent from the reading keeps its value.
        assert_eq!(
            read_reading(&paths, "work").unwrap().seven_day.unwrap().used,
            20.0
        );
        // A change is history.
        let more = PerWindow {
            five_hour: sample(15.0, Some(T0 + HOUR)),
            seven_day: None,
        };
        assert!(record_reading(&paths, "work", &more, RecordOptions::at(T0 + 50_000)).changed);
        let history = crate::fsutil::read_json_lines(&history_file(&paths, "readings", T0));
        assert_eq!(history.len(), 3);
        assert_eq!(history[2]["window"], "five_hour");
        assert_eq!(history[2]["used"], json!(15));
        // Not an account id: nothing.
        assert_eq!(
            record_reading(&paths, "../x", &more, RecordOptions::at(T0)),
            Recorded::default()
        );
    }

    #[test]
    fn a_poll_brings_the_meters_and_a_status_line_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let meter = Meter {
            kind: "weekly_fable".into(),
            group: Some("weekly".into()),
            label: Some("Fable".into()),
            used: 80.0,
            resets_at: Some(iso(T0 + DAY)),
            severity: None,
            active: false,
        };
        let reading = PerWindow {
            five_hour: sample(10.0, None),
            seven_day: sample(30.0, Some(T0 + DAY)),
        };
        let polled = record_reading(
            &paths,
            "work",
            &reading,
            RecordOptions {
                source: "poll".into(),
                refresh_ms: 0,
                limits: Some(Some(vec![meter.clone()])),
                extra_usage: Some(None),
                ..RecordOptions::at(T0)
            },
        );
        assert!(polled.written);
        record_reading(
            &paths,
            "work",
            &PerWindow {
                five_hour: sample(12.0, None),
                seven_day: None,
            },
            RecordOptions::at(T0 + 1000),
        );
        let stored = read_reading(&paths, "work").unwrap();
        assert_eq!(stored.limits, Some(vec![meter]));
        assert_eq!(stored.limits_at, Some(iso(T0)));
        assert_eq!(
            stored.five_hour.as_ref().unwrap().source.as_deref(),
            Some("statusline")
        );
        assert_eq!(stored.seven_day.as_ref().unwrap().source.as_deref(), Some("poll"));
        // A status line that repeats what the poll read leaves the poll's reading as it is; one that
        // says more is the later word.
        let weekly = |used: f64, at: Ms| {
            record_reading(
                &paths,
                "work",
                &PerWindow {
                    five_hour: None,
                    seven_day: sample(used, Some(T0 + DAY)),
                },
                RecordOptions::at(at),
            );
            read_reading(&paths, "work").unwrap().seven_day.unwrap()
        };
        let repeated = weekly(30.0, T0 + HOUR);
        assert_eq!(
            (repeated.source.as_deref(), repeated.read_at),
            (Some("poll"), Some(iso(T0)))
        );
        let more = weekly(31.0, T0 + HOUR + 1000);
        assert_eq!(
            (more.source.as_deref(), more.read_at),
            (Some("statusline"), Some(iso(T0 + HOUR + 1000)))
        );
        let stored = Reading {
            seven_day: stored.seven_day.clone(),
            ..read_reading(&paths, "work").unwrap()
        };
        // The policy's weekly window is the model's own limit while it is the fuller one.
        let strict = strictest_weekly(Some(&stored), T0 + HOUR).unwrap();
        assert_eq!(
            (strict.used, strict.label.as_deref(), strict.source.as_deref()),
            (80.0, Some("Fable"), Some("poll"))
        );
        // Once it has reset, the all-models window again.
        let strict = strictest_weekly(Some(&stored), T0 + 2 * DAY).unwrap();
        assert_eq!((strict.used, strict.label), (30.0, None));
        assert_eq!(strictest_weekly(None, T0), None);
    }

    #[test]
    fn readings_recorded_at_the_same_time_all_land() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        // A poll and a session's status line, over and over: each window keeps its latest value.
        let record = |window: Window, source: &'static str| {
            let paths = paths.clone();
            std::thread::spawn(move || {
                for n in 1..=40 {
                    let value = sample(f64::from(n), Some(T0 + DAY));
                    let reading = match window {
                        Window::FiveHour => PerWindow {
                            five_hour: value,
                            seven_day: None,
                        },
                        Window::SevenDay => PerWindow {
                            five_hour: None,
                            seven_day: value,
                        },
                    };
                    record_reading(
                        &paths,
                        "work",
                        &reading,
                        RecordOptions {
                            source: source.into(),
                            ..RecordOptions::at(T0 + Ms::from(n))
                        },
                    );
                }
            })
        };
        let both = [
            record(Window::FiveHour, "statusline"),
            record(Window::SevenDay, "poll"),
        ];
        for thread in both {
            thread.join().unwrap();
        }
        let stored = read_reading(&paths, "work").unwrap();
        assert_eq!(stored.five_hour.unwrap().used, 40.0);
        assert_eq!(stored.seven_day.unwrap().used, 40.0);
    }

    #[test]
    fn refusals_are_kept_per_limit() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let ids = vec!["work".to_owned(), "spare".to_owned()];
        assert_eq!(load_rejections(&paths, &ids)["work"], Rejections::default());
        record_rejection(
            &paths,
            "work",
            &Rejection {
                window: "five_hour".into(),
                at: Some(iso(T0)),
                until: Some(iso(T0 + HOUR)),
                until_source: Some("message".into()),
                ..Rejection::default()
            },
        );
        record_rejection(
            &paths,
            "work",
            &Rejection {
                window: "nonsense".into(),
                at: Some(iso(T0)),
                ..Rejection::default()
            },
        );
        record_rejection(
            &paths,
            "other",
            &Rejection {
                window: "seven_day".into(),
                ..Rejection::default()
            },
        );
        let all = load_rejections(&paths, &ids);
        let work = &all["work"];
        assert_eq!(work.get(Limit::FiveHour).unwrap().until, Some(iso(T0 + HOUR)));
        assert_eq!(
            work.get(Limit::Unknown).unwrap().until_source.as_deref(),
            Some("none")
        );
        assert_eq!(work.get(Limit::Unknown).unwrap().window, "unknown");
        assert!(work.seven_day.is_none());
        assert_eq!(all["spare"], Rejections::default());
        assert!(!all.contains_key("other"));
    }

    #[test]
    fn reset_times_in_messages() {
        let noon = local_ms(2026, 3, 10, 12, 0).unwrap();
        let at = |h, m| local_ms(2026, 3, 10, h, m).unwrap();
        assert_eq!(
            parse_reset_time("limit reached, resets 3pm", noon),
            Some(at(15, 0))
        );
        assert_eq!(
            parse_reset_time("resets 3:20pm (Europe/Athens)", noon),
            Some(at(15, 20))
        );
        assert_eq!(parse_reset_time("Resets at 15:20", noon), Some(at(15, 20)));
        assert_eq!(
            parse_reset_time("resets 12am", noon),
            Some(local_ms(2026, 3, 11, 0, 0).unwrap())
        );
        // A time already past today is tomorrow's.
        assert_eq!(
            parse_reset_time("resets 9am", noon),
            Some(local_ms(2026, 3, 11, 9, 0).unwrap())
        );
        assert_eq!(
            parse_reset_time("resets Oct 27, 9am", noon),
            Some(local_ms(2026, 10, 27, 9, 0).unwrap())
        );
        assert_eq!(
            parse_reset_time("resets Jan 5 at 9:30am", noon),
            Some(local_ms(2027, 1, 5, 9, 30).unwrap())
        );
        assert_eq!(
            parse_reset_time("resets in 2h 30m", noon),
            Some(noon + 150 * MINUTE)
        );
        assert_eq!(
            parse_reset_time("resets in 45 minutes", noon),
            Some(noon + 45 * MINUTE)
        );
        assert_eq!(parse_reset_time("resets in 3 hours", noon), Some(noon + 3 * HOUR));
        assert_eq!(parse_reset_time("resets 5", noon), None);
        assert_eq!(parse_reset_time("resets 25:00", noon), None);
        assert_eq!(parse_reset_time("no time here", noon), None);
        assert_eq!(
            parse_time_after("rate limited until 3:14am.", "until", noon),
            Some(local_ms(2026, 3, 11, 3, 14).unwrap())
        );
        assert_eq!(parse_time_after("until 3pm", "un til", noon), None);
    }

    #[test]
    fn what_a_refusal_is_about() {
        let reading = Reading {
            five_hour: Some(WindowReading {
                used: 100.0,
                resets_at: Some(iso(T0 + HOUR)),
                read_at: None,
                source: None,
                label: None,
            }),
            seven_day: Some(WindowReading {
                used: 50.0,
                resets_at: Some(iso(T0 + DAY)),
                read_at: None,
                source: None,
                label: None,
            }),
            ..Reading::default()
        };
        let inferred = infer_rejection(
            Some("rate_limit"),
            Some("You've hit your session limit"),
            None,
            Some(&reading),
            T0,
        );
        assert_eq!(
            inferred,
            Inferred {
                window: Limit::FiveHour,
                until: Some(iso(T0 + HOUR)),
                until_source: "reading"
            }
        );
        let inferred = infer_rejection(
            Some("rate_limit"),
            Some("weekly limit reached"),
            Some("resets in 2h"),
            Some(&reading),
            T0,
        );
        assert_eq!(
            inferred,
            Inferred {
                window: Limit::SevenDay,
                until: Some(iso(T0 + 2 * HOUR)),
                until_source: "message"
            }
        );
        // No words: the window that is full.
        let inferred = infer_rejection(Some("rate_limit"), None, None, Some(&reading), T0);
        assert_eq!(inferred.window, Limit::FiveHour);
        // A reading that has reset says nothing.
        let inferred = infer_rejection(Some("rate_limit"), None, None, Some(&reading), T0 + 2 * DAY);
        assert_eq!(
            inferred,
            Inferred {
                window: Limit::Unknown,
                until: None,
                until_source: "none"
            }
        );
        let inferred = infer_rejection(Some("billing_error"), Some("no credit"), None, Some(&reading), T0);
        assert_eq!(inferred.window, Limit::Unknown);
    }

    #[test]
    fn turn_samples_from_lines() {
        let sample = TurnSample::from_value(&json!({
            "at": iso(T0), "account": "work",
            "five_hour": { "used": 12, "resetsAt": iso(T0 + HOUR) }, "seven_day": null
        }));
        assert_eq!(sample.at, Some(T0));
        assert_eq!(sample.account.as_deref(), Some("work"));
        assert_eq!(
            sample.windows.five_hour,
            Some(Sample {
                used: 12.0,
                resets_at: Some(iso(T0 + HOUR))
            })
        );
        assert_eq!(sample.windows.seven_day, None);
        assert_eq!(TurnSample::from_value(&json!(null)), TurnSample::default());
    }

    #[test]
    fn events_are_stamped() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        record_event(&paths, json!({ "type": "hop", "from": "a", "to": "b" }), T0);
        let lines = crate::fsutil::read_json_lines(&history_file(&paths, "events", T0));
        assert_eq!(
            lines,
            vec![json!({ "at": iso(T0), "type": "hop", "from": "a", "to": "b" })]
        );
        assert!(history_file(&paths, "events", T0).ends_with("events-2026-05.jsonl"));
    }

    #[test]
    fn a_refusal_names_the_model_whose_limit_it_is() {
        assert_eq!(
            limit_scope("You've hit your Fable limit \u{b7} resets Nov 22, 7am (Europe/Athens)").as_deref(),
            Some("Fable")
        );
        assert_eq!(limit_scope("Opus limit reached").as_deref(), Some("Opus"));
        for words in [
            "You've hit your weekly limit",
            "You've hit your session limit \u{b7} resets 3pm",
            "5-hour limit reached",
            "You've hit your limit",
            "Rate limit exceeded",
            "usage limit reached, then the limit lifts",
            "",
        ] {
            assert_eq!(limit_scope(words), None, "{words}");
        }
        assert!(covers("Fable", "claude-fable-5-2") && covers("Fable 5", "fable"));
        assert!(!covers("Fable", "claude-opus-5-5") && !covers("", "claude-opus-5-5"));
        assert!(same_model("Fable", "fable") && !same_model("Fable", "Opus") && !same_model("", ""));
        // The refusal is a weekly one, whichever model it names.
        let inferred = infer_rejection(
            Some("rate_limit"),
            Some("You've hit your Haiku limit"),
            None,
            None,
            T0,
        );
        assert_eq!(inferred.window, Limit::SevenDay);
    }
}
