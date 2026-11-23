//! The footer: the keys that do something where the keyboard is, each one clickable.

use baton_core::lines::{Line, Span, fit_line, line_width, p, s};
use baton_core::theme::Glyphs;

use crate::keys::Key;
use crate::layout::frame_cols;
use crate::mouse::{Hit, Target};
use crate::pane::{Item, current_item, pane_overflows};
use crate::state::{Focus, ModalKind, State, session_by_id};

/// A hint of the footer: a key by the name the footer knows it under, what it does, and how soon it
/// is dropped when room is short (the heavier, the sooner).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hint {
    /// The key: `updown`, `leftright`, `down`, `enter`, or what is shown as it is (`Esc`, `q`).
    pub key: &'static str,
    /// What it does.
    pub label: &'static str,
    /// How soon it gives way.
    pub weight: u8,
}

/// A hint that gives way last.
const fn hint(key: &'static str, label: &'static str) -> Hint {
    Hint {
        key,
        label,
        weight: 1,
    }
}

/// A key as the footer shows it.
pub fn key_label(key: &str, g: &Glyphs) -> String {
    match key {
        "updown" => format!("{}{}", g.up, g.down),
        "leftright" => format!("{}{}", g.left, g.right),
        "down" => g.down.to_owned(),
        "enter" => g.enter.to_owned(),
        other => other.to_owned(),
    }
}

/// The hints for where the keyboard is.
pub fn footer_hints(state: &State) -> Vec<Hint> {
    match state.modal.as_ref().map(|modal| &modal.kind) {
        Some(ModalKind::Choice { .. } | ModalKind::Actions { .. }) => {
            return vec![
                hint("updown", "Choose"),
                hint("enter", "Select"),
                hint("Esc", "Close"),
            ];
        }
        Some(ModalKind::Confirm { .. }) => {
            return vec![
                hint("leftright", "Choose"),
                hint("enter", "Select"),
                hint("Esc", "Cancel"),
            ];
        }
        Some(ModalKind::Input { .. }) => {
            return vec![
                hint("Ctrl+U", "Clear"),
                hint("enter", "Save"),
                hint("Esc", "Cancel"),
            ];
        }
        Some(ModalKind::Thresholds { .. }) => {
            return vec![
                hint("updown", "Field"),
                hint("leftright", "Adjust"),
                Hint {
                    key: "0-9",
                    label: "Type",
                    weight: 2,
                },
                hint("d", "Default"),
                hint("enter", "Save"),
                hint("Esc", "Cancel"),
            ];
        }
        Some(ModalKind::Keys) => {
            return vec![hint("updown", "Scroll"), hint("Esc", "Close")];
        }
        None => {}
    }
    if state.filtering {
        return vec![
            hint("down", "Results"),
            hint("enter", "Done"),
            hint("Ctrl+U", "Clear"),
            hint("Esc", "Cancel"),
        ];
    }
    let item = current_item(state);
    let jump = Hint {
        key: "Home/End",
        label: "Jump",
        weight: 2,
    };
    if item == Item::Policy && state.form.is_some() {
        return vec![
            hint("updown", "Field"),
            hint("leftright", "Adjust"),
            Hint {
                key: "0-9",
                label: "Type",
                weight: 2,
            },
            jump,
            hint("d", "Default"),
            hint("enter", "Apply"),
            hint("Esc", "Done"),
        ];
    }
    let mut hints = vec![hint("updown", "Navigate"), jump];
    if matches!(state.focus, Focus::Search | Focus::Add) {
        hints.push(hint("enter", "Activate"));
    } else {
        match &item {
            Item::Policy => hints.push(hint("enter", "Edit")),
            Item::Sessions => hints.push(hint(
                "enter",
                if state.open_sessions { "Collapse" } else { "Expand" },
            )),
            Item::Overview | Item::Account(_) => hints.push(hint("enter", "Actions")),
            Item::Session(id) if session_by_id(state, id).is_some_and(|x| x.live) => {
                hints.push(hint("enter", "Actions"));
            }
            _ => {}
        }
    }
    if !state.filter.is_empty() {
        hints.push(hint("Esc", "Clear"));
    }
    if pane_overflows(state) {
        hints.push(Hint {
            key: "PgUp/Dn",
            label: "Scroll",
            weight: 2,
        });
    }
    // Refresh gives way before the keys do: the keys' own list names it.
    hints.push(Hint {
        key: "r",
        label: "Refresh",
        weight: 3,
    });
    hints.push(hint("?", "Keys"));
    hints.push(hint("q", "Quit"));
    hints
}

/// The footer laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Footer {
    /// The line, as wide as the frame.
    pub line: Line,
    /// Where each hint is and the key a click on it stands for.
    pub targets: Vec<Target>,
}

/// The hints as spans.
fn spans(list: &[Hint], g: &Glyphs) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, hint) in list.iter().enumerate() {
        out.push(p(if i > 0 { "    " } else { "  " }));
        out.push(s(key_label(hint.key, g), "accent bold"));
        out.push(s(format!(" {}", hint.label), "muted"));
    }
    out
}

/// The key a click on a hint stands for.
fn command_of(key: &str) -> Key {
    Key::from_name(match key {
        "updown" => "down",
        "leftright" => "right",
        "enter" => "enter",
        "Esc" => "escape",
        "Home/End" => "end",
        "PgUp/Dn" => "pagedown",
        "Ctrl+U" => "ctrl+u",
        other => other,
    })
}

/// The footer: as many hints as fit, the heaviest dropped first, and where each one is.
pub fn footer_model(state: &State, g: &Glyphs) -> Footer {
    let cols = frame_cols(state);
    let mut shown = footer_hints(state);
    while shown.len() > 1 && line_width(&spans(&shown, g)) > cols {
        let weight = shown.iter().map(|h| h.weight).max().unwrap_or(0);
        if let Some(index) = shown.iter().rposition(|h| h.weight == weight) {
            shown.remove(index);
        }
    }
    let mut x = 0;
    let y = state.size.rows.saturating_sub(2);
    let targets = shown
        .iter()
        .enumerate()
        .map(|(i, hint)| {
            x += if i > 0 { 4 } else { 2 };
            let start = x;
            let size = line_width(&[p(format!("{} {}", key_label(hint.key, g), hint.label))]);
            x += size;
            Target {
                x: start,
                y,
                width: size,
                hit: Hit::Command(command_of(hint.key)),
            }
        })
        .collect();
    Footer {
        line: fit_line(&spans(&shown, g), cols, ""),
        targets,
    }
}

/// The footer's line.
pub fn footer_line(state: &State, g: &Glyphs) -> Line {
    footer_model(state, g).line
}
