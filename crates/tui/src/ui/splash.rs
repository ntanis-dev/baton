//! The splash screen, and the same mark with the size the menu needs when the terminal is too small.

use baton_core::art::{SLOGAN, caption_lines, relay_art, splash_art};
use baton_core::lines::{Line, fit_line, line_width, s};

use super::kit::{Float, centered, overlay};
use crate::layout::{MIN_SIZE, frame_cols, frame_rows};
use crate::state::State;

/// The lines of the splash screen (`error`: of the size error), before they are centred.
fn splash_lines(state: &State, error: bool) -> Vec<Line> {
    let rows = frame_rows(state);
    let mut art: Vec<Line> = Vec::new();
    if rows >= 24 {
        art.extend(relay_art());
        art.push(Vec::new());
    }
    art.extend(splash_art());
    let art_width = art.iter().map(|line| line_width(line)).max().unwrap_or(0);
    let captions = caption_lines("");
    let caption_width = captions.iter().map(|line| line_width(line)).max().unwrap_or(0);
    let mut lines: Vec<Line> = art.iter().map(|line| fit_line(line, art_width, "")).collect();
    lines.push(Vec::new());
    if error {
        lines.push(vec![s(SLOGAN, "accent bold")]);
        lines.push(Vec::new());
        lines.push(vec![s("A little more room.", "accent")]);
        lines.push(vec![s(
            format!(
                "Baton needs {} x {} terminal cells.",
                MIN_SIZE.cols, MIN_SIZE.rows
            ),
            "muted",
        )]);
        lines.push(vec![s(
            format!(
                "Current size: {} x {}. Resize to continue.",
                state.size.cols, state.size.rows
            ),
            "muted",
        )]);
        lines.push(Vec::new());
        lines.push(vec![s("q  Quit", "muted")]);
    } else {
        lines.extend(captions.iter().map(|line| fit_line(line, caption_width, "")));
        lines.push(Vec::new());
        lines.push(Vec::new());
        lines.push(vec![s("Press any key to start", "accent bold")]);
    }
    if rows >= lines.len() {
        return lines;
    }
    // No room for the art: the name and the last lines.
    let keep = if error { 5 } else { 1 };
    let mut shown: Vec<Line> = vec![vec![s("BATON", "brand bold")], Vec::new()];
    shown.extend(lines.split_off(lines.len().saturating_sub(keep)));
    shown
}

/// A line as it shows in `cols` cells: whole, or cut and ending in an ellipsis.
fn cut_to(line: &[baton_core::lines::Span], cols: usize) -> Line {
    if line_width(line) <= cols {
        return line.to_vec();
    }
    centered(line, cols)
}

/// The splash screen (`error`: the size error) as the frame's rows, every line in the middle.
pub fn splash(state: &State, error: bool) -> Vec<Line> {
    let cols = frame_cols(state);
    let rows = frame_rows(state);
    let shown = splash_lines(state, error);
    let mut out: Vec<Line> = (0..rows).map(|_| fit_line(&[], cols, "")).collect();
    if shown.len() > rows {
        // More lines than rows: the lines share the rows evenly, a later one over an earlier one.
        for (i, line) in shown.iter().enumerate() {
            let row = ((i * rows) as f64 / shown.len() as f64 + 0.5).floor() as usize;
            let w = line_width(line).min(cols);
            if w > 0 {
                let float = Float {
                    top: row,
                    left: (cols - w) / 2,
                    width: w,
                    surface: false,
                };
                overlay(&mut out, float, &[cut_to(line, cols)]);
            }
        }
        return out;
    }
    // Half a row of room goes above, as the layout that drew this before rounded it.
    let top = (rows - shown.len()).div_ceil(2);
    for (i, line) in shown.iter().enumerate() {
        if let Some(slot) = out.get_mut(top + i) {
            *slot = centered(line, cols);
        }
    }
    out
}
