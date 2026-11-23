//! The policy settings as a form: Up and Down pick a field, Left and Right change it (PgUp and PgDn by
//! ten steps), digits type a number into it, Enter asks to confirm, Esc leaves without saving. Each
//! field has a one-line hint that never wraps: its short form, or none, when the line has no room for
//! it. The form scrolls with the keyboard where the pane is shorter than it.

use baton_core::config::Policy;
use baton_core::json::{num, show};
use baton_core::lines::{Line, line_width, p, s};
use baton_core::text::{round, width as cells};
use baton_core::theme::Glyphs;
use serde_json::{Value, json};

use super::common::{pad_end, pad_start, section_rule};
use crate::keys::Key;
use crate::pane::{Item, ViewModel, body, pane_box};
use crate::state::{Effect, Focus, Modal, ModalKind, Op, Outcome, State, Tone, result, toast};

/// What kind of value a field takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FieldKind {
    /// One of a few words.
    Choice(&'static [&'static str]),
    /// A number in a range, changed in steps, with its unit.
    Number {
        /// The smallest it may be.
        min: f64,
        /// The largest it may be.
        max: f64,
        /// How much Left and Right change it.
        step: f64,
        /// Its unit, written right after it: `%`, `x`, `m` for minutes, `d` for days, or none.
        unit: &'static str,
    },
    /// On or off.
    Boolean,
}

/// A setting of the form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    /// The setting's name in the policy.
    pub key: &'static str,
    /// What the form calls it.
    pub label: &'static str,
    /// What kind of value it takes.
    pub kind: FieldKind,
    /// Its hint.
    pub help: &'static str,
    /// Its hint when room is short.
    pub short: &'static str,
}

/// A percentage from `min` to 100 in steps of `step`.
const fn percent(min: f64, step: f64) -> FieldKind {
    FieldKind::Number {
        min,
        max: 100.0,
        step,
        unit: "%",
    }
}

/// Minutes from 0 to `max` in steps of five.
const fn minutes(max: f64) -> FieldKind {
    FieldKind::Number {
        min: 0.0,
        max,
        step: 5.0,
        unit: "m",
    }
}

/// Days from `min` to `max` in steps of `step`.
const fn days(min: f64, max: f64, step: f64) -> FieldKind {
    FieldKind::Number {
        min,
        max,
        step,
        unit: "d",
    }
}

/// The form's fields, in order.
pub const FIELDS: [Field; 15] = [
    Field {
        key: "mode",
        label: "Mode",
        kind: FieldKind::Choice(&["smart", "order"]),
        help: "smart picks by room and urgency; order fills one then next",
        short: "how accounts are picked",
    },
    Field {
        key: "fiveHour",
        label: "5-hour limit",
        kind: percent(1.0, 1.0),
        help: "an account at or above it is blocked (default for all)",
        short: "blocked at or above",
    },
    Field {
        key: "weekly",
        label: "Weekly limit",
        kind: percent(1.0, 1.0),
        help: "an account at or above it is blocked (default for all)",
        short: "blocked at or above",
    },
    Field {
        key: "startReserve",
        label: "Start reserve",
        kind: percent(0.0, 5.0),
        help: "5-hour room an account needs to take new work",
        short: "room for new work",
    },
    Field {
        key: "planFactor",
        label: "Plan factor",
        kind: FieldKind::Number {
            min: 0.0,
            max: 10.0,
            step: 0.1,
            unit: "x",
        },
        help: "hop between turns when room < factor x next turn",
        short: "x the next turn",
    },
    Field {
        key: "waitInPlaceMinutes",
        label: "Wait in place",
        kind: minutes(600.0),
        help: "a 5-hour refusal ending this soon waits instead",
        short: "wait if sooner",
    },
    Field {
        key: "minStayMinutes",
        label: "Min stay",
        kind: minutes(600.0),
        help: "no planned hop, nor a hop back, sooner after arriving",
        short: "no ping-pong",
    },
    Field {
        key: "compactionRebalance",
        label: "Rebalance",
        kind: FieldKind::Boolean,
        help: "rebalance at the cheap moment after a compaction",
        short: "after compaction",
    },
    Field {
        key: "usagePollMinutes",
        label: "Usage poll",
        kind: minutes(1440.0),
        help: "ask every account's usage this often (0: on request)",
        short: "every",
    },
    Field {
        key: "statusLineUsage",
        label: "Bar usage",
        kind: FieldKind::Boolean,
        help: "add Baton's usage after your own status line",
        short: "after yours",
    },
    Field {
        key: "fullSync",
        label: "Full sync",
        kind: FieldKind::Boolean,
        help: "keep what you set up the same on every account",
        short: "every account",
    },
    Field {
        key: "historyDays",
        label: "History",
        kind: days(0.0, 3650.0, 30.0),
        help: "months of history that ended longer ago go (0: none)",
        short: "kept (0: all)",
    },
    Field {
        key: "sessionDays",
        label: "Sessions",
        kind: days(1.0, 365.0, 1.0),
        help: "records of conversations not seen this long go",
        short: "kept unseen",
    },
    Field {
        key: "logDays",
        label: "Logs",
        kind: days(0.0, 365.0, 1.0),
        help: "wrappers' logs not written this long go (0: none)",
        short: "kept (0: all)",
    },
    Field {
        key: "backupsKept",
        label: "Backups",
        kind: FieldKind::Number {
            min: 0.0,
            max: 1000.0,
            step: 5.0,
            unit: "",
        },
        help: "the newest kept, the older ones go (0: none go)",
        short: "kept (0: all)",
    },
];

/// The row of the Apply button: the one after the last field.
const APPLY_ROW: usize = FIELDS.len();

/// The first field of the Storage group: how long Baton keeps its own records. The fields before it
/// are how accounts are picked and conversations moved.
const STORAGE_FROM: usize = 11;

/// The line of the form a row is on (a field, or the Apply button): the Storage group has a blank
/// line and its title above it, and the button a blank line.
pub fn line_of(row: usize) -> usize {
    if row < STORAGE_FROM {
        row
    } else if row < APPLY_ROW {
        row + 2
    } else {
        APPLY_ROW + 3
    }
}

/// The row a line of the form holds: a field, or the Apply button; None for the lines between.
pub fn row_at(line: usize) -> Option<usize> {
    (0..=APPLY_ROW).find(|row| line_of(*row) == line)
}

/// The policy form while it is open.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    /// The settings as they were when the form opened, one per field.
    pub original: Vec<Value>,
    /// The settings as the form has them now, one per field.
    pub values: Vec<Value>,
    /// The row the keyboard is on: a field, or the Apply button after the last one.
    pub index: usize,
    /// What was typed into the number field the keyboard is on; None when nothing was, and empty
    /// when all of it was taken back.
    pub typing: Option<String>,
}

impl Form {
    /// The form's value of the setting `key`.
    pub fn value(&self, key: &str) -> Option<&Value> {
        FIELDS
            .iter()
            .position(|f| f.key == key)
            .and_then(|at| self.values.get(at))
    }
}

/// A form over a policy's settings.
pub fn start_form(policy: &Policy) -> Form {
    let values: Vec<Value> = FIELDS
        .iter()
        .map(|f| policy.get(f.key).unwrap_or(Value::Null))
        .collect();
    Form {
        original: values.clone(),
        values,
        index: 0,
        typing: None,
    }
}

/// A number to two decimals.
fn round2(n: f64) -> f64 {
    round(n * 100.0) / 100.0
}

/// A field's value as the form shows it.
pub fn show_value(field: &Field, value: &Value) -> String {
    match field.kind {
        FieldKind::Boolean => if value.as_bool() == Some(true) {
            "on"
        } else {
            "off"
        }
        .to_owned(),
        FieldKind::Number { unit, .. } => {
            let n = show(round2(value.as_f64().unwrap_or(0.0)));
            format!("{n}{unit}")
        }
        FieldKind::Choice(_) => value.as_str().unwrap_or_default().to_owned(),
    }
}

/// The settings the form changed, by name, in the form's order.
pub fn changes(form: &Form) -> Vec<(String, Value)> {
    let defaults = Policy::default();
    FIELDS
        .iter()
        .enumerate()
        .filter(|(at, _)| form.values.get(*at) != form.original.get(*at))
        .filter_map(|(at, f)| {
            let value = form.values.get(at)?.clone();
            // A setting put back to its default is no longer set at all, as `default` on the command
            // line leaves it: it follows the default from then on, also when that changes.
            let unset = defaults.get(f.key).as_ref() == Some(&value);
            Some((f.key.to_owned(), if unset { Value::Null } else { value }))
        })
        .collect()
}

/// A field's control: arrows around a number, a dot and a word, or the choices with the chosen one in
/// brackets.
fn control(field: &Field, value: &Value, changed: bool, typing: Option<&str>, g: &Glyphs) -> Line {
    let tone = if changed { "warn bold" } else { "bold" };
    match field.kind {
        // What is being typed shows as it was typed, underlined: its value is settled (kept in the
        // field's range) all along, and shows when the keyboard leaves the field.
        FieldKind::Number { unit, .. } if typing.is_some() => vec![
            s(format!("{} ", g.left), "muted"),
            s(
                pad_start(&format!("{}{unit}", typing.unwrap_or_default()), 6),
                "warn bold underline",
            ),
            s(format!(" {}", g.right), "muted"),
        ],
        FieldKind::Number { .. } => vec![
            s(format!("{} ", g.left), "muted"),
            s(pad_start(&show_value(field, value), 6), tone),
            s(format!(" {}", g.right), "muted"),
        ],
        FieldKind::Boolean => {
            let on = value.as_bool() == Some(true);
            vec![
                s(
                    format!("{} ", if on { g.dot } else { g.ring }),
                    if on { "ok" } else { "muted" },
                ),
                s(show_value(field, value), tone),
            ]
        }
        FieldKind::Choice(choices) => {
            let mut line = Vec::new();
            for (i, choice) in choices.iter().enumerate() {
                if i > 0 {
                    line.push(p(" "));
                }
                let chosen = value.as_str() == Some(*choice);
                line.push(s(
                    if chosen {
                        format!("[{choice}]")
                    } else {
                        format!(" {choice} ")
                    },
                    if chosen { tone } else { "muted" },
                ));
            }
            line
        }
    }
}

/// The form as lines.
pub fn form_lines(form: &Form, width: usize, g: &Glyphs, focused: bool) -> Vec<Line> {
    let changed = changes(form);
    let count = changed.len();
    let is_changed = |key: &str| changed.iter().any(|(name, _)| name == key);
    let mut lines: Vec<Line> = Vec::new();
    let label_width = FIELDS.iter().map(|f| cells(f.label)).max().unwrap_or(0) + 2;
    let controls: Vec<Line> = FIELDS
        .iter()
        .enumerate()
        .map(|(at, f)| {
            control(
                f,
                form.values.get(at).unwrap_or(&Value::Null),
                is_changed(f.key),
                form.typing.as_deref().filter(|_| at == form.index),
                g,
            )
        })
        .collect();
    let control_width = controls.iter().map(|line| line_width(line)).max().unwrap_or(0) + 2;
    for (i, f) in FIELDS.iter().enumerate() {
        if i == STORAGE_FROM {
            lines.push(Vec::new());
            lines.push(section_rule("Storage", width, g));
        }
        let at = i == form.index;
        let mut line = vec![
            s(
                if at {
                    format!("{} ", g.arrow)
                } else {
                    "  ".to_owned()
                },
                "accent bold",
            ),
            s(
                pad_end(f.label, label_width),
                if at && focused { "selected bold" } else { "bold" },
            ),
        ];
        line.extend(controls[i].iter().cloned());
        line.push(p(
            " ".repeat(control_width.saturating_sub(line_width(&controls[i])).max(2))
        ));
        let room = width.saturating_sub(line_width(&line));
        if let Some(hint) = [f.help, f.short]
            .into_iter()
            .find(|hint| !hint.is_empty() && cells(hint) <= room)
        {
            line.push(s(hint, "muted"));
        }
        lines.push(line);
    }
    lines.push(Vec::new());
    let at_apply = form.index == APPLY_ROW;
    let apply = if count > 0 {
        format!(" Apply {count} change{} ", if count == 1 { "" } else { "s" })
    } else {
        " Apply ".to_owned()
    };
    lines.push(vec![
        s(
            if at_apply {
                format!("{} ", g.arrow)
            } else {
                "  ".to_owned()
            },
            "accent bold",
        ),
        s(
            apply,
            if at_apply && focused {
                "selected bold"
            } else if count > 0 {
                "accent bold"
            } else {
                "muted"
            },
        ),
        s(
            if count > 0 {
                "  asks to confirm first"
            } else {
                "  nothing changed yet"
            },
            "muted",
        ),
    ]);
    let defaults = Policy::default();
    let differ = FIELDS
        .iter()
        .enumerate()
        .filter(|(at, f)| form.values.get(*at) != defaults.get(f.key).as_ref())
        .count();
    lines.push(Vec::new());
    lines.push(vec![s(
        if differ > 0 {
            format!("{differ} differ from the defaults (d resets the field)")
        } else {
            "all at their defaults".to_owned()
        },
        "muted",
    )]);
    lines
}

/// A field's value one step on.
fn step(field: &Field, value: &Value, direction: i32) -> Value {
    match field.kind {
        FieldKind::Number { min, max, step, .. } => {
            let moved = value.as_f64().unwrap_or(0.0) + f64::from(direction) * step;
            num(round2(moved.clamp(min, max)))
        }
        FieldKind::Boolean => json!(value.as_bool() != Some(true)),
        FieldKind::Choice(choices) => {
            let count = choices.len() as i32;
            let at = choices
                .iter()
                .position(|c| value.as_str() == Some(*c))
                .map_or(-1, |at| at as i32);
            json!(choices[(at + direction).rem_euclid(count) as usize])
        }
    }
}

/// The longest a typed number is.
const TYPED_MOST: usize = 7;

/// What typing `key` into a number field makes of what was typed before: a digit more, a decimal
/// point where the field steps by less than one, or with Backspace a character fewer (of the number
/// as it stands, when nothing was typed yet). None for a key that types nothing; a digit that would
/// take the number past the field's largest, or past two decimals, leaves what was typed as it was.
fn typed(field: &Field, value: &Value, typing: Option<&str>, key: &Key) -> Option<String> {
    let FieldKind::Number { max, step, .. } = field.kind else {
        return None;
    };
    let before = typing.unwrap_or_default();
    let mut text = before.to_owned();
    match key {
        Key::Backspace => {
            if typing.is_none() {
                text = show(round2(value.as_f64().unwrap_or(0.0)));
            }
            text.pop();
        }
        Key::Char(digit @ '0'..='9') => text.push(*digit),
        Key::Char('.') if step < 1.0 && !text.contains('.') => {
            if text.is_empty() {
                text.push('0');
            }
            text.push('.');
        }
        _ => return None,
    }
    let decimals = text.split_once('.').map_or(0, |(_, decimals)| decimals.len());
    let past = text.parse::<f64>().is_ok_and(|n| n > max);
    Some(if past || decimals > 2 || text.len() > TYPED_MOST {
        before.to_owned()
    } else {
        text
    })
}

/// The value typed text stands for in a field, kept in the field's range; None while it is no number
/// (all of it was taken back).
fn typed_value(field: &Field, text: &str) -> Option<Value> {
    let FieldKind::Number { min, max, .. } = field.kind else {
        return None;
    };
    let n = text.parse::<f64>().ok().filter(|n| n.is_finite())?;
    Some(num(round2(n.clamp(min, max))))
}

/// The state with the form's field `at` set to `value`, and nothing typed.
fn with_value(state: &State, at: usize, value: Value) -> State {
    let mut next = state.clone();
    if let Some(form) = next.form.as_mut() {
        form.typing = None;
        if let Some(slot) = form.values.get_mut(at) {
            *slot = value;
        }
    }
    next
}

/// The state with the form's row `row` under the keyboard and in view: the pane scrolls as little as
/// that takes, to the top for the first row and to the end for the button.
pub fn with_row(state: &State, row: usize) -> State {
    let mut next = state.clone();
    let Some(form) = next.form.as_mut() else {
        return next;
    };
    form.index = row.min(APPLY_ROW);
    form.typing = None;
    let row = form.index;
    let room = pane_box(state).room.rows.max(1);
    // Under the button: a blank line and how many settings differ from the defaults.
    let total = line_of(APPLY_ROW) + 3;
    let most = total.saturating_sub(room);
    let key = Item::Policy.key();
    let line = line_of(row);
    let top = state.views.get(&key).copied().unwrap_or(0).min(most);
    let top = if row == APPLY_ROW {
        most
    } else if line < top {
        // The group's title comes along with its first field.
        line.saturating_sub(if row == STORAGE_FROM { 2 } else { 0 })
    } else if line >= top + room {
        line + 1 - room
    } else {
        top
    };
    next.views.insert(key, if row == 0 { 0 } else { top.min(most) });
    next
}

/// Asks to confirm the form's changes; says so when there are none.
pub fn confirm_apply(state: &State) -> Outcome {
    let Some(form) = &state.form else {
        return result(state.clone());
    };
    let changed = changes(form);
    if changed.is_empty() {
        return result(toast(state, "Nothing changed.", Tone::Info));
    }
    let lines = FIELDS
        .iter()
        .enumerate()
        .filter_map(|(at, f)| {
            let (_, value) = changed.iter().find(|(key, _)| key == f.key)?;
            Some(format!(
                "{}: {} \u{2192} {}",
                f.label,
                show_value(f, form.original.get(at).unwrap_or(&Value::Null)),
                show_value(f, value)
            ))
        })
        .collect();
    let mut next = state.clone();
    next.modal = Some(Modal {
        title: "Apply these policy settings?".to_owned(),
        lines,
        scroll: None,
        kind: ModalKind::Confirm {
            yes: true,
            focus: None,
            effect: Effect::apply(
                Op::Policy { changes: changed },
                Some("Saved the policy.".to_owned()),
            ),
        },
    });
    result(next)
}

/// The state with the form closed; says so when changes are left behind.
pub fn close_form(state: &State) -> State {
    let Some(form) = &state.form else {
        return state.clone();
    };
    let dirty = !changes(form).is_empty();
    let mut next = state.clone();
    next.form = None;
    if dirty {
        toast(&next, "Policy left unchanged.", Tone::Info)
    } else {
        next
    }
}

/// What a key does in the open form, or None for a key the form does not use.
pub fn form_keys(state: &State, key: &Key) -> Option<Outcome> {
    let form = state.form.as_ref()?;
    let row = match key {
        Key::Up => Some(form.index.saturating_sub(1)),
        Key::Down => Some((form.index + 1).min(APPLY_ROW)),
        Key::Home => Some(0),
        Key::End => Some(APPLY_ROW),
        _ => None,
    };
    if let Some(row) = row {
        return Some(result(with_row(state, row)));
    }
    if *key == Key::Escape {
        let mut next = close_form(state);
        next.focus = Focus::Side;
        return Some(result(next));
    }
    if *key == Key::Enter {
        return Some(confirm_apply(state));
    }
    let Some(field) = FIELDS.get(form.index) else {
        return (*key == Key::Backspace || matches!(key, Key::Char('0'..='9')))
            .then(|| result(state.clone()));
    };
    let value = form.values.get(form.index).cloned().unwrap_or(Value::Null);
    let number = matches!(field.kind, FieldKind::Number { .. });
    if let Some(text) = typed(field, &value, form.typing.as_deref(), key) {
        // Nothing left of what was typed: the setting is as it was when the form opened.
        let settled = typed_value(field, &text)
            .or_else(|| form.original.get(form.index).cloned())
            .unwrap_or(value);
        let mut next = with_value(state, form.index, settled);
        if let Some(form) = next.form.as_mut() {
            form.typing = Some(text);
        }
        return Some(result(next));
    }
    match key {
        Key::Left => Some(result(with_value(state, form.index, step(field, &value, -1)))),
        Key::Right => Some(result(with_value(state, form.index, step(field, &value, 1)))),
        // Ten steps at once, where a number has far to go.
        Key::PageUp if number => Some(result(with_value(state, form.index, step(field, &value, 10)))),
        Key::PageDown if number => Some(result(with_value(state, form.index, step(field, &value, -10)))),
        Key::Char('d') => {
            let default = Policy::default().get(field.key).unwrap_or(Value::Null);
            Some(result(with_value(state, form.index, default)))
        }
        // A digit on a setting that is no number does nothing, here as anywhere.
        Key::Backspace | Key::Char('0'..='9') => Some(result(state.clone())),
        _ => None,
    }
}

/// The Policy's pane.
pub fn model(state: &State, width: usize) -> ViewModel {
    let Some(data) = state.data.as_deref() else {
        return ViewModel::default();
    };
    let fresh;
    let form = match &state.form {
        Some(form) => form,
        None => {
            fresh = start_form(&data.policy);
            &fresh
        }
    };
    let unsaved = changes(form).len();
    ViewModel {
        title: vec![s("Policy", "bold")],
        right: if unsaved > 0 {
            vec![s(format!("{unsaved} unsaved"), "warn bold")]
        } else {
            vec![s(data.policy.mode.as_str(), "muted")]
        },
        body: body(form_lines(
            form,
            width,
            state.glyphs.glyphs(),
            state.focus == Focus::Pane && state.form.is_some(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::lines::plain;
    use baton_core::text::width;
    use baton_core::theme::RICH;

    #[test]
    fn values_as_the_form_shows_them() {
        let form = start_form(&Policy::default());
        let shown: Vec<String> = FIELDS
            .iter()
            .zip(&form.values)
            .map(|(f, v)| show_value(f, v))
            .collect();
        assert_eq!(
            shown,
            [
                "smart", "90%", "95%", "20%", "1.5x", "20m", "30m", "on", "30m", "off", "on", "180d", "30d",
                "14d", "50"
            ]
        );
        assert!(changes(&form).is_empty());
        assert_eq!(form.value("planFactor"), Some(&json!(1.5)));
    }

    #[test]
    fn stepping_a_field() {
        assert_eq!(step(&FIELDS[4], &json!(1.5), 1), json!(1.6));
        assert_eq!(step(&FIELDS[4], &json!(0.05), -1), json!(0));
        assert_eq!(step(&FIELDS[1], &json!(100), 1), json!(100));
        assert_eq!(step(&FIELDS[7], &json!(true), 1), json!(false));
        assert_eq!(step(&FIELDS[0], &json!("smart"), -1), json!("order"));
        assert_eq!(step(&FIELDS[0], &json!("order"), 1), json!("smart"));
    }

    #[test]
    fn a_setting_put_back_to_its_default_is_no_longer_set() {
        // The operator had the 5-hour limit at 80 and the mode on order.
        let set = Policy {
            five_hour: 80.0,
            mode: baton_core::config::Mode::Order,
            ..Policy::default()
        };
        let mut form = start_form(&set);
        form.values[1] = json!(90);
        form.values[0] = json!("smart");
        form.values[2] = json!(96);
        assert_eq!(
            changes(&form),
            [
                ("mode".to_owned(), Value::Null),
                ("fiveHour".to_owned(), Value::Null),
                ("weekly".to_owned(), json!(96)),
            ]
        );
    }

    #[test]
    fn the_form_as_lines() {
        let mut form = start_form(&Policy::default());
        form.values[1] = json!(91);
        let lines: Vec<String> = form_lines(&form, 68, &RICH, true)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(lines.len(), line_of(APPLY_ROW) + 3);
        assert_eq!(
            lines[0],
            "› Mode           [smart]  order   how accounts are picked"
        );
        assert_eq!(lines[1], "  5-hour limit   ←    91% →       blocked at or above");
        assert_eq!(lines[7], "  Rebalance      ● on             after compaction");
        assert_eq!(lines[10], "  Full sync      ● on             every account");
        // The storage has a title of its own, a blank line under the settings before it.
        assert_eq!(lines[11], "");
        assert!(lines[12].starts_with("Storage  "), "{}", lines[12]);
        assert_eq!(width(&lines[12]), 68);
        assert_eq!(lines[13], "  History        ←   180d →       kept (0: all)");
        assert_eq!(lines[16], "  Backups        ←     50 →       kept (0: all)");
        assert_eq!(lines[18], "   Apply 1 change   asks to confirm first");
        assert_eq!(lines[20], "1 differ from the defaults (d resets the field)");
        // What is typed shows as it was typed.
        form.index = 1;
        form.typing = Some("07".to_owned());
        assert_eq!(
            plain(&form_lines(&form, 68, &RICH, true)[1]),
            "› 5-hour limit   ←    07% →       blocked at or above"
        );
        let wide: Vec<String> = form_lines(&start_form(&Policy::default()), 100, &RICH, false)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(
            wide[0],
            "› Mode           [smart]  order   smart picks by room and urgency; order fills one then next"
        );
        assert_eq!(wide[18], "   Apply   nothing changed yet");
        assert_eq!(wide[20], "all at their defaults");
    }

    #[test]
    fn the_rows_of_the_form_and_the_lines_they_are_on() {
        assert_eq!(FIELDS[STORAGE_FROM].key, "historyDays");
        assert_eq!((line_of(0), line_of(STORAGE_FROM - 1)), (0, STORAGE_FROM - 1));
        assert_eq!(line_of(STORAGE_FROM), STORAGE_FROM + 2);
        assert_eq!(line_of(APPLY_ROW), FIELDS.len() + 3);
        for row in 0..=APPLY_ROW {
            assert_eq!(row_at(line_of(row)), Some(row));
        }
        // The lines between hold no row.
        assert_eq!(row_at(STORAGE_FROM), None);
        assert_eq!(row_at(STORAGE_FROM + 1), None);
        assert_eq!(row_at(line_of(APPLY_ROW) - 1), None);
        assert_eq!(row_at(line_of(APPLY_ROW) + 1), None);
        // Every setting of the form is one the policy has, in its range.
        for field in FIELDS {
            assert!(Policy::default().get(field.key).is_some(), "{}", field.key);
        }
    }

    #[test]
    fn typing_a_number_into_a_field() {
        let key = |c: char| Key::Char(c);
        let five = &FIELDS[1];
        let some = |text: &str| Some(text.to_owned());
        assert_eq!(typed(five, &json!(90), None, &key('4')), some("4"));
        assert_eq!(typed(five, &json!(4), Some("4"), &key('2')), some("42"));
        assert_eq!(typed(five, &json!(10), Some("10"), &key('0')), some("100"));
        // Past the largest: left as it was.
        assert_eq!(typed(five, &json!(42), Some("42"), &key('0')), some("42"));
        // Backspace: of what was typed, else of the number as it stands.
        assert_eq!(typed(five, &json!(90), None, &Key::Backspace), some("9"));
        assert_eq!(typed(five, &json!(9), Some("9"), &Key::Backspace), some(""));
        assert_eq!(typed(five, &json!(90), Some(""), &Key::Backspace), some(""));
        // A decimal point only where the setting steps by less than one, and two decimals at most.
        assert_eq!(typed(five, &json!(9), Some("9"), &key('.')), None);
        let factor = &FIELDS[4];
        assert_eq!(typed(factor, &json!(1.5), None, &key('.')), some("0."));
        assert_eq!(typed(factor, &json!(2), Some("2"), &key('.')), some("2."));
        assert_eq!(typed(factor, &json!(2), Some("2."), &key('.')), None);
        assert_eq!(typed(factor, &json!(2.25), Some("2.25"), &key('5')), some("2.25"));
        // Other keys, and other kinds of setting, type nothing.
        assert_eq!(typed(five, &json!(90), None, &key('x')), None);
        assert_eq!(typed(&FIELDS[0], &json!("smart"), None, &key('1')), None);
        assert_eq!(typed(&FIELDS[7], &json!(true), None, &Key::Backspace), None);
        // What the typed text stands for stays in the field's range.
        assert_eq!(typed_value(five, "42"), Some(json!(42)));
        assert_eq!(typed_value(five, "0"), Some(json!(1)));
        assert_eq!(typed_value(five, "007"), Some(json!(7)));
        assert_eq!(typed_value(five, ""), None);
        assert_eq!(typed_value(factor, "2."), Some(json!(2)));
        assert_eq!(typed_value(factor, "0.25"), Some(json!(0.25)));
    }
}
