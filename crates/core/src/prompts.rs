//! The words Baton uses: the first prompt of a resumed conversation and the one-line notices. Pure;
//! times are formatted in local time.
//!
//! The notices are the operator's, on the terminal, and name the accounts. The prompts are read by the
//! model and name none: which accounts the operator has, and what they are called, is not the model's
//! to know.

use crate::clock::{Ms, local, same_local_day};
use crate::window::Limit;

/// The short names of the days, Sunday first.
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
/// The short names of the months.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A moment as the operator's clock shows it: `15:20` today, `Tue, Oct 27 15:20` on another day.
pub fn clock(ms: Option<Ms>, now: Ms) -> String {
    let Some(ms) = ms else {
        return "an unknown time".to_owned();
    };
    let at = local(ms);
    let time = format!("{:02}:{:02}", at.hour, at.minute);
    if same_local_day(ms, now) {
        return time;
    }
    format!(
        "{}, {} {} {time}",
        DAYS[at.weekday as usize % 7],
        MONTHS[(at.month as usize + 11) % 12],
        at.day
    )
}

/// A span in words: `45 s`, `12 min`, `2 h 5 min`, `3 d 4 h`.
pub fn duration(ms_left: Option<Ms>) -> String {
    let Some(ms_left) = ms_left else {
        return "unknown".to_owned();
    };
    let s = ((ms_left as f64 / 1000.0).round() as i64).max(0);
    if s < 60 {
        return format!("{s} s");
    }
    let m = (s as f64 / 60.0).round() as i64;
    if m < 60 {
        return format!("{m} min");
    }
    let h = m / 60;
    if h < 48 {
        return format!("{h} h {} min", m % 60);
    }
    format!("{} d {} h", h / 24, h % 24)
}

/// A limit by name: `5-hour limit`, `weekly limit`, `usage limit`.
fn limit_name(window: Option<Limit>) -> String {
    format!("{} limit", window.map_or("usage", Limit::label))
}

/// " (resets 15:20)" when the reset is known.
fn resets(until: Option<Ms>, now: Ms) -> String {
    match until {
        Some(until) if until != 0 => format!(" (resets {})", clock(Some(until), now)),
        _ => String::new(),
    }
}

/// The account a conversation ran on, for the model: no name.
const RAN_ON: &str = "the account it ran on";

/// Why the conversation moved, in one clause. `from` is the account it ran on, as the reader may
/// know it: its name for the operator, no name for the model.
pub fn hop_reason(reason: &str, from: &str, window: Option<Limit>, until: Option<Ms>, now: Ms) -> String {
    match reason {
        "refusal" => format!("{from} reached its {}{}", limit_name(window), resets(until, now)),
        "planned" => {
            format!(
                "{from} was about to reach its {} (planned move between turns)",
                limit_name(window)
            )
        }
        "threshold" => {
            format!(
                "{from} passed the share of its {} that is set for it (move between turns)",
                limit_name(window)
            )
        }
        "compaction" => {
            format!("the conversation was just compacted and {from} is close to its limits (rebalance)")
        }
        "manual" => "the operator asked for it".to_owned(),
        "all-blocked-wait" | "available" => {
            "every account was at its limit; Baton waited until one was available".to_owned()
        }
        "reblocked" => {
            "the accounts kept refusing right after resuming; Baton backed off before trying again".to_owned()
        }
        _ => "of a usage limit".to_owned(),
    }
}

/// The first prompt of a conversation resumed on another account. It names neither account.
pub fn continue_prompt(reason: &str, window: Option<Limit>, until: Option<Ms>, now: Ms) -> String {
    [
        format!(
            "[Baton] This conversation was moved to another account because {}.",
            hop_reason(reason, RAN_ON, window, until, now)
        ),
        "Carry on exactly where you stopped.".to_owned(),
        "If a workflow run was interrupted, relaunch it the same way (with resumeFromRunId, or the same script) so its finished agents replay from their saved results instead of running again.".to_owned(),
        "Background agents and background shells that were running before the move have ended; restart the ones that are still needed.".to_owned(),
    ]
    .join(" ")
}

/// When the operator chose to stay on a limited account: the prompt meets the limit and Claude Code's
/// own wait-at-limit takes over. It does not name the account.
pub fn stay_prompt(window: Option<Limit>, until: Option<Ms>, now: Ms) -> String {
    [
        format!(
            "[Baton] The account this conversation runs on reached its {}{}; the operator chose to stay on it instead of moving the conversation.",
            limit_name(window),
            resets(until, now)
        ),
        "Carry on exactly where you stopped once the limit allows.".to_owned(),
    ]
    .join(" ")
}

/// The notice before a move, with the countdown when `seconds` is given.
pub fn hop_line(
    from: &str,
    to: &str,
    reason: &str,
    window: Option<Limit>,
    until: Option<Ms>,
    now: Ms,
    seconds: Option<i64>,
) -> String {
    let why = hop_reason(reason, from, window, until, now);
    let tail = match seconds {
        Some(seconds) => {
            format!(" in {seconds} s · Enter: now · any other key: stay on {from} · Ctrl+C: quit")
        }
        None => String::new(),
    };
    format!("baton: {why}; moving this conversation to {to}{tail}")
}

/// The notice while every account is blocked.
pub fn wait_line(
    total: usize,
    to: &str,
    window: Option<Limit>,
    at: Option<Ms>,
    now: Ms,
    reason: &str,
) -> String {
    let lead = if reason == "reblocked" {
        "baton: accounts refused again right after resuming; waiting before the next try".to_owned()
    } else {
        format!("baton: all {total} accounts at limit; {to} is first back")
    };
    let what = match window {
        Some(window) => format!(", its {} window resets at {}", window.label(), clock(at, now)),
        None => format!(" at {}", clock(at, now)),
    };
    format!(
        "{lead}{what} (in {}); resuming there then · any key: stay here · Ctrl+C: quit",
        duration(at.map(|at| at - now))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{DAY, HOUR, MINUTE, local_ms};

    #[test]
    fn clocks() {
        let noon = local_ms(2026, 3, 10, 12, 0).unwrap();
        assert_eq!(clock(Some(noon + 3 * HOUR + 20 * MINUTE), noon), "15:20");
        assert_eq!(
            clock(Some(local_ms(2026, 10, 27, 9, 5).unwrap()), noon),
            "Tue, Oct 27 09:05"
        );
        assert_eq!(clock(None, noon), "an unknown time");
    }

    #[test]
    fn durations() {
        assert_eq!(duration(None), "unknown");
        assert_eq!(duration(Some(-5)), "0 s");
        assert_eq!(duration(Some(45_400)), "45 s");
        assert_eq!(duration(Some(12 * MINUTE)), "12 min");
        assert_eq!(duration(Some(2 * HOUR + 5 * MINUTE)), "2 h 5 min");
        assert_eq!(duration(Some(47 * HOUR)), "47 h 0 min");
        assert_eq!(duration(Some(3 * DAY + 4 * HOUR)), "3 d 4 h");
    }

    #[test]
    fn why_a_conversation_moved() {
        let noon = local_ms(2026, 3, 10, 12, 0).unwrap();
        assert_eq!(
            hop_reason("refusal", "Work", Some(Limit::FiveHour), Some(noon + HOUR), noon),
            "Work reached its 5-hour limit (resets 13:00)"
        );
        assert_eq!(
            hop_reason("refusal", "Work", None, None, noon),
            "Work reached its usage limit"
        );
        assert_eq!(
            hop_reason("planned", "Work", Some(Limit::SevenDay), None, noon),
            "Work was about to reach its weekly limit (planned move between turns)"
        );
        assert_eq!(
            hop_reason("manual", "Work", None, None, noon),
            "the operator asked for it"
        );
        assert_eq!(hop_reason("else", "Work", None, None, noon), "of a usage limit");
    }

    #[test]
    fn prompts_and_notices() {
        let noon = local_ms(2026, 3, 10, 12, 0).unwrap();
        let prompt = continue_prompt("manual", None, None, noon);
        assert!(prompt.starts_with(
            "[Baton] This conversation was moved to another account because the operator asked for it. Carry on exactly where you stopped."
        ));
        assert!(prompt.contains("resumeFromRunId"));
        // What the model reads says why, and names no account.
        let first = |reason: &str, window: Option<Limit>, until: Option<Ms>| {
            let prompt = continue_prompt(reason, window, until, noon);
            prompt.split_once(". ").unwrap().0.to_owned()
        };
        assert_eq!(
            first("refusal", Some(Limit::FiveHour), Some(noon + HOUR)),
            "[Baton] This conversation was moved to another account because the account it ran on reached its 5-hour limit (resets 13:00)"
        );
        assert_eq!(
            first("planned", Some(Limit::SevenDay), None),
            "[Baton] This conversation was moved to another account because the account it ran on was about to reach its weekly limit (planned move between turns)"
        );
        assert_eq!(
            first("threshold", Some(Limit::FiveHour), None),
            "[Baton] This conversation was moved to another account because the account it ran on passed the share of its 5-hour limit that is set for it (move between turns)"
        );
        assert_eq!(
            first("compaction", None, None),
            "[Baton] This conversation was moved to another account because the conversation was just compacted and the account it ran on is close to its limits (rebalance)"
        );
        assert_eq!(
            stay_prompt(Some(Limit::FiveHour), Some(noon + HOUR), noon),
            "[Baton] The account this conversation runs on reached its 5-hour limit (resets 13:00); the operator chose to stay on it instead of moving the conversation. Carry on exactly where you stopped once the limit allows."
        );
        assert_eq!(
            hop_line("Work", "Spare", "manual", None, None, noon, None),
            "baton: the operator asked for it; moving this conversation to Spare"
        );
        assert_eq!(
            hop_line(
                "Work",
                "Spare",
                "refusal",
                Some(Limit::FiveHour),
                None,
                noon,
                Some(10)
            ),
            "baton: Work reached its 5-hour limit; moving this conversation to Spare in 10 s · Enter: now · any other key: stay on Work · Ctrl+C: quit"
        );
        assert_eq!(
            wait_line(
                3,
                "Spare",
                Some(Limit::FiveHour),
                Some(noon + 2 * HOUR),
                noon,
                "all-blocked-wait"
            ),
            "baton: all 3 accounts at limit; Spare is first back, its 5-hour window resets at 14:00 (in 2 h 0 min); resuming there then · any key: stay here · Ctrl+C: quit"
        );
        assert_eq!(
            wait_line(3, "Spare", None, Some(noon + 15 * MINUTE), noon, "reblocked"),
            "baton: accounts refused again right after resuming; waiting before the next try at 12:15 (in 15 min); resuming there then · any key: stay here · Ctrl+C: quit"
        );
    }
}
