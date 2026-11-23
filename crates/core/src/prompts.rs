//! The words Baton uses: the first prompt of a resumed conversation and the one-line notices. Pure;
//! times are formatted in local time.
//!
//! The notices are the operator's, on the terminal, and name the accounts. The prompts are read by the
//! model and name none: which accounts the operator has, and what they are called, is not the model's
//! to know.

use crate::clock::Ms;
use crate::text::{span, when};
use crate::window::Limit;

/// A moment as the operator's clock shows it ([`when`]); `an unknown time` when it is not known.
fn at_time(at: Option<Ms>, now: Ms) -> String {
    match at {
        Some(at) => when(Some(at), now),
        None => "an unknown time".to_owned(),
    }
}

/// A limit by name: `5-hour limit`, `weekly limit`, `usage limit`.
fn limit_name(window: Option<Limit>) -> String {
    format!("{} limit", window.map_or("usage", Limit::label))
}

/// " (resets 15:20)" when the reset is known.
fn resets(until: Option<Ms>, now: Ms) -> String {
    match until {
        Some(until) if until != 0 => format!(" (resets {})", when(Some(until), now)),
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
            format!(
                " in {} · Enter: now · any other key: stay on {from} · Ctrl+C: quit",
                span(Some(seconds * 1000))
            )
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
        Some(window) => format!(", its {} window resets at {}", window.label(), at_time(at, now)),
        None => format!(" at {}", at_time(at, now)),
    };
    format!(
        "{lead}{what} (in {}); resuming there then · any key: stay here · Ctrl+C: quit",
        at.map_or_else(|| "an unknown time".to_owned(), |at| span(Some(at - now)))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{HOUR, MINUTE, local_ms};

    #[test]
    fn a_time_that_is_not_known_is_said_so() {
        let noon = local_ms(2026, 3, 10, 12, 0).unwrap();
        assert_eq!(at_time(Some(noon + 3 * HOUR + 20 * MINUTE), noon), "15:20");
        assert_eq!(at_time(None, noon), "an unknown time");
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
            "baton: Work reached its 5-hour limit; moving this conversation to Spare in 10s · Enter: now · any other key: stay on Work · Ctrl+C: quit"
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
            "baton: all 3 accounts at limit; Spare is first back, its 5-hour window resets at 14:00 (in 2h); resuming there then · any key: stay here · Ctrl+C: quit"
        );
        assert_eq!(
            wait_line(3, "Spare", None, Some(noon + 15 * MINUTE), noon, "reblocked"),
            "baton: accounts refused again right after resuming; waiting before the next try at 12:15 (in 15m); resuming there then · any key: stay here · Ctrl+C: quit"
        );
    }
}
