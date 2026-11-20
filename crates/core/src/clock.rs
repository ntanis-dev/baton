//! Time as Baton keeps it: epoch milliseconds in memory, ISO 8601 text (UTC, milliseconds) in its files,
//! and the operator's local wall clock on the screen.

use chrono::{DateTime, Datelike, Days, Local, LocalResult, Months, NaiveDate, TimeZone, Timelike, Utc};
use serde_json::Value;

/// A moment or a span, in milliseconds (a moment counts from the Unix epoch).
pub type Ms = i64;

/// One minute.
pub const MINUTE: Ms = 60_000;
/// One hour.
pub const HOUR: Ms = 60 * MINUTE;
/// One day.
pub const DAY: Ms = 24 * HOUR;

/// The time now.
pub fn now_ms() -> Ms {
    Utc::now().timestamp_millis()
}

/// A moment as the text Baton writes: `2026-11-15T09:30:00.000Z`. A moment outside the calendar is the
/// empty string.
pub fn iso(ms: Ms) -> String {
    match DateTime::<Utc>::from_timestamp_millis(ms) {
        Some(at) => at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        None => String::new(),
    }
}

/// [`iso`] of a moment that may be missing.
pub fn iso_opt(ms: Option<Ms>) -> Option<String> {
    ms.map(iso)
}

/// Reads a date as text: what [`iso`] writes, any other RFC 3339 time, a local time without a zone
/// (`2026-11-15T09:30:00`) or a bare date (UTC midnight). None for anything else.
pub fn parse_ms(text: &str) -> Option<Ms> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Some(at.timestamp_millis());
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text, format) {
            return from_local(naive);
        }
    }
    let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis())
}

/// [`parse_ms`] of text that may be missing.
pub fn parse_opt(text: Option<&str>) -> Option<Ms> {
    text.and_then(parse_ms)
}

/// A moment from JSON: a number is epoch milliseconds, text is read by [`parse_ms`].
pub fn ms_of(value: &Value) -> Option<Ms> {
    match value {
        Value::Number(n) => n.as_f64().filter(|n| n.is_finite()).map(|n| n as Ms),
        Value::String(text) => parse_ms(text),
        _ => None,
    }
}

/// The local date and time of a moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    /// The year.
    pub year: i32,
    /// The month, 1 to 12.
    pub month: u32,
    /// The day of the month, from 1.
    pub day: u32,
    /// The hour, 0 to 23.
    pub hour: u32,
    /// The minute.
    pub minute: u32,
    /// The second.
    pub second: u32,
    /// The day of the week, 0 for Sunday.
    pub weekday: u32,
}

/// The operator's wall clock at `ms`.
pub fn local(ms: Ms) -> LocalTime {
    let at = match Local.timestamp_millis_opt(ms) {
        LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => at,
        LocalResult::None => Local.timestamp_millis_opt(0).earliest().unwrap_or_default(),
    };
    LocalTime {
        year: at.year(),
        month: at.month(),
        day: at.day(),
        hour: at.hour(),
        minute: at.minute(),
        second: at.second(),
        weekday: at.weekday().num_days_from_sunday(),
    }
}

/// A naive local date and time as a moment. A time that occurs twice (the clock going back) is its
/// first occurrence; one that does not exist (the clock going forward) is the same reading an hour on.
fn from_local(naive: chrono::NaiveDateTime) -> Option<Ms> {
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => Some(at.timestamp_millis()),
        LocalResult::None => {
            let later = naive.checked_add_signed(chrono::Duration::hours(1))?;
            Local
                .from_local_datetime(&later)
                .earliest()
                .map(|at| at.timestamp_millis())
        }
    }
}

/// The moment of a local date and time. `month` counts from 1; a month past 12 or a day past the end
/// of its month runs on into the next one, as a calendar would.
pub fn local_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Option<Ms> {
    let first = NaiveDate::from_ymd_opt(year, 1, 1)?
        .checked_add_months(Months::new(month.checked_sub(1)?))?
        .checked_add_days(Days::new(u64::from(day.checked_sub(1)?)))?;
    from_local(first.and_hms_opt(hour, minute, 0)?)
}

/// The local date of a moment, `2026-11-15`.
pub fn local_date(ms: Ms) -> String {
    let at = local(ms);
    format!("{:04}-{:02}-{:02}", at.year, at.month, at.day)
}

/// Whether two moments fall on the same local day.
pub fn same_local_day(a: Ms, b: Ms) -> bool {
    let (x, y) = (local(a), local(b));
    (x.year, x.month, x.day) == (y.year, y.month, y.day)
}

/// The day of the week (0 for Sunday) of a calendar date; None when there is no such date.
pub fn weekday_of(year: i32, month: u32, day: u32) -> Option<u32> {
    NaiveDate::from_ymd_opt(year, month, day).map(|date| date.weekday().num_days_from_sunday())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn iso_round_trips() {
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(1_763_199_000_123), "2025-11-15T09:30:00.123Z");
        assert_eq!(parse_ms("2025-11-15T09:30:00.123Z"), Some(1_763_199_000_123));
        assert_eq!(parse_ms("2025-11-15T11:30:00.123+02:00"), Some(1_763_199_000_123));
        assert_eq!(parse_ms("2025-11-15"), Some(1_763_164_800_000));
        assert_eq!(parse_ms("soon"), None);
        assert_eq!(parse_ms(""), None);
    }

    #[test]
    fn moments_from_json() {
        assert_eq!(ms_of(&json!(1500)), Some(1500));
        assert_eq!(ms_of(&json!("1970-01-01T00:00:01.500Z")), Some(1500));
        assert_eq!(ms_of(&json!(null)), None);
        assert_eq!(ms_of(&json!("")), None);
        assert_eq!(ms_of(&json!(true)), None);
    }

    #[test]
    fn local_time_round_trips_and_runs_on() {
        let at = local_ms(2026, 3, 10, 15, 20).unwrap();
        let read = local(at);
        assert_eq!(
            (read.year, read.month, read.day, read.hour, read.minute),
            (2026, 3, 10, 15, 20)
        );
        assert_eq!(read.weekday, 2);
        assert_eq!(local_date(at), "2026-03-10");
        // The 32nd of January is the 1st of February; month 13 is January of the next year.
        assert_eq!(local_date(local_ms(2026, 1, 32, 12, 0).unwrap()), "2026-02-01");
        assert_eq!(local_date(local_ms(2026, 13, 1, 12, 0).unwrap()), "2027-01-01");
        assert!(same_local_day(at, at + HOUR));
        assert!(!same_local_day(at, at + DAY));
        assert_eq!(weekday_of(2026, 11, 15), Some(0));
    }
}
