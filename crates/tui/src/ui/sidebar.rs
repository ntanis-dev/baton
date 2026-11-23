//! The sidebar: the mark and the pinned search control, the list, and the pinned Add account control.

use baton_core::art::branding;
use baton_core::lines::{Line, fit_line, p, restyle, s};
use baton_core::theme::Theme;

use super::kit::{ON_TERMINAL, block, on_surface};
use crate::layout::{SIDE_FOOTER_ROWS, brand_rows, compact};
use crate::modal::field_window;
use crate::state::{Focus, State};
use crate::tree::{side_focused, side_lines, side_span, side_top, tree_rows};

/// The colour of the rule between the sidebar and the pane.
const RULE: &str = "#2b3037";

/// The sidebar as `height` rows of `width` cells: a cell of padding, the content on the surface colour,
/// a cell of padding and the rule at the right edge.
pub fn sidebar(state: &State, theme: &Theme, width: usize, height: usize) -> Vec<Line> {
    let cells = width.saturating_sub(3);
    let rows = tree_rows(state);
    let above = brand_rows(state);
    let room = height.saturating_sub(above + SIDE_FOOTER_ROWS).max(1);
    let view = side_lines(
        state,
        cells,
        side_focused(state),
        theme.g,
        &rows,
        side_span(state, cells, &rows),
    );
    let top = side_top(view.lines.len(), view.from, view.to, room);
    let search: Line = if state.filtering {
        let field = field_window(
            &state.filter,
            state.filter.chars().count(),
            cells.saturating_sub(6),
        );
        vec![
            s("  / ", "accent bold"),
            p(field.before),
            s(field.at, "inverse"),
            p(field.after),
        ]
    } else {
        let typed = !state.filter.is_empty();
        vec![
            p(if state.focus == Focus::Search {
                format!("{} ", theme.g.arrow)
            } else {
                "  ".to_owned()
            }),
            s("/ ", "accent bold"),
            s(
                if typed { state.filter.as_str() } else { "Search" },
                if typed { "text" } else { "muted" },
            ),
        ]
    };
    let control = |line: Line, active: bool| {
        if active {
            fit_line(&restyle(&line, "selected bold"), cells, "selected bold")
        } else {
            line
        }
    };
    let add = vec![
        p(if state.focus == Focus::Add {
            format!("{} ", theme.g.arrow)
        } else {
            "  ".to_owned()
        }),
        s("+ ", "accent bold"),
        s("Add account", "accent"),
    ];
    // The wordmark where there is room for it, the name alone where there is not; the search control
    // is the row before the last either way.
    let mut brand: Vec<Line> = if compact(state) {
        vec![vec![s("  BATON", "brand bold")]]
    } else {
        let mut brand = vec![Vec::new()];
        brand.extend(branding("  "));
        brand
    };
    brand.push(Vec::new());
    brand.push(control(search, state.focus == Focus::Search && !state.filtering));
    brand.push(Vec::new());
    let mut more: Line = Vec::new();
    if top > 0 {
        more.push(s(format!("  {} more above", theme.g.up), "muted"));
    }
    if top + room < view.lines.len() {
        more.push(s(
            format!(
                "{}{} more below",
                if top > 0 { "   " } else { "  " },
                theme.g.down
            ),
            "muted",
        ));
    }
    let shown: Vec<Line> = view.lines.iter().skip(top).take(room).cloned().collect();
    let mut content = block(&brand, cells, Some(above));
    content.extend(block(&shown, cells, Some(room)));
    content.extend(block(
        &[Vec::new(), control(add, state.focus == Focus::Add), more],
        cells,
        Some(SIDE_FOOTER_ROWS),
    ));
    let rule = s(
        "\u{2502}",
        &if theme.color {
            format!("{RULE} {ON_TERMINAL}")
        } else {
            String::new()
        },
    );
    block(&content, cells, Some(height))
        .into_iter()
        .map(|line| {
            let mut row = vec![p(" ")];
            row.extend(line);
            row.push(p(" "));
            let mut row = on_surface(&row);
            row.push(rule.clone());
            fit_line(&row, width, "")
        })
        .collect()
}
