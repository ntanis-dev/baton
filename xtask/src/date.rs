//! Just enough calendar maths for the release-age check, so the xtask needs no date crate.
//!
//! Days are counted from 1970-01-01 in UTC. The conversions are Howard Hinnant's `days_from_civil` and
//! `civil_from_days`, which are exact for the proleptic Gregorian calendar.

use std::time::{SystemTime, UNIX_EPOCH};

const SECONDS_PER_DAY: i64 = 86_400;

/// A calendar date in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i64,
    pub month: u32,
    pub day: u32,
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Days since 1970-01-01 for a calendar date.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The calendar date of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> Date {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Date {
        year,
        // Both are small by construction (1..=12 and 1..=31).
        month: u32::try_from(month).unwrap_or(1),
        day: u32::try_from(day).unwrap_or(1),
    }
}

/// Today's day count in UTC.
pub fn today() -> i64 {
    let seconds = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    };
    seconds.div_euclid(SECONDS_PER_DAY)
}

/// The UTC day count of an RFC 3339 timestamp such as `2026-10-19T08:12:45.123456+00:00` or
/// `2026-10-19T08:12:45Z`. `None` when it does not parse.
pub fn rfc3339_day(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut parts = date.splitn(3, '-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // The clock, then an optional fraction, then `Z` or `±hh:mm`.
    let clock_end = rest.find(['Z', 'z', '+', '-', '.']).unwrap_or(rest.len());
    let mut clock = rest[..clock_end].splitn(3, ':');
    let hours: i64 = clock.next()?.parse().ok()?;
    let minutes: i64 = clock.next()?.parse().ok()?;
    let seconds: i64 = clock.next()?.parse().ok()?;
    let mut tail = &rest[clock_end..];
    if let Some(fraction) = tail.strip_prefix('.') {
        let digits = fraction
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(fraction.len());
        tail = &fraction[digits..];
    }
    let offset = match tail {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match tail.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (offset_hours, offset_minutes) = tail[1..].split_once(':')?;
            let offset_hours: i64 = offset_hours.parse().ok()?;
            let offset_minutes: i64 = offset_minutes.parse().ok()?;
            sign * (offset_hours * 3600 + offset_minutes * 60)
        }
    };

    let local = days_from_civil(year, month, day) * SECONDS_PER_DAY + hours * 3600 + minutes * 60 + seconds;
    Some((local - offset).div_euclid(SECONDS_PER_DAY))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_day_zero() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0).to_string(), "1970-01-01");
    }

    #[test]
    fn conversions_round_trip_across_leap_years() {
        for days in -800_000..800_000 {
            if days % 997 != 0 {
                continue;
            }
            let date = civil_from_days(days);
            assert_eq!(days_from_civil(date.year, date.month, date.day), days);
        }
        assert_eq!(
            civil_from_days(days_from_civil(2024, 2, 29)).to_string(),
            "2024-02-29"
        );
        assert_eq!(
            civil_from_days(days_from_civil(2026, 10, 19) + 14).to_string(),
            "2026-11-02"
        );
    }

    #[test]
    fn timestamps_from_crates_io_parse() {
        let day = days_from_civil(2026, 10, 19);
        assert_eq!(rfc3339_day("2026-10-19T08:12:45.123456+00:00"), Some(day));
        assert_eq!(rfc3339_day("2026-10-19T23:59:59Z"), Some(day));
        assert_eq!(rfc3339_day("2026-10-19T00:00:00.5Z"), Some(day));
        // An offset can move the instant to another UTC day.
        assert_eq!(rfc3339_day("2026-10-20T01:00:00+02:00"), Some(day));
        assert_eq!(rfc3339_day("2026-10-19T23:00:00-02:00"), Some(day + 1));
    }

    #[test]
    fn nonsense_does_not_parse() {
        assert_eq!(rfc3339_day(""), None);
        assert_eq!(rfc3339_day("yesterday"), None);
        assert_eq!(rfc3339_day("2026-13-01T00:00:00Z"), None);
        assert_eq!(rfc3339_day("2026-10-19T00:00:00+0200"), None);
    }
}
