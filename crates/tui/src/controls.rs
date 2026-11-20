//! Buttons under a dialog: a label and its shortcut key each, wrapped onto more rows when they do not
//! fit on one.

use baton_core::lines::{Line, fit_line, line_width, p, s};

use crate::keys::Key;

/// A button to lay out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Button {
    /// The shortcut shown after the label; empty for none.
    pub key: String,
    /// What it says.
    pub label: String,
    /// The key it stands for.
    pub command: Key,
    /// Whether the keyboard is on it.
    pub selected: bool,
}

impl Button {
    /// A button that is not selected.
    pub fn new(key: &str, label: &str, command: Key) -> Button {
        Button {
            key: key.to_owned(),
            label: label.to_owned(),
            command,
            selected: false,
        }
    }
}

/// Where a button landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ButtonAt {
    /// Its first column.
    pub x: usize,
    /// Its row.
    pub y: usize,
    /// Its width.
    pub width: usize,
    /// The button.
    pub button: Button,
}

/// Buttons laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buttons {
    /// The rows, each `width` cells wide.
    pub lines: Vec<Line>,
    /// Where each button is.
    pub targets: Vec<ButtonAt>,
}

/// Lays buttons out in rows `width` cells wide.
pub fn buttons(entries: &[Button], width: usize) -> Buttons {
    let mut lines: Vec<Line> = vec![Vec::new()];
    let mut targets = Vec::new();
    for entry in entries {
        let mut line = vec![s(
            format!(" {} ", entry.label),
            if entry.selected { "selected bold" } else { "muted" },
        )];
        if !entry.key.is_empty() {
            line.push(s(
                format!(" {} ", entry.key),
                if entry.selected {
                    "selected bold"
                } else {
                    "accent bold"
                },
            ));
        }
        let size = line_width(&line);
        let mut row = lines.len() - 1;
        let mut x = line_width(&lines[row]);
        if x > 0 && x + size > width {
            lines.push(Vec::new());
            row += 1;
            x = 0;
        }
        lines[row].extend(line);
        lines[row].push(p("  "));
        targets.push(ButtonAt {
            x,
            y: row,
            width: size.min(width.saturating_sub(x)),
            button: entry.clone(),
        });
    }
    Buttons {
        lines: lines.iter().map(|line| fit_line(line, width, "")).collect(),
        targets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::lines::plain;

    #[test]
    fn buttons_in_rows() {
        let entries = [
            Button::new("Enter", "Save", Key::Enter),
            Button::new("Esc", "Cancel", Key::Escape),
        ];
        let wide = buttons(&entries, 40);
        assert_eq!(
            wide.lines.iter().map(|l| plain(l)).collect::<Vec<_>>(),
            [format!("{:<40}", " Save  Enter    Cancel  Esc")]
        );
        assert_eq!(
            wide.targets
                .iter()
                .map(|t| (t.x, t.y, t.width))
                .collect::<Vec<_>>(),
            [(0, 0, 13), (15, 0, 13)]
        );
        let narrow = buttons(&entries, 20);
        assert_eq!(
            narrow.lines.iter().map(|l| plain(l)).collect::<Vec<_>>(),
            [
                format!("{:<20}", " Save  Enter"),
                format!("{:<20}", " Cancel  Esc")
            ]
        );
        assert_eq!(narrow.targets[1].y, 1);
        let chosen = buttons(
            &[Button {
                selected: true,
                ..Button::new("y", "Yes", Key::Char('y'))
            }],
            20,
        );
        assert!(
            chosen.lines[0][..2]
                .iter()
                .all(|span| span.style == "selected bold")
        );
        assert_eq!(buttons(&[], 5).lines, vec![fit_line(&[], 5, "")]);
    }
}
