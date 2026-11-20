//! The whole screen as rows: the sidebar and the pane, the footer, and the toasts and the dialog over
//! them; or the splash screen, or the size the menu needs.

use baton_core::lines::{Line, s};

use super::chrome::footer_line;
use super::kit::{Float, block, overlay, spin, theme_of};
use super::overlays::{modal_layout, toast_layouts};
use super::pane::right_pane;
use super::sidebar::sidebar;
use super::splash::splash;
use crate::layout::{body_height, frame_cols, frame_rows, too_small};
use crate::pane::pane_box;
use crate::state::State;
use crate::tree::side_width;

/// The body: the sidebar beside the pane, or a word that the accounts are being read.
fn body(state: &State) -> Vec<Line> {
    let width = frame_cols(state);
    let height = body_height(state);
    if state.data.is_none() {
        return block(
            &[Vec::new(), vec![s("  Loading your accounts...", "muted")]],
            width,
            Some(height),
        );
    }
    let theme = theme_of(state);
    let side = sidebar(state, &theme, side_width(state), height);
    let pane = right_pane(state, &pane_box(state));
    let rows: Vec<Line> = side
        .into_iter()
        .zip(pane)
        .map(|(mut left, right)| {
            left.extend(right);
            left
        })
        .collect();
    block(&rows, width, Some(height))
}

/// The screen for a state: every row but the terminal's last one (writing its bottom-right cell would
/// scroll a Windows console), each exactly as wide as the terminal. `tick` turns the spinners: one
/// frame every 100 ms.
///
/// A span's style words are drawn through [`super::kit::props`], which also gives the frame its text
/// colour and its backgrounds.
pub fn frame(state: &State, tick: u64) -> Vec<Line> {
    turn(&layout(state), state, tick)
}

/// The rows of a layout with their spinners at the frame of this tick.
pub fn turn(rows: &[Line], state: &State, tick: u64) -> Vec<Line> {
    let g = theme_of(state).g;
    rows.iter().map(|row| spin(row, g, tick)).collect()
}

/// Whether a layout has a spinner in it (and so changes with the tick).
pub fn spins(rows: &[Line]) -> bool {
    rows.iter()
        .flatten()
        .any(|span| span.style.split(' ').any(|word| word == "spin"))
}

/// The screen for a state before its spinners turn: what only changes when the state does.
pub fn layout(state: &State) -> Vec<Line> {
    let theme = theme_of(state);
    let cols = frame_cols(state);
    let small = too_small(state);
    let mut rows = if small || state.splash {
        splash(state, small)
    } else {
        let mut rows = body(state);
        rows.extend(block(&[footer_line(state, theme.g)], cols, Some(1)));
        for toast in toast_layouts(state, &theme) {
            let float = Float {
                top: toast.top,
                left: toast.left,
                width: toast.width,
                surface: true,
            };
            overlay(&mut rows, float, &toast.lines);
        }
        if let Some(modal) = modal_layout(state, &theme) {
            let float = Float {
                top: modal.top,
                left: modal.left,
                width: modal.width,
                surface: true,
            };
            overlay(&mut rows, float, &modal.lines);
        }
        rows
    };
    rows.truncate(frame_rows(state));
    rows
}

/// A frame as plain text, one string per row, without the spaces at the end of each.
pub fn frame_text(state: &State, tick: u64) -> Vec<String> {
    frame(state, tick)
        .iter()
        .map(|row| baton_core::lines::plain(row).trim_end().to_owned())
        .collect()
}
