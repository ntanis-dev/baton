//! Time as Baton keeps it: epoch milliseconds in memory, ISO 8601 text (UTC, milliseconds) in its files,
//! and the operator's local wall clock on the screen.

use jiff::civil::{Date, DateTime};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
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
    Timestamp::now().as_millisecond()
}

/// A moment as the text Baton writes: `2026-11-15T09:30:00.000Z`. A moment outside the calendar is the
/// empty string.
pub fn iso(ms: Ms) -> String {
    match Timestamp::from_millisecond(ms) {
        Ok(at) => at.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        Err(_) => String::new(),
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
    if let Ok(at) = text.parse::<Timestamp>() {
        return Some(at.as_millisecond());
    }
    for format in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = DateTime::strptime(format, text) {
            return from_local(naive);
        }
    }
    let date = Date::strptime("%Y-%m-%d", text).ok()?;
    utc_ms(date)
}

/// UTC midnight at the start of a date.
fn utc_ms(date: Date) -> Option<Ms> {
    let at = TimeZone::UTC.to_timestamp(date.at(0, 0, 0, 0)).ok()?;
    Some(at.as_millisecond())
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
    let at = Timestamp::from_millisecond(ms)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_zoned(TimeZone::system());
    LocalTime {
        year: i32::from(at.year()),
        month: at.month().unsigned_abs().into(),
        day: at.day().unsigned_abs().into(),
        hour: at.hour().unsigned_abs().into(),
        minute: at.minute().unsigned_abs().into(),
        second: at.second().unsigned_abs().into(),
        weekday: at.weekday().to_sunday_zero_offset().unsigned_abs().into(),
    }
}

/// A naive local date and time as a moment. A time that occurs twice (the clock going back) is its
/// first occurrence; one that does not exist (the clock going forward) is the same reading an hour on.
fn from_local(naive: DateTime) -> Option<Ms> {
    let at = TimeZone::system()
        .to_ambiguous_timestamp(naive)
        .compatible()
        .ok()?;
    Some(at.as_millisecond())
}

/// The moment of a local date and time. `month` counts from 1; a month past 12 or a day past the end
/// of its month runs on into the next one, as a calendar would.
pub fn local_ms(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> Option<Ms> {
    let span = Span::new()
        .try_months(i64::from(month.checked_sub(1)?))
        .ok()?
        .try_days(i64::from(day.checked_sub(1)?))
        .ok()?;
    let date = Date::new(i16::try_from(year).ok()?, 1, 1)
        .ok()?
        .checked_add(span)
        .ok()?;
    let time = jiff::civil::Time::new(i8::try_from(hour).ok()?, i8::try_from(minute).ok()?, 0, 0).ok()?;
    from_local(date.to_datetime(time))
}

/// The first moment (UTC) of every month from the one holding `from` to `to`.
pub fn utc_months(from: Ms, to: Ms) -> Vec<Ms> {
    let mut out = Vec::new();
    let Ok(start) = Timestamp::from_millisecond(from) else {
        return out;
    };
    let mut next = Some(start.to_zoned(TimeZone::UTC).date().first_of_month());
    while let Some(first) = next {
        match utc_ms(first) {
            Some(at) if at <= to => out.push(at),
            _ => break,
        }
        next = first.checked_add(Span::new().months(1)).ok();
    }
    out
}

/// The local date of a moment, `2026-11-15`.
pub fn local_date(ms: Ms) -> String {
    let at = local(ms);
    format!("{:04}-{:02}-{:02}", at.year, at.month, at.day)
}

/// The local dates of the `days` days that end with the day of `now`, oldest first. They are stepped
/// through by the calendar: a day the clock changes on is not 24 hours long, and a step of 24 hours
/// can skip a date or name one twice.
pub fn local_days(now: Ms, days: i64) -> Vec<String> {
    let today = Timestamp::from_millisecond(now)
        .unwrap_or(Timestamp::UNIX_EPOCH)
        .to_zoned(TimeZone::system())
        .date();
    (0..days.max(1))
        .rev()
        .filter_map(|back| today.checked_sub(Span::new().try_days(back).ok()?).ok())
        .map(|date| format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day()))
        .collect()
}

/// Whether two moments fall on the same local day.
pub fn same_local_day(a: Ms, b: Ms) -> bool {
    let (x, y) = (local(a), local(b));
    (x.year, x.month, x.day) == (y.year, y.month, y.day)
}

/// The day of the week (0 for Sunday) of a calendar date; None when there is no such date.
pub fn weekday_of(year: i32, month: u32, day: u32) -> Option<u32> {
    let date = Date::new(
        i16::try_from(year).ok()?,
        i8::try_from(month).ok()?,
        i8::try_from(day).ok()?,
    )
    .ok()?;
    Some(date.weekday().to_sunday_zero_offset().unsigned_abs().into())
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
        assert_eq!(weekday_of(2026, 2, 30), None);
    }

    #[test]
    fn the_days_of_a_period_are_calendar_days() {
        let noon = local_ms(2026, 3, 10, 12, 0).unwrap();
        assert_eq!(local_days(noon, 3), ["2026-03-08", "2026-03-09", "2026-03-10"]);
        assert_eq!(local_days(noon, 1), ["2026-03-10"]);
        assert_eq!(local_days(noon, 0), ["2026-03-10"]);
        // Across the months where the clocks change (wherever this runs), every date is there once,
        // whatever the time of day.
        for (month, day, hour) in [(4, 5, 0), (4, 5, 23), (11, 10, 0), (11, 10, 23)] {
            let at = local_ms(2026, month, day, hour, 30).unwrap();
            let days = local_days(at, 40);
            assert_eq!(days.len(), 40);
            assert_eq!(days.last().unwrap(), &local_date(at));
            assert!(days.windows(2).all(|pair| pair[0] < pair[1]), "{days:?}");
        }
    }

    #[test]
    fn local_times_without_a_zone_and_times_that_are_not() {
        let at = local_ms(2026, 3, 10, 15, 20).unwrap();
        assert_eq!(parse_ms("2026-03-10T15:20:00"), Some(at));
        assert_eq!(parse_ms("2026-03-10T15:20"), Some(at));
        assert_eq!(parse_ms("2026-03-10T15:20:00.250"), Some(at + 250));
        assert_eq!(parse_ms("2026-03-10T15:20:00 or so"), None);
        assert_eq!(parse_ms("2026-13-40"), None);
        assert_eq!(iso(Ms::MAX), "");
        assert_eq!(local_ms(2026, 0, 1, 0, 0), None);
        assert_eq!(local_ms(2026, 1, 1, 24, 0), None);
    }

    #[test]
    fn the_months_of_a_period_start_with_the_one_that_holds_its_first_moment() {
        let from = parse_ms("2025-11-15T09:30:00.000Z").unwrap();
        let to = parse_ms("2026-01-01T00:00:00.000Z").unwrap();
        let months: Vec<String> = utc_months(from, to).into_iter().map(iso).collect();
        assert_eq!(
            months,
            [
                "2025-11-01T00:00:00.000Z",
                "2025-12-01T00:00:00.000Z",
                "2026-01-01T00:00:00.000Z"
            ]
        );
        assert_eq!(utc_months(from, from).len(), 1);
    }
}
