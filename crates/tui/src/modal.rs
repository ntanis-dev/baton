//! Dialogs over the view: confirm (y / n), input (a line of text: a new account's name), choice (pick
//! one: where to hop), thresholds (a small form of two numbers), the selection's actions and the keys
//! of the menu. Each holds its effect as data; `ui::overlays` draws them.

use baton_core::config::Mark;
use baton_core::json::show;
use baton_core::names::{clean_name, name_problem as text_problem, same_name};
use baton_core::text::char_width;

use crate::actions::run_action;
use crate::keys::Key;
use crate::state::{
    Choose, Effect, Modal, ModalKind, Op, Outcome, State, Submit, Tone, result, result_with, toast,
};
use crate::ui::kit::theme_of;
use crate::ui::overlays::modal_layout;

/// The dialog that lists the menu's keys, from its first line.
pub fn keys_modal() -> Modal {
    Modal {
        title: "Keys".to_owned(),
        lines: Vec::new(),
        scroll: Some(0),
        kind: ModalKind::Keys,
    }
}

/// The state with the dialog closed.
fn close(state: &State) -> State {
    let mut next = state.clone();
    next.modal = None;
    next
}

/// The state with its dialog replaced.
fn with_modal(state: &State, modal: Modal) -> State {
    let mut next = state.clone();
    next.modal = Some(modal);
    next
}

/// A line of text being edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextField {
    /// The text.
    pub value: String,
    /// The cursor, in characters.
    pub cursor: usize,
}

/// The field after a key, or None for a key that does not edit.
pub fn edit_field(field: &TextField, key: &Key) -> Option<TextField> {
    let mut chars: Vec<char> = field.value.chars().collect();
    let mut cursor = field.cursor.min(chars.len());
    match key {
        Key::Backspace if cursor > 0 => {
            chars.remove(cursor - 1);
            cursor -= 1;
        }
        Key::Delete => {
            if cursor < chars.len() {
                chars.remove(cursor);
            }
        }
        Key::Left => cursor = cursor.saturating_sub(1),
        Key::Right => cursor = (cursor + 1).min(chars.len()),
        Key::Home => cursor = 0,
        Key::End => cursor = chars.len(),
        Key::Ctrl('u') => {
            chars.drain(..cursor);
            cursor = 0;
        }
        other => {
            let typed = other.printable()?;
            chars.insert(cursor, typed);
            cursor += 1;
        }
    }
    Some(TextField {
        value: chars.into_iter().collect(),
        cursor,
    })
}

/// The field with text put in at the cursor: line breaks as spaces, control characters left out.
pub fn insert_text(field: &TextField, text: &str) -> TextField {
    let mut clean = String::new();
    let mut breaking = false;
    for c in text.chars() {
        if c == '\r' || c == '\n' {
            if !breaking {
                clean.push(' ');
            }
            breaking = true;
            continue;
        }
        breaking = false;
        if !matches!(c, '\u{00}'..='\u{08}' | '\u{0B}'..='\u{1F}' | '\u{7F}') {
            clean.push(c);
        }
    }
    let mut chars: Vec<char> = field.value.chars().collect();
    let cursor = field.cursor.min(chars.len());
    let added: Vec<char> = clean.chars().collect();
    let count = added.len();
    chars.splice(cursor..cursor, added);
    TextField {
        value: chars.into_iter().collect(),
        cursor: cursor + count,
    }
}

/// What text typed or pasted does in the open dialog, or None when the dialog takes no text.
pub fn modal_text(state: &State, text: &str) -> Option<Outcome> {
    let modal = state.modal.as_ref()?;
    let ModalKind::Input {
        value,
        cursor,
        submit,
    } = &modal.kind
    else {
        return None;
    };
    let edited = insert_text(
        &TextField {
            value: value.clone(),
            cursor: *cursor,
        },
        text,
    );
    let kind = ModalKind::Input {
        value: edited.value,
        cursor: edited.cursor,
        submit: submit.clone(),
    };
    Some(result(with_modal(
        state,
        Modal {
            kind,
            ..modal.clone()
        },
    )))
}

/// The thresholds dialog: 5-hour and weekly, each a percentage or "default" (the policy's).
fn threshold_keys(state: &State, modal: &Modal, key: &Key) -> Outcome {
    let ModalKind::Thresholds {
        account,
        account_name,
        index,
        values,
        defaults,
        typing,
    } = &modal.kind
    else {
        return result(state.clone());
    };
    let field = (*index).min(1);
    let typed = |index: usize, values: [Option<f64>; 2], typing: Option<String>| {
        let kind = ModalKind::Thresholds {
            account: account.clone(),
            account_name: account_name.clone(),
            index,
            values,
            defaults: *defaults,
            typing,
        };
        result(with_modal(
            state,
            Modal {
                kind,
                ..modal.clone()
            },
        ))
    };
    // A digit types the percentage, Backspace takes one back (of the number as it stands, when
    // nothing was typed yet); with all of it taken back the threshold follows the policy. A digit
    // that would take it past 100 is not taken.
    if *key == Key::Backspace || matches!(key, Key::Char('0'..='9')) {
        let mut text = typing.clone().unwrap_or_default();
        match key {
            Key::Char(digit) => text.push(*digit),
            _ => {
                if typing.is_none() {
                    text = show(values[field].unwrap_or(defaults[field]));
                }
                text.pop();
            }
        }
        let number = text.parse::<f64>().ok();
        if number.is_some_and(|n| n > 100.0) {
            return result(state.clone());
        }
        let mut next = *values;
        next[field] = number.map(|n| n.clamp(1.0, 100.0));
        return typed(field, next, Some(text));
    }
    let change = |index: usize, values: [Option<f64>; 2]| {
        let kind = ModalKind::Thresholds {
            account: account.clone(),
            account_name: account_name.clone(),
            index,
            values,
            defaults: *defaults,
            typing: None,
        };
        result(with_modal(
            state,
            Modal {
                kind,
                ..modal.clone()
            },
        ))
    };
    match key {
        Key::Escape => result(close(state)),
        Key::Up | Key::Down => change((field + 1) % 2, *values),
        Key::Left | Key::Right => {
            let direction = if *key == Key::Left { -1.0 } else { 1.0 };
            let current = values[field].unwrap_or(defaults[field]);
            let mut next = *values;
            next[field] = Some((current + direction).clamp(1.0, 100.0));
            change(field, next)
        }
        Key::Char('d') => {
            let mut next = *values;
            next[field] = None;
            change(field, next)
        }
        Key::Enter => {
            let name = if account_name.is_empty() {
                account
            } else {
                account_name
            };
            result_with(
                close(state),
                vec![Effect::apply(
                    Op::Thresholds {
                        id: account.clone(),
                        five_hour: values[0],
                        weekly: values[1],
                    },
                    Some(format!("Saved the thresholds of {name}.")),
                )],
            )
        }
        _ => result(state.clone()),
    }
}

/// Why a name cannot be an account's: any printable text is fine, unique among the accounts ignoring
/// case (the account being renamed aside).
fn name_problem(state: &State, value: &str, own: Option<&str>) -> Option<String> {
    if let Some(problem) = text_problem(value) {
        return Some(problem);
    }
    let taken = state
        .data
        .as_ref()?
        .config
        .accounts
        .iter()
        .find(|a| Some(a.id.as_str()) != own && same_name(&a.name, value))?;
    Some(format!("{} is the name of another account.", taken.name))
}

/// Takes an input dialog's text.
fn submit_input(state: &State, value: &str, submit: &Submit) -> Outcome {
    let value = clean_name(value);
    match submit {
        Submit::Add => {
            if let Some(problem) = name_problem(state, &value, None) {
                return result(toast(state, &problem, Tone::Error));
            }
            // The new account's sign-in follows (the runtime runs it with its id).
            result_with(close(state), vec![Effect::apply(Op::Add { name: value }, None)])
        }
        Submit::Rename(account) => {
            let current = state
                .data
                .as_ref()
                .and_then(|data| data.config.accounts.iter().find(|a| &a.id == account));
            if current.is_some_and(|current| current.name == value) {
                return result(close(state));
            }
            if let Some(problem) = name_problem(state, &value, Some(account)) {
                return result(toast(state, &problem, Tone::Error));
            }
            let old = current.map_or(account.as_str(), |current| current.name.as_str());
            let done = format!("Renamed {old} to {value}.");
            result_with(
                close(state),
                vec![Effect::apply(
                    Op::Rename {
                        id: account.clone(),
                        name: value,
                    },
                    Some(done),
                )],
            )
        }
    }
}

/// What a choice dialog's pick asks for.
fn pick(state: &State, modal: &Modal, at: usize) -> Outcome {
    let ModalKind::Choice { options, choose, .. } = &modal.kind else {
        return result(close(state));
    };
    let Some(option) = options.get(at) else {
        return result(state.clone());
    };
    let effect = match choose {
        Choose::Mark { id, done } => Effect::apply(
            Op::Mark {
                id: id.clone(),
                mark: Mark::parse(&option.value).unwrap_or(Mark::Normal),
            },
            Some(done.clone()),
        ),
        Choose::HopTo { session } => Effect::apply(
            Op::Hop {
                session: session.clone(),
                to: Some(option.value.clone()),
            },
            None,
        ),
    };
    result_with(close(state), vec![effect])
}

/// What a key does in the open dialog.
pub fn modal_keys(state: &State, key: &Key) -> Outcome {
    let Some(modal) = state.modal.as_ref() else {
        return result(state.clone());
    };
    match &modal.kind {
        ModalKind::Actions { index, options, .. } => {
            if *key == Key::Escape {
                return result(close(state));
            }
            let count = options.len().max(1);
            if matches!(key, Key::Up | Key::Down) {
                let at = if *key == Key::Up {
                    (*index + count - 1) % count
                } else {
                    (*index + 1) % count
                };
                let mut moved = modal.with_index(at);
                moved.scroll = None;
                return result(with_modal(state, moved));
            }
            if *key == Key::Enter {
                return match options.get(*index) {
                    Some(option) => run_action(state, &option.command),
                    None => result(state.clone()),
                };
            }
            let name = key.name();
            match options.iter().find(|option| option.key == name) {
                Some(direct) => run_action(state, &direct.command),
                None => result(state.clone()),
            }
        }
        ModalKind::Confirm { yes, focus, effect } => {
            let on_yes = focus.unwrap_or(*yes);
            if key.is('y') || (*key == Key::Enter && on_yes) {
                return result_with(close(state), vec![effect.clone()]);
            }
            if key.is('n') || matches!(key, Key::Escape | Key::Enter) {
                return result(close(state));
            }
            if matches!(key, Key::Left | Key::Right) {
                let kind = ModalKind::Confirm {
                    yes: *yes,
                    focus: Some(!on_yes),
                    effect: effect.clone(),
                };
                return result(with_modal(
                    state,
                    Modal {
                        kind,
                        ..modal.clone()
                    },
                ));
            }
            result(state.clone())
        }
        ModalKind::Input {
            value,
            cursor,
            submit,
        } => {
            if *key == Key::Escape {
                return result(close(state));
            }
            if *key == Key::Enter {
                return submit_input(state, value, submit);
            }
            match edit_field(
                &TextField {
                    value: value.clone(),
                    cursor: *cursor,
                },
                key,
            ) {
                Some(edited) => {
                    let kind = ModalKind::Input {
                        value: edited.value,
                        cursor: edited.cursor,
                        submit: submit.clone(),
                    };
                    result(with_modal(
                        state,
                        Modal {
                            kind,
                            ..modal.clone()
                        },
                    ))
                }
                None => result(state.clone()),
            }
        }
        ModalKind::Choice { options, index, .. } => {
            if *key == Key::Escape || key.is('q') {
                return result(close(state));
            }
            let count = options.len().max(1);
            if matches!(key, Key::Up | Key::Down) {
                let at = if *key == Key::Up {
                    (*index + count - 1) % count
                } else {
                    (*index + 1) % count
                };
                let mut moved = modal.with_index(at);
                moved.scroll = None;
                return result(with_modal(state, moved));
            }
            if let Key::Char(digit @ '1'..='9') = key {
                let number = *digit as usize - '0' as usize;
                if number <= options.len() {
                    return pick(state, modal, number - 1);
                }
            }
            if *key == Key::Enter {
                return pick(state, modal, *index);
            }
            result(state.clone())
        }
        ModalKind::Thresholds { .. } => threshold_keys(state, modal, key),
        // The arrows and the page keys scroll it; any other key closes it.
        ModalKind::Keys => {
            let (at, most, page) = modal_layout(state, &theme_of(state)).map_or((0, 0, 1), |layout| {
                (layout.offset, layout.max, layout.room.max(2) - 1)
            });
            let to = match key {
                Key::Up => at.saturating_sub(1),
                Key::Down => at + 1,
                Key::PageUp => at.saturating_sub(page),
                Key::PageDown => at + page,
                Key::Home => 0,
                Key::End => most,
                _ => return result(close(state)),
            };
            let mut moved = modal.clone();
            moved.scroll = Some(to.min(most));
            result(with_modal(state, moved))
        }
    }
}

/// The part of a field's text that shows in `room` cells, around the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldWindow {
    /// The text before the cursor.
    pub before: String,
    /// The character under the cursor; a space at the end of the text.
    pub at: String,
    /// The text after the cursor.
    pub after: String,
}

/// The part of `value` that shows in `room` cells with the cursor at character `cursor`.
pub fn field_window(value: &str, cursor: usize, room: usize) -> FieldWindow {
    let chars: Vec<char> = value.chars().collect();
    let at = cursor.min(chars.len());
    let cells = |list: &[char]| list.iter().map(|c| char_width(*c)).sum::<usize>();
    let mut start = 0;
    while cells(&chars[start..at]) + 1 > room && start < at {
        start += 1;
    }
    let mut end = at + 1;
    while end < chars.len() && cells(&chars[start..=end]) <= room {
        end += 1;
    }
    let tail_end = end.max(at + 1).min(chars.len());
    FieldWindow {
        before: chars[start..at].iter().collect(),
        at: chars.get(at).map_or_else(|| " ".to_owned(), char::to_string),
        after: chars[(at + 1).min(tail_end)..tail_end].iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(value: &str, cursor: usize) -> TextField {
        TextField {
            value: value.to_owned(),
            cursor,
        }
    }

    #[test]
    fn editing_a_line() {
        assert_eq!(
            edit_field(&field("ac", 1), &Key::Char('b')),
            Some(field("abc", 2))
        );
        assert_eq!(
            edit_field(&field("abc", 2), &Key::Backspace),
            Some(field("ac", 1))
        );
        assert_eq!(edit_field(&field("abc", 0), &Key::Backspace), None);
        assert_eq!(edit_field(&field("abc", 1), &Key::Delete), Some(field("ac", 1)));
        assert_eq!(edit_field(&field("abc", 3), &Key::Delete), Some(field("abc", 3)));
        assert_eq!(edit_field(&field("abc", 0), &Key::Left), Some(field("abc", 0)));
        assert_eq!(edit_field(&field("abc", 3), &Key::Right), Some(field("abc", 3)));
        assert_eq!(edit_field(&field("abc", 2), &Key::Home), Some(field("abc", 0)));
        assert_eq!(edit_field(&field("abc", 0), &Key::End), Some(field("abc", 3)));
        assert_eq!(
            edit_field(&field("abcd", 2), &Key::Ctrl('u')),
            Some(field("cd", 0))
        );
        assert_eq!(
            edit_field(&field("abc", 9), &Key::Char('d')),
            Some(field("abcd", 4))
        );
        assert_eq!(edit_field(&field("abc", 1), &Key::Enter), None);
        assert_eq!(
            edit_field(&field("日本", 1), &Key::Char('x')),
            Some(field("日x本", 2))
        );
    }

    #[test]
    fn pasting_into_a_line() {
        assert_eq!(insert_text(&field("ad", 1), "bc"), field("abcd", 3));
        assert_eq!(
            insert_text(&field("", 0), "one\r\ntwo\n\nthree\u{7}"),
            field("one two three", 13)
        );
        assert_eq!(insert_text(&field("x", 5), "y"), field("xy", 2));
    }

    #[test]
    fn the_part_of_a_field_that_shows() {
        let w = field_window("abc", 3, 10);
        assert_eq!(
            (w.before.as_str(), w.at.as_str(), w.after.as_str()),
            ("abc", " ", "")
        );
        let w = field_window("abcdef", 2, 10);
        assert_eq!(
            (w.before.as_str(), w.at.as_str(), w.after.as_str()),
            ("ab", "c", "def")
        );
        // The text scrolls so that the cursor stays in the room.
        let w = field_window("abcdefghij", 10, 5);
        assert_eq!(
            (w.before.as_str(), w.at.as_str(), w.after.as_str()),
            ("ghij", " ", "")
        );
        let w = field_window("abcdefghij", 0, 5);
        assert_eq!(
            (w.before.as_str(), w.at.as_str(), w.after.as_str()),
            ("", "a", "bcde")
        );
        let w = field_window("", 0, 5);
        assert_eq!(
            (w.before.as_str(), w.at.as_str(), w.after.as_str()),
            ("", " ", "")
        );
    }
}
