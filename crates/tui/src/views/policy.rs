//! The policy settings as a form: Up and Down pick a field, Left and Right change it, Enter asks to
//! confirm, Esc leaves without saving. Each field has a one-line hint that never wraps: its short form,
//! or none, when the line has no room for it.

use baton_core::config::Policy;
use baton_core::json::{num, show};
use baton_core::lines::{Line, line_width, p, s};
use baton_core::text::{round, width as cells};
use baton_core::theme::Glyphs;
use serde_json::{Value, json};

use super::common::{pad_end, pad_start};
use crate::keys::Key;
use crate::pane::{ViewModel, body};
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
        /// Its unit: `%`, `x` or a word.
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
        unit: "min",
    }
}

/// The form's fields, in order.
pub const FIELDS: [Field; 11] = [
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
];

/// The row of the Apply button: the one after the last field.
const APPLY_ROW: usize = FIELDS.len();

/// The policy form while it is open.
#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    /// The settings as they were when the form opened, one per field.
    pub original: Vec<Value>,
    /// The settings as the form has them now, one per field.
    pub values: Vec<Value>,
    /// The row the keyboard is on: a field, or the Apply button after the last one.
    pub index: usize,
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
            match unit {
                "%" => format!("{n}%"),
                "x" => format!("{n}x"),
                unit => format!("{n} {unit}"),
            }
        }
        FieldKind::Choice(_) => value.as_str().unwrap_or_default().to_owned(),
    }
}

/// The settings the form changed, by name, in the form's order.
pub fn changes(form: &Form) -> Vec<(String, Value)> {
    FIELDS
        .iter()
        .enumerate()
        .filter(|(at, _)| form.values.get(*at) != form.original.get(*at))
        .filter_map(|(at, f)| Some((f.key.to_owned(), form.values.get(at)?.clone())))
        .collect()
}

/// A field's control: arrows around a number, a dot and a word, or the choices with the chosen one in
/// brackets.
fn control(field: &Field, value: &Value, changed: bool, g: &Glyphs) -> Line {
    let tone = if changed { "warn bold" } else { "bold" };
    match field.kind {
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
                g,
            )
        })
        .collect();
    let control_width = controls.iter().map(|line| line_width(line)).max().unwrap_or(0) + 2;
    for (i, f) in FIELDS.iter().enumerate() {
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

/// The state with the form's field `at` set to `value`.
fn with_value(state: &State, at: usize, value: Value) -> State {
    let mut next = state.clone();
    if let Some(slot) = next.form.as_mut().and_then(|form| form.values.get_mut(at)) {
        *slot = value;
    }
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
    let count = FIELDS.len() + 1;
    let index = match key {
        Key::Up => Some(form.index.saturating_sub(1)),
        Key::Down => Some((form.index + 1).min(count - 1)),
        Key::Home => Some(0),
        Key::End => Some(count - 1),
        _ => None,
    };
    if let Some(index) = index {
        let mut next = state.clone();
        if let Some(form) = next.form.as_mut() {
            form.index = index;
        }
        return Some(result(next));
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
        return Some(result(state.clone()));
    };
    let value = form.values.get(form.index).cloned().unwrap_or(Value::Null);
    match key {
        Key::Left => Some(result(with_value(state, form.index, step(field, &value, -1)))),
        Key::Right => Some(result(with_value(state, form.index, step(field, &value, 1)))),
        Key::Char('d') => {
            let default = Policy::default().get(field.key).unwrap_or(Value::Null);
            Some(result(with_value(state, form.index, default)))
        }
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
                "smart", "90%", "95%", "20%", "1.5x", "20 min", "30 min", "on", "30 min", "off", "on"
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
    fn the_form_as_lines() {
        let mut form = start_form(&Policy::default());
        form.values[1] = json!(91);
        let lines: Vec<String> = form_lines(&form, 68, &RICH, true)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(lines.len(), FIELDS.len() + 4);
        assert_eq!(
            lines[0],
            "› Mode           [smart]  order   how accounts are picked"
        );
        assert_eq!(lines[1], "  5-hour limit   ←    91% →       blocked at or above");
        assert_eq!(lines[7], "  Rebalance      ● on             after compaction");
        assert_eq!(lines[10], "  Full sync      ● on             every account");
        assert_eq!(lines[12], "   Apply 1 change   asks to confirm first");
        assert_eq!(lines[14], "1 differ from the defaults (d resets the field)");
        let wide: Vec<String> = form_lines(&start_form(&Policy::default()), 100, &RICH, false)
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(
            wide[0],
            "› Mode           [smart]  order   smart picks by room and urgency; order fills one then next"
        );
        assert_eq!(wide[12], "   Apply   nothing changed yet");
        assert_eq!(wide[14], "all at their defaults");
    }
}
