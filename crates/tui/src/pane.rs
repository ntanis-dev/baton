//! The right pane shows what the sidebar has selected.

use baton_core::lines::{Line, line_width, s, wrap_line};

use crate::keys::Key;
use crate::layout::{Size, body_height, frame_cols, inner};
use crate::state::State;
use crate::tree::{selected_row, side_width, tree_rows};
use crate::views;

/// What the pane shows.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Item {
    /// Nothing: the filter matches no row.
    Empty,
    /// The Overview.
    Overview,
    /// An account, by its id.
    Account(String),
    /// A session, by its id.
    Session(String),
    /// The recent sessions.
    Sessions,
    /// The Policy.
    Policy,
    /// The Stats.
    Stats,
}

impl Item {
    /// The item's kind: `empty`, `overview`, `account`, `session`, `sessions`, `policy`, `stats`.
    pub fn kind(&self) -> &'static str {
        match self {
            Item::Empty => "empty",
            Item::Overview => "overview",
            Item::Account(_) => "account",
            Item::Session(_) => "session",
            Item::Sessions => "sessions",
            Item::Policy => "policy",
            Item::Stats => "stats",
        }
    }

    /// What keys the item's scroll position: the sidebar row's id.
    pub fn key(&self) -> String {
        match self {
            Item::Empty => "empty".to_owned(),
            Item::Overview => "overview".to_owned(),
            Item::Account(id) => format!("acct:{id}"),
            Item::Session(id) => format!("sess:{id}"),
            Item::Sessions => "more:sessions".to_owned(),
            Item::Policy => "policy".to_owned(),
            Item::Stats => "stats".to_owned(),
        }
    }
}

/// What the pane shows for the state's selection.
pub fn current_item(state: &State) -> Item {
    if !state.filter.trim().is_empty() && selected_row(state, &tree_rows(state)).is_none() {
        return Item::Empty;
    }
    let id = state.selected.as_str();
    if let Some(account) = id.strip_prefix("acct:") {
        return Item::Account(account.to_owned());
    }
    if let Some(session) = id.strip_prefix("sess:") {
        return Item::Session(session.to_owned());
    }
    match id {
        "more:sessions" => Item::Sessions,
        "policy" => Item::Policy,
        "stats" => Item::Stats,
        _ => Item::Overview,
    }
}

/// The pane's place: its size and the room inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaneBox {
    /// Its width.
    pub width: usize,
    /// Its height.
    pub height: usize,
    /// The room for its body: columns and rows.
    pub room: Size,
}

/// The pane's place in the state's terminal.
pub fn pane_box(state: &State) -> PaneBox {
    let cols = frame_cols(state);
    let height = body_height(state);
    let width = cols.saturating_sub(side_width(state)).max(10);
    PaneBox {
        width,
        height,
        room: inner(width, height),
    }
}

/// A part of a body line that a click selects something through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// Its first column in the body.
    pub x: usize,
    /// Its width.
    pub width: usize,
    /// The sidebar row it selects (`add-account` opens the Add account dialog).
    pub select: String,
}

/// A line of a pane's body, with what a click on it does.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyLine {
    /// The line.
    pub line: Line,
    /// What a click on it selects.
    pub links: Vec<Link>,
}

impl From<Line> for BodyLine {
    fn from(line: Line) -> BodyLine {
        BodyLine {
            line,
            links: Vec::new(),
        }
    }
}

/// Lines as body lines that a click does nothing on.
pub fn body(lines: Vec<Line>) -> Vec<BodyLine> {
    lines.into_iter().map(BodyLine::from).collect()
}

/// A pane's content as its view gives it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewModel {
    /// The title.
    pub title: Line,
    /// The note under the title.
    pub right: Line,
    /// The body.
    pub body: Vec<BodyLine>,
}

/// A pane's content, laid out for its room. Nothing is cut: a line wider than the pane wraps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneModel {
    /// The title.
    pub title: Line,
    /// The note under the title.
    pub right: Line,
    /// The body's lines.
    pub body: Vec<Line>,
    /// For each line of the body: what a click on it selects.
    pub targets: Vec<Vec<Link>>,
    /// What keys the pane's scroll position.
    pub view_key: String,
}

/// The pane's content for an item in a box.
pub fn pane_model(state: &State, item: &Item, pane: &PaneBox) -> PaneModel {
    let view = if state.data.is_some() {
        views::model(state, item, pane)
    } else {
        ViewModel::default()
    };
    let width = pane.room.cols;
    let mut title = view.title;
    let mut lines = view.body;
    if line_width(&title) > width {
        let mut moved = body(wrap_line(&title, width, 0));
        moved.push(BodyLine::default());
        moved.extend(lines);
        lines = moved;
        title = vec![s(
            if matches!(item, Item::Account(_)) {
                "Account"
            } else {
                "Session"
            },
            "bold",
        )];
    }
    let mut model = PaneModel {
        title,
        right: view.right,
        view_key: item.key(),
        ..PaneModel::default()
    };
    for row in lines {
        let wrapped = if line_width(&row.line) > width {
            wrap_line(&row.line, width, 2)
        } else {
            vec![row.line]
        };
        for line in wrapped {
            model.targets.push(row.links.clone());
            model.body.push(line);
        }
    }
    model
}

/// The part of a pane's body that shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The rows there are.
    pub room: usize,
    /// The first line shown.
    pub top: usize,
    /// The furthest the pane scrolls.
    pub max: usize,
    /// The lines shown.
    pub lines: Vec<Line>,
    /// How many lines are above.
    pub above: usize,
    /// How many lines are below.
    pub below: usize,
    /// How many lines there are in all.
    pub total: usize,
}

/// The pane's scroll position: its first shown line.
pub fn scroll_of(state: &State, model: &PaneModel) -> usize {
    state.views.get(&model.view_key).copied().unwrap_or(0)
}

/// The part of the pane's body that shows.
pub fn pane_window(state: &State, model: &PaneModel, pane: &PaneBox) -> Window {
    let room = pane.room.rows.max(1);
    let total = model.body.len();
    let max = total.saturating_sub(room);
    let top = scroll_of(state, model).min(max);
    Window {
        room,
        top,
        max,
        lines: model.body.iter().skip(top).take(room).cloned().collect(),
        above: top,
        below: total.saturating_sub(top + room),
        total,
    }
}

/// Whether the right pane is taller than its room (PgUp/PgDn scroll it).
pub fn pane_overflows(state: &State) -> bool {
    if state.data.is_none() {
        return false;
    }
    let pane = pane_box(state);
    let w = pane_window(state, &pane_model(state, &current_item(state), &pane), &pane);
    w.total > w.room
}

/// The state with the pane scrolled by a key, or None for a key that does not scroll.
pub fn scroll_pane(state: &State, key: &Key, model: &PaneModel, pane: &PaneBox) -> Option<State> {
    let w = pane_window(state, model, pane);
    let page = w.room.saturating_sub(1).max(1);
    let top = match key {
        Key::Up => w.top.saturating_sub(1),
        Key::Down => w.top + 1,
        Key::PageUp => w.top.saturating_sub(page),
        Key::PageDown => w.top + page,
        Key::Home => 0,
        Key::End => w.max,
        _ => return None,
    };
    let mut next = state.clone();
    next.views.insert(model.view_key.clone(), top.min(w.max));
    Some(next)
}

/// The height of the pane.
pub fn pane_height(state: &State) -> usize {
    body_height(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Init, initial_state};
    use baton_core::lines::p;

    #[test]
    fn what_the_selection_shows() {
        let mut state = initial_state(Init::default());
        assert_eq!(current_item(&state), Item::Overview);
        for (selected, item, kind) in [
            ("acct:work", Item::Account("work".into()), "account"),
            ("sess:abc", Item::Session("abc".into()), "session"),
            ("more:sessions", Item::Sessions, "sessions"),
            ("policy", Item::Policy, "policy"),
            ("stats", Item::Stats, "stats"),
            ("anything", Item::Overview, "overview"),
        ] {
            state.selected = selected.to_owned();
            assert_eq!(current_item(&state), item);
            assert_eq!(item.kind(), kind);
            if selected != "anything" {
                assert_eq!(item.key(), selected);
            }
        }
        state.filter = "zzz".to_owned();
        assert_eq!(current_item(&state), Item::Empty);
        assert_eq!(Item::Empty.key(), "empty");
    }

    #[test]
    fn the_panes_place() {
        let state = initial_state(Init {
            size: Size { cols: 120, rows: 30 },
            ..Init::default()
        });
        assert_eq!(
            pane_box(&state),
            PaneBox {
                width: 74,
                height: 28,
                room: Size { cols: 68, rows: 23 }
            }
        );
    }

    #[test]
    fn scrolling_a_body() {
        let state = initial_state(Init {
            size: Size { cols: 120, rows: 30 },
            ..Init::default()
        });
        let pane = pane_box(&state);
        let model = PaneModel {
            body: (0..60).map(|i| vec![p(i.to_string())]).collect(),
            targets: vec![Vec::new(); 60],
            view_key: "stats".into(),
            ..PaneModel::default()
        };
        let w = pane_window(&state, &model, &pane);
        assert_eq!(
            (w.room, w.top, w.max, w.above, w.below, w.total, w.lines.len()),
            (23, 0, 37, 0, 37, 60, 23)
        );
        let down = scroll_pane(&state, &Key::PageDown, &model, &pane).unwrap();
        assert_eq!(down.views["stats"], 22);
        let end = scroll_pane(&down, &Key::End, &model, &pane).unwrap();
        assert_eq!(end.views["stats"], 37);
        assert_eq!(
            scroll_pane(&end, &Key::Down, &model, &pane).unwrap().views["stats"],
            37
        );
        assert_eq!(
            scroll_pane(&end, &Key::PageUp, &model, &pane).unwrap().views["stats"],
            15
        );
        assert_eq!(
            scroll_pane(&end, &Key::Home, &model, &pane).unwrap().views["stats"],
            0
        );
        assert_eq!(
            scroll_pane(&state, &Key::Up, &model, &pane).unwrap().views["stats"],
            0
        );
        assert_eq!(scroll_pane(&state, &Key::Enter, &model, &pane), None);
        let last = pane_window(&end, &model, &pane);
        assert_eq!((last.top, last.above, last.below), (37, 37, 0));
    }
}
