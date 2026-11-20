//! One fixed rail, one workspace and a quiet footer. A reserved last row avoids Windows console
//! scrolling when the bottom-right cell is written.

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
pub const MIN_SIZE: Size = Size { cols: 100, rows: 28 };
/// Rows above the body.
pub const HEADER_ROWS: usize = 0;
/// Rows below the body.
pub const FOOTER_ROWS: usize = 1;
/// The width of the sidebar.
pub const SIDEBAR_COLS: usize = 46;
/// The rows of the sidebar's brand and search control.
pub const SIDE_BRAND_ROWS: usize = 13;
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

/// Whether one pane shows at a time. Never: the menu asks for its minimum size instead.
pub fn is_narrow(_state: &State) -> bool {
    false
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
        assert!(!is_narrow(&state));
        let small = initial_state(Init {
            size: Size { cols: 80, rows: 24 },
            ..Init::default()
        });
        assert!(too_small(&small));
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
