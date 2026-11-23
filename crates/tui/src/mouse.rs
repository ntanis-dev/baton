//! What a click or a turn of the wheel lands on.

use baton_core::text::width;

use crate::keys::Key;
use crate::layout::{PANE_HEADER_ROWS, SIDE_FOOTER_ROWS, body_height, brand_rows, frame_cols, too_small};
use crate::pane::{Item, current_item, pane_box, pane_model, pane_window};
use crate::state::{ModalKind, MouseEvent, MouseKind, State};
use crate::tree::{side_focused, side_lines, side_span, side_top, side_width, tree_rows};
use crate::ui::chrome::footer_model;
use crate::ui::kit::theme_of;
use crate::ui::overlays::{modal_layout, toast_layouts};
use crate::views::policy::{FIELDS, FieldKind};

/// What a click on a policy field's control does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FieldCommand {
    /// Picks the first choice.
    HomeChoice,
    /// Picks the last choice.
    EndChoice,
    /// Steps the value down.
    Left,
    /// Steps the value up.
    Right,
    /// Asks to apply the form.
    Enter,
}

/// What the mouse landed on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The splash screen: any click leaves it.
    Dismiss,
    /// A toast, by its number: a click dismisses it.
    Toast(u64),
    /// The open dialog, scrolled to this line.
    ModalScroll(usize),
    /// The open dialog's list, with the keyboard on this option: the wheel moves it, never past the ends.
    ModalIndex(usize),
    /// Beside the open dialog: a click there closes a menu, a pick list or a confirmation.
    Outside,
    /// An option of a choice dialog.
    Choice(usize),
    /// A command of an Actions dialog.
    RunAction {
        /// The command.
        command: String,
        /// Its place among the commands.
        index: usize,
    },
    /// A field of the thresholds dialog, and the key a click there stands for.
    Threshold {
        /// The field.
        index: usize,
        /// The key.
        command: Option<Key>,
    },
    /// A sidebar row or a link of a pane, by the row's id (`add-account` opens the Add account dialog).
    Select(String),
    /// The wheel: lines to scroll, over the sidebar or over the pane.
    Scroll {
        /// Lines, down positive.
        delta: i32,
        /// Whether the pointer is over the sidebar.
        side: bool,
    },
    /// A row of the policy form, and what a click on its control does.
    Field {
        /// The row.
        index: usize,
        /// What the control does.
        command: Option<FieldCommand>,
    },
    /// A key: a hint of the footer, a button of a dialog, the pinned controls of the sidebar.
    Command(Key),
}

/// A place on the screen and what a click on it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Its first column.
    pub x: usize,
    /// Its row.
    pub y: usize,
    /// Its width.
    pub width: usize,
    /// What a click on it does.
    pub hit: Hit,
}

/// What is at a cell among these targets.
fn at(targets: &[Target], x: usize, y: usize) -> Option<Hit> {
    targets
        .iter()
        .find(|t| y == t.y && x >= t.x && x < t.x + t.width)
        .map(|t| t.hit.clone())
}

/// What a mouse event lands on, or None.
pub fn hit_test(state: &State, event: &MouseEvent) -> Option<Hit> {
    if too_small(state) || event.x < 0 || event.y < 0 {
        return None;
    }
    let (x, y) = (event.x as usize, event.y as usize);
    if x >= frame_cols(state) || y + 1 >= state.size.rows {
        return None;
    }
    if state.splash {
        return (event.kind == MouseKind::Press).then_some(Hit::Dismiss);
    }
    if !matches!(event.kind, MouseKind::Press | MouseKind::Wheel) {
        return None;
    }
    // The left and the right button click; the middle one does nothing.
    if event.kind == MouseKind::Press && !matches!(event.button, 0 | 2) {
        return None;
    }
    let theme = theme_of(state);
    if let Some(layout) = modal_layout(state, &theme) {
        if event.kind == MouseKind::Wheel {
            let options = match state.modal.as_ref().map(|modal| &modal.kind) {
                Some(ModalKind::Actions { index, options, .. }) => Some((*index, options.len())),
                Some(ModalKind::Choice { index, options, .. }) => Some((*index, options.len())),
                _ => None,
            };
            if let Some((index, count)) = options {
                let last = count.saturating_sub(1) as i64;
                let to = (index as i64 + i64::from(event.delta)).clamp(0, last);
                return Some(Hit::ModalIndex(to as usize));
            }
            let line = (layout.offset as i64 + i64::from(event.delta)).clamp(0, layout.max as i64);
            return Some(Hit::ModalScroll(line as usize));
        }
        // The footer shows the dialog's own keys while it is open.
        let hint = (y + 2 == state.size.rows)
            .then(|| at(&footer_model(state, theme.g).targets, x, y))
            .flatten();
        let inside = x >= layout.left
            && x < layout.left + layout.width
            && y >= layout.top
            && y < layout.top + layout.lines.len();
        return hint
            .or_else(|| at(&layout.targets, x, y))
            .or((!inside).then_some(Hit::Outside));
    }
    state.data.as_ref()?;
    if event.kind == MouseKind::Press {
        let toast = toast_layouts(state, &theme)
            .into_iter()
            .find(|t| x >= t.left && x < t.left + t.width && y >= t.top && y < t.top + t.lines.len());
        if let Some(toast) = toast {
            return Some(Hit::Toast(toast.id));
        }
        if y + 2 == state.size.rows {
            return at(&footer_model(state, theme.g).targets, x, y);
        }
    }
    let side = side_width(state);
    let in_side = x < side;
    if event.kind == MouseKind::Wheel {
        return Some(Hit::Scroll {
            delta: event.delta,
            side: in_side,
        });
    }
    if in_side {
        let above = brand_rows(state);
        if y == above - 2 {
            return Some(Hit::Command(Key::Char('/')));
        }
        if y + 2 == body_height(state) {
            return Some(Hit::Command(Key::Named("add-account".to_owned())));
        }
        let cells = side - 3;
        let rows = tree_rows(state);
        let view = side_lines(
            state,
            cells,
            side_focused(state),
            theme.g,
            &rows,
            side_span(state, cells, &rows),
        );
        let room = body_height(state).saturating_sub(above + SIDE_FOOTER_ROWS).max(1);
        let top = side_top(view.lines.len(), view.from, view.to, room);
        if y < above || y - above >= room || x < 1 || x >= side - 1 {
            return None;
        }
        let id = view.owners.get(top + y - above)?.clone()?;
        return Some(Hit::Select(id));
    }
    let pane = pane_box(state);
    let px = x.checked_sub(side + 3).filter(|px| *px < pane.room.cols)?;
    let py = y
        .checked_sub(PANE_HEADER_ROWS)
        .filter(|py| *py < pane.room.rows)?;
    let item = current_item(state);
    let model = pane_model(state, &item, &pane);
    let w = pane_window(state, &model, &pane);
    let body_y = py + w.top;
    let link = model
        .targets
        .get(body_y)
        .and_then(|links| links.iter().find(|l| px >= l.x && px < l.x + l.width));
    if let Some(link) = link {
        return Some(Hit::Select(link.select.clone()));
    }
    if item == Item::Policy {
        if let Some(field) = FIELDS.get(body_y) {
            let control_x = 2 + FIELDS.iter().map(|f| width(f.label)).max().unwrap_or(0) + 2;
            let command = if px < control_x || px > control_x + 18 {
                None
            } else if matches!(field.kind, FieldKind::Choice(_)) {
                Some(if px < control_x + 8 {
                    FieldCommand::HomeChoice
                } else {
                    FieldCommand::EndChoice
                })
            } else {
                Some(if px < control_x + 2 {
                    FieldCommand::Left
                } else {
                    FieldCommand::Right
                })
            };
            return Some(Hit::Field {
                index: body_y,
                command,
            });
        }
        if body_y == FIELDS.len() + 1 {
            return Some(Hit::Field {
                index: FIELDS.len(),
                command: Some(FieldCommand::Enter),
            });
        }
    }
    None
}
