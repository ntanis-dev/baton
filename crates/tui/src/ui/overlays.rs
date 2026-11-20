//! What floats over the panes: dialogs and the toasts, each a box of lines padded to its width.

use baton_core::json::show;
use baton_core::lines::{Line, Span, fit_line, p, s, spread, wrap_line};
use baton_core::text::{width as cells, wrap};
use baton_core::theme::Theme;

use super::kit::{Edge, edge};
use crate::controls::{Button, buttons};
use crate::keys::Key;
use crate::layout::{HEADER_ROWS, body_height, frame_cols};
use crate::modal::field_window;
use crate::mouse::{Hit, Target};
use crate::state::{Modal, ModalKind, State, Submit, Tone};

/// Lines in a box: a top edge with the title in it, the lines between two borders, a bottom edge.
fn box_lines(theme: &Theme, width: usize, lines: &[Line], title: &[Span], style: &str) -> Vec<Line> {
    let v = theme.g.corners.v;
    let room = width.saturating_sub(4);
    let mut out = vec![edge(
        theme,
        width,
        &Edge {
            left: title,
            right: &[],
            bottom: false,
            style,
        },
    )];
    for line in lines {
        let mut row = vec![s(format!("{v} "), style)];
        row.extend(fit_line(line, room, ""));
        row.push(s(format!(" {v}"), style));
        out.push(row);
    }
    out.push(edge(
        theme,
        width,
        &Edge {
            left: &[],
            right: &[],
            bottom: true,
            style,
        },
    ));
    out
}

/// A text field's value as spans, the cursor inverse, scrolled so that the cursor shows in `room`
/// cells.
pub fn field_spans(value: &str, cursor: usize, room: usize, style: &str) -> Line {
    let f = field_window(value, cursor, room);
    vec![s(f.before, style), s(f.at, "inverse"), s(f.after, style)]
}

/// A dialog's content: its title, its lines, what a click on them does, and its buttons when they are
/// not the usual two.
struct Content {
    /// The title.
    title: Line,
    /// The lines.
    lines: Vec<Line>,
    /// What a click does, in the content's own cells.
    targets: Vec<Target>,
    /// The dialog's own buttons.
    buttons: Option<Vec<Button>>,
}

/// A percentage threshold as text: `92`.
fn percent(value: f64) -> String {
    show(value)
}

/// The content of the open dialog, `inner` cells wide.
fn modal_content(modal: &Modal, theme: &Theme, inner: usize) -> Content {
    let g = theme.g;
    let mut lines: Vec<Line> = vec![Vec::new()];
    for text in &modal.lines {
        lines.extend(wrap(text, inner, "").into_iter().map(|l| vec![p(l)]));
    }
    let title = vec![s(&modal.title, "bold")];
    let mut targets = Vec::new();
    match &modal.kind {
        ModalKind::Actions { index, options, .. } => {
            lines.push(Vec::new());
            for (i, option) in options.iter().enumerate() {
                let active = i == *index;
                let style = if active { "selected bold" } else { "" };
                let label_style = if active {
                    style
                } else if option.command == "d" {
                    "err"
                } else {
                    "text"
                };
                let right = if option.key.is_empty() {
                    Vec::new()
                } else {
                    vec![s(
                        format!("{} ", option.key),
                        if active { style } else { "accent bold" },
                    )]
                };
                let line = spread(
                    &[
                        s(
                            if active {
                                format!(" {} ", g.arrow)
                            } else {
                                "   ".to_owned()
                            },
                            if active { style } else { "accent" },
                        ),
                        s(&option.label, label_style),
                    ],
                    &right,
                    inner,
                    style,
                );
                targets.push(Target {
                    x: 0,
                    y: lines.len(),
                    width: inner,
                    hit: Hit::RunAction {
                        command: option.command.clone(),
                        index: i,
                    },
                });
                lines.push(line);
            }
            Content {
                title,
                lines,
                targets,
                buttons: Some(vec![Button::new("Esc", "Close", Key::Escape)]),
            }
        }
        ModalKind::Confirm { yes, focus, .. } => {
            let on_yes = focus.unwrap_or(*yes);
            let yes = Button {
                selected: on_yes,
                ..Button::new("y", "Yes", Key::Char('y'))
            };
            let no = Button {
                selected: !on_yes,
                ..Button::new("n", "No", Key::Char('n'))
            };
            Content {
                title,
                lines,
                targets,
                buttons: Some(vec![yes, no]),
            }
        }
        ModalKind::Choice { options, index, .. } => {
            lines.push(Vec::new());
            for (i, option) in options.iter().enumerate() {
                let own = if i == *index {
                    wrap_line(
                        &[s(
                            format!(" {} {} {} ", g.arrow, i + 1, option.label),
                            "selected bold",
                        )],
                        inner,
                        5,
                    )
                } else {
                    wrap_line(
                        &[s(format!("   {} ", i + 1), "accent"), p(&option.label)],
                        inner,
                        5,
                    )
                };
                for line in own {
                    targets.push(Target {
                        x: 0,
                        y: lines.len(),
                        width: inner,
                        hit: Hit::Choice(i),
                    });
                    lines.push(line);
                }
            }
            Content {
                title,
                lines,
                targets,
                buttons: None,
            }
        }
        ModalKind::Thresholds {
            account,
            account_name,
            index,
            values,
            defaults,
        } => {
            let name = if account_name.is_empty() {
                account
            } else {
                account_name
            };
            let words = format!("At or above these, {name} is blocked. \"default\" follows the policy.");
            let mut out: Vec<Line> = vec![Vec::new()];
            out.extend(wrap(&words, inner, "").into_iter().map(|l| vec![s(l, "muted")]));
            out.push(Vec::new());
            for (i, label) in ["5-hour", "Weekly"].into_iter().enumerate() {
                let at = i == *index;
                let value = match values[i] {
                    Some(v) => s(format!("{}%", percent(v)), "bold"),
                    None => s(format!("default ({}%)", percent(defaults[i])), "muted"),
                };
                out.push(vec![
                    s(
                        if at {
                            format!("{} ", g.arrow)
                        } else {
                            "  ".to_owned()
                        },
                        "accent bold",
                    ),
                    s(format!("{label:<8}"), if at { "selected bold" } else { "bold" }),
                    s(format!("{} ", g.left), "muted"),
                    value,
                    s(format!(" {}", g.right), "muted"),
                ]);
                let y = out.len() - 1;
                targets.push(Target {
                    x: 0,
                    y,
                    width: 10,
                    hit: Hit::Threshold {
                        index: i,
                        command: None,
                    },
                });
                targets.push(Target {
                    x: 10,
                    y,
                    width: 2,
                    hit: Hit::Threshold {
                        index: i,
                        command: Some(Key::Left),
                    },
                });
                targets.push(Target {
                    x: 12,
                    y,
                    width: inner.saturating_sub(12),
                    hit: Hit::Threshold {
                        index: i,
                        command: Some(Key::Right),
                    },
                });
            }
            Content {
                title: vec![s(format!("Thresholds of {name}"), "bold")],
                lines: out,
                targets,
                buttons: None,
            }
        }
        ModalKind::Input { value, cursor, .. } => {
            let prompt = format!("{} ", g.arrow);
            lines.push(Vec::new());
            let mut line = vec![s(&prompt, "accent bold")];
            line.extend(field_spans(
                value,
                *cursor,
                inner.saturating_sub(cells(&prompt)).max(4),
                "",
            ));
            lines.push(line);
            Content {
                title,
                lines,
                targets,
                buttons: None,
            }
        }
    }
}

/// A dialog laid out on the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModalLayout {
    /// Its first row.
    pub top: usize,
    /// Its first column.
    pub left: usize,
    /// Its lines, borders included.
    pub lines: Vec<Line>,
    /// Its width.
    pub width: usize,
    /// What a click does, in the screen's cells.
    pub targets: Vec<Target>,
    /// The first line of its content that shows.
    pub offset: usize,
    /// The furthest its content scrolls.
    pub max: usize,
    /// The rows its content has.
    pub room: usize,
}

/// The label of the button that takes a dialog.
fn submit_label(modal: &Modal) -> &str {
    match &modal.kind {
        ModalKind::Choice {
            submit_label: Some(label),
            ..
        } => label,
        ModalKind::Input {
            submit: Submit::Rename(_),
            ..
        } => "Rename",
        ModalKind::Input { .. } => "Add account",
        ModalKind::Choice { .. } => "Move session",
        _ => "Save",
    }
}

/// The open dialog laid out in the middle of the body; None when no dialog is open.
pub fn modal_layout(state: &State, theme: &Theme) -> Option<ModalLayout> {
    let modal = state.modal.as_ref()?;
    let cols = frame_cols(state);
    let height = body_height(state);
    let width = cols.saturating_sub(4).clamp(30, 70);
    let inner = width - 4;
    let content = modal_content(modal, theme, inner);
    let usual = [
        Button::new("Enter", submit_label(modal), Key::Enter),
        Button::new("Esc", "Cancel", Key::Escape),
    ];
    let tools = buttons(content.buttons.as_deref().unwrap_or(&usual), inner);
    let room = height.saturating_sub(4 + tools.lines.len()).max(1);
    let max = content.lines.len().saturating_sub(room);
    // The row of the option the keyboard is on, to keep it in view.
    let chosen = modal.index().and_then(|index| {
        content.targets.iter().find_map(|t| match &t.hit {
            Hit::Choice(i) | Hit::RunAction { index: i, .. } if *i == index => Some(t.y),
            _ => None,
        })
    });
    let desired = chosen.map_or(max, |row| row.saturating_sub(room / 2));
    let offset = modal.scroll.unwrap_or(desired).min(max);
    let body: Vec<Line> = content.lines.iter().skip(offset).take(room).cloned().collect();
    let mut lines = body.clone();
    lines.push(Vec::new());
    lines.extend(tools.lines.iter().cloned());
    let boxed = box_lines(theme, width, &lines, &content.title, "muted");
    let top = HEADER_ROWS + height.saturating_sub(boxed.len()) / 2;
    let left = cols.saturating_sub(width) / 2;
    let mut targets: Vec<Target> = content
        .targets
        .into_iter()
        .filter(|t| t.y >= offset && t.y < offset + room)
        .map(|t| Target { y: t.y - offset, ..t })
        .collect();
    targets.extend(tools.targets.iter().map(|t| Target {
        x: t.x,
        y: body.len() + 1 + t.y,
        width: t.width,
        hit: Hit::Command(t.button.command.clone()),
    }));
    for target in &mut targets {
        target.x += left + 2;
        target.y += top + 1;
    }
    Some(ModalLayout {
        top,
        left,
        lines: boxed,
        width,
        targets,
        offset,
        max,
        room,
    })
}

/// A toast laid out on the screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastLayout {
    /// The toast's number.
    pub id: u64,
    /// Its first row.
    pub top: usize,
    /// Its first column.
    pub left: usize,
    /// Its lines.
    pub lines: Vec<Line>,
    /// Its width.
    pub width: usize,
}

/// The last two toasts laid out from the bottom right of the body up, the newest lowest.
pub fn toast_layouts(state: &State, theme: &Theme) -> Vec<ToastLayout> {
    let g = theme.g;
    let cols = frame_cols(state);
    let height = body_height(state);
    let mut floats = Vec::new();
    let mut bottom = (HEADER_ROWS + height) as i64 - 1;
    for t in state.toasts.iter().rev().take(2) {
        let icon = match t.tone {
            Tone::Ok => g.check,
            Tone::Error => g.cross,
            Tone::Info => g.bullet,
        };
        let width = (cells(&t.text) + 5).min(72).min(cols.saturating_sub(2)).max(16);
        let style = match t.tone {
            Tone::Ok => "on-ok",
            Tone::Error => "on-err",
            Tone::Info => "on-accent",
        };
        let lines: Vec<Line> = wrap(&t.text, width - 5, "")
            .into_iter()
            .take(3)
            .enumerate()
            .map(|(i, l)| {
                let head = if i > 0 {
                    "    ".to_owned()
                } else {
                    format!(" {icon}  ")
                };
                fit_line(&[s(head, &format!("{style} bold")), s(l, style)], width, style)
            })
            .collect();
        bottom -= lines.len() as i64;
        if bottom < HEADER_ROWS as i64 {
            break;
        }
        floats.push(ToastLayout {
            id: t.id,
            top: bottom as usize,
            left: cols.saturating_sub(width + 2),
            lines,
            width,
        });
    }
    floats
}
