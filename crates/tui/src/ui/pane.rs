//! The right pane: a title and a note, the body, and the legend of the signs the view uses.

use baton_core::lines::{Line, fit_line, line_width, p, restyle, s, spread};

use super::kit::block;
use crate::layout::PANE_HEADER_ROWS;
use crate::legend::legend_line;
use crate::pane::{PaneBox, current_item, pane_model, pane_window};
use crate::state::State;

/// The right pane as `pane.height` rows of `pane.width` cells, three cells of padding at each side.
pub fn right_pane(state: &State, pane: &PaneBox) -> Vec<Line> {
    let item = current_item(state);
    let model = pane_model(state, &item, pane);
    let w = pane_window(state, &model, pane);
    let title = restyle(&model.title, "bold");
    let right = match &state.busy {
        Some(busy) if !busy.is_empty() => vec![s(busy, "accent")],
        _ => model.right.clone(),
    };
    let progress: Line = if w.max > 0 {
        vec![s(
            format!(
                "{}\u{2013}{} of {}",
                w.top + 1,
                w.total.min(w.top + w.room),
                w.total
            ),
            "muted",
        )]
    } else {
        Vec::new()
    };
    let room = pane.room.cols;
    let legend_room = room.saturating_sub(if progress.is_empty() {
        0
    } else {
        line_width(&progress) + 3
    });
    let mut content = block(
        &[Vec::new(), title, right, Vec::new()],
        room,
        Some(PANE_HEADER_ROWS),
    );
    content.extend(block(&w.lines, room, Some(pane.room.rows)));
    content.push(spread(
        &legend_line(state, item.kind(), legend_room),
        &progress,
        room,
        "",
    ));
    block(&content, room, Some(pane.height))
        .into_iter()
        .map(|line| {
            let mut row = vec![p("   ")];
            row.extend(line);
            fit_line(&row, pane.width, "")
        })
        .collect()
}
