//! A quiet label bar beside the view that uses the symbols.

use baton_core::lines::{Line, line_width, p, s};

use crate::state::State;

/// The signs a kind of view uses, each with what it means, as many as fit in `width` cells.
pub fn legend_line(state: &State, kind: &str, width: usize) -> Line {
    let g = state.glyphs.glyphs();
    let entries: Vec<(&str, &str)> = match kind {
        "account" => vec![
            (g.limit, "Threshold"),
            (g.baton, "Next account"),
            (g.blocked, "Blocked"),
        ],
        "session" | "sessions" => {
            vec![
                (g.spinner[0], "Working"),
                (g.dot, "Idle"),
                (g.diamond, "Wait / move"),
                (g.ring, "Recent"),
            ]
        }
        "overview" | "stats" => vec![
            (g.preferred, "Preferred"),
            (g.last_resort, "Last resort"),
            (g.normal, "Normal"),
            (g.blocked, "Blocked"),
        ],
        _ => Vec::new(),
    };
    let mut line: Line = Vec::new();
    for (sign, label) in entries {
        let mut next: Line = Vec::new();
        if !line.is_empty() {
            next.push(p(" "));
        }
        next.push(s(format!(" {sign} {label} "), "label"));
        if line_width(&line) + line_width(&next) <= width {
            line.extend(next);
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Init, initial_state};
    use baton_core::lines::plain;

    #[test]
    fn the_signs_of_each_view() {
        let state = initial_state(Init::default());
        assert_eq!(
            plain(&legend_line(&state, "account", 100)),
            " ┃ Threshold   ╺━╸ Next account   ⊘ Blocked "
        );
        assert_eq!(
            plain(&legend_line(&state, "sessions", 100)),
            " ⠋ Working   ● Idle   ◆ Wait / move   ○ Recent "
        );
        assert_eq!(
            plain(&legend_line(&state, "overview", 100)),
            " ★ Preferred   ▿ Last resort   · Normal   ⊘ Blocked "
        );
        assert_eq!(legend_line(&state, "policy", 100), Vec::new());
        assert_eq!(legend_line(&state, "empty", 100), Vec::new());
        // What does not fit is left out; a later one that fits still shows.
        assert_eq!(
            plain(&legend_line(&state, "overview", 30)),
            " ★ Preferred   ▿ Last resort "
        );
        assert_eq!(
            plain(&legend_line(&state, "overview", 26)),
            " ★ Preferred   · Normal "
        );
    }
}
