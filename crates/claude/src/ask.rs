//! Asking the operator about settings the accounts differ on, before a session starts.
//!
//! Full sync never writes over a setting two accounts changed in different ways: it keeps both and
//! notes a question (`baton_profile::sync`). `baton` asks those questions here, one line each: a digit
//! takes that value for every account, any other key leaves the rest for later (`baton sync` shows
//! them again), Ctrl+C quits.

use std::io::Write;
use std::time::{Duration, Instant};

use baton_core::term::{Terminal, notice};
use baton_profile::sync::{HOME, Question};
use serde_json::Value;

use crate::countdown::KeyInput;

/// How many questions are asked at one start; the rest wait for the next one.
const AT_ONCE: usize = 5;

/// Keys typed before the question could be read (leftovers of what started baton) are ignored.
const GRACE: Duration = Duration::from_millis(300);

/// The values a question's participants hold, each with who holds it, in the order they are offered.
pub type Choices = Vec<(Option<Value>, Vec<String>)>;

/// The distinct values of a question: the accounts' first, `~/.claude.json`'s alone last.
pub fn choices(question: &Question) -> Choices {
    let mut out: Choices = Vec::new();
    let accounts_first = question.values.iter().filter(|held| held.who != HOME);
    for held in accounts_first.chain(question.values.iter().filter(|held| held.who == HOME)) {
        match out.iter_mut().find(|(value, _)| *value == held.value) {
            Some((_, holders)) => holders.push(held.who.clone()),
            None => out.push((held.value.clone(), vec![held.who.clone()])),
        }
    }
    out
}

/// A value in a few words: text in quotes, a map as `{...}`, nothing as `nothing`.
pub fn value_words(value: Option<&Value>) -> String {
    let text = match value {
        None => return "nothing".to_owned(),
        Some(Value::String(text)) => format!("\"{text}\""),
        Some(Value::Object(_)) => "{...}".to_owned(),
        Some(other) => other.to_string(),
    };
    if text.chars().count() > 40 {
        format!("{}...", text.chars().take(37).collect::<String>())
    } else {
        text
    }
}

/// The one line a question is asked with.
pub fn question_line(question: &Question, choices: &Choices, label: &dyn Fn(&str) -> String) -> String {
    let offered: Vec<String> = choices
        .iter()
        .enumerate()
        .map(|(at, (value, holders))| {
            let who: Vec<String> = holders.iter().map(|who| label(who)).collect();
            format!("{}: {} ({})", at + 1, value_words(value.as_ref()), who.join(", "))
        })
        .collect();
    format!(
        "baton: the accounts differ on {} · {} · any other key: later · Ctrl+C: quit",
        question.label,
        offered.join(" · ")
    )
}

/// What the operator answered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answers {
    /// For each question answered: its leaf, and whose value every account gets.
    pub taken: Vec<(String, String)>,
    /// Whether the operator pressed Ctrl+C.
    pub quit: bool,
}

/// What the asking needs beside the questions.
pub struct Asking<'a> {
    /// Where keys come from.
    pub input: &'a mut dyn KeyInput,
    /// Where the questions are written.
    pub output: &'a mut dyn Write,
    /// How they are drawn.
    pub term: &'a Terminal,
    /// The name of a participant: an account's, or `~/.claude.json`.
    pub label: &'a dyn Fn(&str) -> String,
    /// How long one question waits for a key before it is left for later.
    pub wait: Duration,
}

impl std::fmt::Debug for Asking<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Asking")
            .field("wait", &self.wait)
            .finish_non_exhaustive()
    }
}

/// Asks the questions one by one. The first key that is not one of the digits offered leaves that
/// question and the ones after it for later.
pub fn ask(questions: &[Question], asking: Asking<'_>) -> Answers {
    let Asking {
        input,
        output,
        term,
        label,
        wait,
    } = asking;
    let mut answers = Answers::default();
    input.begin();
    for question in questions.iter().take(AT_ONCE) {
        let offered = choices(question);
        if offered.len() < 2 {
            continue;
        }
        let _ = writeln!(
            output,
            "{}",
            notice(&question_line(question, &offered, label), term)
        );
        let _ = output.flush();
        let started = Instant::now();
        let key = loop {
            let left = wait.saturating_sub(started.elapsed());
            if left.is_zero() {
                break None;
            }
            match input.read(left.min(Duration::from_millis(200))) {
                Some(_) if started.elapsed() < GRACE => {}
                Some(bytes) => break bytes.first().copied(),
                None => {}
            }
        };
        let picked = key
            .filter(u8::is_ascii_digit)
            .and_then(|digit| usize::from(digit - b'0').checked_sub(1))
            .and_then(|at| offered.get(at));
        match (key, picked) {
            (Some(3), _) => {
                answers.quit = true;
                break;
            }
            (_, Some((_, holders))) => {
                if let Some(who) = holders.first() {
                    answers.taken.push((question.leaf.clone(), who.clone()));
                }
            }
            _ => break,
        }
    }
    input.end();
    if !answers.quit && questions.len() > AT_ONCE && answers.taken.len() == AT_ONCE {
        let more = questions.len() - AT_ONCE;
        let _ = writeln!(
            output,
            "{}",
            notice(&format!("baton: {more} more to decide: baton sync"), term)
        );
    }
    answers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::countdown::channel_keys;
    use baton_core::term::make_terminal;
    use baton_core::theme::GlyphSet;
    use baton_profile::sync::Held;
    use serde_json::json;

    fn question(leaf: &str, values: &[(&str, Option<Value>)]) -> Question {
        Question {
            leaf: leaf.to_owned(),
            label: leaf.to_owned(),
            values: values
                .iter()
                .map(|(who, value)| Held {
                    who: (*who).to_owned(),
                    value: value.clone(),
                })
                .collect(),
            at: None,
        }
    }

    fn label(who: &str) -> String {
        if who == HOME {
            "~/.claude.json".to_owned()
        } else {
            who.to_uppercase()
        }
    }

    /// Asks with these keys typed after the grace time, each a little after the other.
    fn asked(questions: &[Question], keys: &[&[u8]]) -> (Answers, String) {
        let (typed, mut input) = channel_keys();
        let keys: Vec<Vec<u8>> = keys.iter().map(|key| key.to_vec()).collect();
        let typist = std::thread::spawn(move || {
            for key in keys {
                std::thread::sleep(GRACE + Duration::from_millis(120));
                if typed.send(key).is_err() {
                    break;
                }
            }
        });
        let mut output = Vec::new();
        let term = make_terminal(false, 1, GlyphSet::Rich, 100);
        let answers = ask(
            questions,
            Asking {
                input: &mut input,
                output: &mut output,
                term: &term,
                label: &label,
                wait: Duration::from_secs(5),
            },
        );
        typist.join().unwrap();
        (answers, String::from_utf8(output).unwrap())
    }

    #[test]
    fn a_question_offers_each_value_once_with_who_holds_it() {
        let theme = question(
            "theme",
            &[
                ("~", Some(json!("dark"))),
                ("a", Some(json!("light"))),
                ("b", Some(json!("dark"))),
                ("c", None),
            ],
        );
        let offered = choices(&theme);
        assert_eq!(
            offered,
            vec![
                (Some(json!("light")), vec!["a".to_owned()]),
                (Some(json!("dark")), vec!["b".to_owned(), "~".to_owned()]),
                (None, vec!["c".to_owned()]),
            ]
        );
        assert_eq!(
            question_line(&theme, &offered, &label),
            "baton: the accounts differ on theme · 1: \"light\" (A) · 2: \"dark\" (B, ~/.claude.json) · 3: nothing (C) · any other key: later · Ctrl+C: quit"
        );
        assert_eq!(value_words(Some(&json!({ "command": "gh" }))), "{...}");
        assert_eq!(value_words(Some(&json!(true))), "true");
        assert_eq!(value_words(Some(&json!("x".repeat(60)))).chars().count(), 40);
    }

    #[test]
    fn a_digit_answers_and_any_other_key_leaves_the_rest_for_later() {
        let questions = vec![
            question(
                "theme",
                &[("a", Some(json!("light"))), ("b", Some(json!("dark")))],
            ),
            question(
                "editorMode",
                &[("a", Some(json!("vim"))), ("b", Some(json!("normal")))],
            ),
            question("verbose", &[("a", Some(json!(true))), ("b", Some(json!(false)))]),
        ];
        let (answers, said) = asked(&questions, &[b"2", b"1", b"x"]);
        assert_eq!(
            answers,
            Answers {
                taken: vec![
                    ("theme".to_owned(), "b".to_owned()),
                    ("editorMode".to_owned(), "a".to_owned())
                ],
                quit: false
            }
        );
        assert_eq!(said.lines().count(), 3);
        // A digit that is not offered is any other key.
        let (answers, said) = asked(&questions, &[b"7"]);
        assert!(answers.taken.is_empty() && !answers.quit);
        assert_eq!(said.lines().count(), 1);
        // Ctrl+C quits.
        let (answers, _) = asked(&questions, &[b"1", &[3]]);
        assert_eq!((answers.taken.len(), answers.quit), (1, true));
    }

    #[test]
    fn nobody_at_the_keyboard_leaves_everything_for_later() {
        let questions = vec![question(
            "theme",
            &[("a", Some(json!("light"))), ("b", Some(json!("dark")))],
        )];
        let (_typed, mut input) = channel_keys();
        let mut output = Vec::new();
        let term = make_terminal(false, 1, GlyphSet::Rich, 100);
        let answers = ask(
            &questions,
            Asking {
                input: &mut input,
                output: &mut output,
                term: &term,
                label: &label,
                wait: Duration::from_millis(50),
            },
        );
        assert_eq!(answers, Answers::default());
    }
}
