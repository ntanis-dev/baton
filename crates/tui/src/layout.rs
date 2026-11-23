//! One rail, one workspace and a quiet footer. A reserved last row avoids Windows console scrolling
//! when the bottom-right cell is written.
//!
//! The menu draws from 80 x 24 cells up, the window a terminal opens with on macOS and Linux. The
//! pane is as wide at 80 columns as it is at 100: what is missing below 100 comes off the rail, which
//! then shows its name in place of the wordmark. Every size here is worked out from the terminal's, in
//! one place, for what is drawn and for where a click lands.

use crate::state::State;

/// The size of a terminal, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    /// Its columns.
    pub cols: usize,
    /// Its rows.
    pub rows: usize,
}

/// The smallest terminal the menu draws in.
pub const MIN_SIZE: Size = Size { cols: 80, rows: 24 };
/// From this many columns and rows on the sidebar has its full width and its wordmark.
pub const ROOMY: Size = Size { cols: 100, rows: 28 };
/// Rows above the body.
pub const HEADER_ROWS: usize = 0;
/// Rows below the body.
pub const FOOTER_ROWS: usize = 1;
/// The width of the sidebar where there is room for all of it.
pub const SIDEBAR_COLS: usize = 46;
/// The rows of the sidebar's wordmark, caption and search control.
const SIDE_BRAND_ROWS: usize = 13;
/// The rows of the sidebar's name and search control, where there is no room for the wordmark.
const SIDE_NAME_ROWS: usize = 4;
/// The rows of the sidebar's Add account control and scroll hint.
pub const SIDE_FOOTER_ROWS: usize = 3;
/// The rows of a pane's title and note.
pub const PANE_HEADER_ROWS: usize = 4;

/// The columns the menu draws on.
pub fn frame_cols(state: &State) -> usize {
    state.size.cols.max(1)
}

/// The rows the menu draws on: every row but the last.
pub fn frame_rows(state: &State) -> usize {
    state.size.rows.saturating_sub(1).max(1)
}

/// The rows of the body: the sidebar and the pane.
pub fn body_height(state: &State) -> usize {
    frame_rows(state).saturating_sub(HEADER_ROWS + FOOTER_ROWS).max(1)
}

/// The width of the sidebar: all of it from 100 columns on, and below that less by every column the
/// terminal is short of 100, so that the pane keeps its width.
pub fn side_cols(state: &State) -> usize {
    let short = ROOMY.cols.saturating_sub(state.size.cols);
    SIDEBAR_COLS
        .saturating_sub(short)
        .max(SIDEBAR_COLS - (ROOMY.cols - MIN_SIZE.cols))
}

/// Whether the sidebar shows its name in place of the wordmark: the wordmark needs the full width,
/// and its rows are the list's where rows are few.
pub fn compact(state: &State) -> bool {
    state.size.cols < ROOMY.cols || state.size.rows < ROOMY.rows
}

/// The rows of the sidebar above its list: the brand and the search control, which is the row before
/// the last of them.
pub fn brand_rows(state: &State) -> usize {
    if compact(state) {
        SIDE_NAME_ROWS
    } else {
        SIDE_BRAND_ROWS
    }
}

/// Whether the terminal is smaller than the menu needs.
pub fn too_small(state: &State) -> bool {
    state.size.cols < MIN_SIZE.cols || state.size.rows < MIN_SIZE.rows
}

/// The room inside a pane of this size: without its side padding, its header and its last row.
pub fn inner(width: usize, height: usize) -> Size {
    Size {
        cols: width.saturating_sub(6).max(1),
        rows: height.saturating_sub(PANE_HEADER_ROWS + 1).max(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Init, initial_state};

    #[test]
    fn rows_and_columns() {
        let state = initial_state(Init {
            size: Size { cols: 120, rows: 30 },
            ..Init::default()
        });
        assert_eq!(
            (frame_cols(&state), frame_rows(&state), body_height(&state)),
            (120, 29, 28)
        );
        assert!(!too_small(&state));
        assert_eq!(
            (side_cols(&state), compact(&state), brand_rows(&state)),
            (46, false, 13)
        );
        // The window a terminal opens with elsewhere: the pane keeps its 54 columns.
        let small = initial_state(Init {
            size: Size { cols: 80, rows: 24 },
            ..Init::default()
        });
        assert!(!too_small(&small));
        assert_eq!(
            (side_cols(&small), compact(&small), brand_rows(&small)),
            (26, true, 4)
        );
        let between = |cols: usize, rows: usize| {
            let state = initial_state(Init {
                size: Size { cols, rows },
                ..Init::default()
            });
            (side_cols(&state), compact(&state), too_small(&state))
        };
        assert_eq!(between(90, 30), (36, true, false));
        assert_eq!(between(100, 27), (46, true, false));
        assert_eq!(between(79, 24), (26, true, true));
        assert_eq!(between(80, 23), (26, true, true));
        let none = initial_state(Init {
            size: Size { cols: 0, rows: 0 },
            ..Init::default()
        });
        assert_eq!(
            (frame_cols(&none), frame_rows(&none), body_height(&none)),
            (1, 1, 1)
        );
        assert_eq!(inner(74, 28), Size { cols: 68, rows: 23 });
        assert_eq!(inner(2, 2), Size { cols: 1, rows: 1 });
    }
}
