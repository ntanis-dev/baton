//! What the console says of itself.

/// How wide the terminal is, in columns; None when there is none.
pub fn columns() -> Option<usize> {
    crossterm::terminal::size()
        .ok()
        .map(|(cols, _)| usize::from(cols))
        .filter(|cols| *cols > 0)
}
