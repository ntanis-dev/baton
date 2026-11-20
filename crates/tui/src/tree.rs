//! The sidebar: one list of everything there is to look at: Overview; the accounts in priority order,
//! each with its mark, its compact 5-hour and weekly use, and a blocked or signed-out mark; the live
//! sessions, then the recent ones folded under "+ n recent"; Policy; Stats.

use baton_core::charts::usage_tone;
use baton_core::config::Mark;
use baton_core::lines::{Line, Span, fit_line, line_width, p, restyle, s, wrap_line};
use baton_core::policy::{AccountState, WindowState};
use baton_core::text::{round, span, width};
use baton_core::theme::Glyphs;

use crate::data::SessionInfo;
use crate::fuzzy::fuzzy_matches;
use crate::layout::{SIDEBAR_COLS, body_height, is_narrow};
use crate::state::{Focus, State, accounts, live_sessions, recent_sessions, signed_out};

/// What a sidebar row is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RowKind {
    /// A section's title.
    Heading,
    /// A blank line between sections.
    Gap,
    /// The Overview.
    Overview,
    /// An account.
    Account,
    /// A session.
    Session,
    /// The fold of the recent sessions.
    More,
    /// The Policy.
    Policy,
    /// The Stats.
    Stats,
    /// "Nothing matches."
    None,
}

/// A row of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// What selects it: `overview`, `acct:<id>`, `sess:<id>`, `more:sessions`, `policy`, `stats`.
    pub id: String,
    /// What it is.
    pub kind: RowKind,
    /// How far it is indented, in steps of two cells.
    pub depth: usize,
    /// Its sign.
    pub icon: Option<Span>,
    /// Its name. Never cut.
    pub name: String,
    /// The style of its name.
    pub name_style: String,
    /// What follows the name at the right edge; dropped first when room is short.
    pub tail: Line,
    /// A shorter tail to try before dropping it.
    pub short_tail: Option<Line>,
    /// The fold it belongs to.
    pub parent: Option<String>,
    /// Whether it can be selected.
    pub selectable: bool,
}

/// A section's title, with a count when there is one.
fn heading(id: &str, text: &str, count: usize) -> Row {
    Row {
        id: id.to_owned(),
        kind: RowKind::Heading,
        depth: 0,
        icon: None,
        name: text.to_owned(),
        name_style: "muted bold".to_owned(),
        tail: if count > 0 {
            vec![s(count.to_string(), "muted")]
        } else {
            Vec::new()
        },
        short_tail: None,
        parent: None,
        selectable: false,
    }
}

/// A blank line between sections.
fn gap(id: &str) -> Row {
    Row {
        id: id.to_owned(),
        kind: RowKind::Gap,
        depth: 0,
        icon: None,
        name: String::new(),
        name_style: String::new(),
        tail: Vec::new(),
        short_tail: None,
        parent: None,
        selectable: false,
    }
}

/// The sign of an account's mark.
pub fn mark_icon(mark: Mark, g: &Glyphs) -> Span {
    match mark {
        Mark::Preferred => s(g.preferred, "baton bold"),
        Mark::LastResort => s(g.last_resort, "muted"),
        Mark::Normal => s(g.normal, "muted"),
    }
}

/// An account's tail: "5h 42 · wk 63" when it fits, else the higher of the two with its window
/// ("wk 63%"); a blocked account shows when it is back, a signed-out one "sign in".
fn account_tail(state: &State, a: &AccountState, g: &Glyphs) -> (Line, Option<Line>) {
    if signed_out(state, &a.id) {
        return (vec![s("sign in", "warn")], None);
    }
    if a.blocked {
        let back = match a.available_at.filter(|_| a.available_known) {
            Some(at) => span(Some(at - state.now)),
            None => "?".to_owned(),
        };
        // Only a model's own limit: its name, in amber. Chats on other models still run here.
        if let Some(model) = a.limited_models.first() {
            return (
                vec![s(format!("{model} {} {back}", g.blocked), "warn")],
                Some(vec![s(format!("{} {back}", g.blocked), "warn")]),
            );
        }
        return (vec![s(format!("{} {back}", g.blocked), "err")], None);
    }
    let five = &a.windows.five_hour;
    // The week every model draws on: a model's own limit is said when it blocks.
    let week = &a.weekly_all;
    let n = |w: &WindowState| {
        w.used
            .map_or_else(|| "-".to_owned(), |used| format!("{}", round(used) as i64))
    };
    let tone = |w: &WindowState| usage_tone(w.used, w.threshold);
    // A plan with no 5-hour limit has the week alone.
    if five.absent {
        return (
            vec![s("wk ", "muted"), s(format!("{}%", n(week)), tone(week))],
            None,
        );
    }
    let (label, higher) = if week.used.unwrap_or(-1.0) > five.used.unwrap_or(-1.0) {
        ("wk", week)
    } else {
        ("5h", five)
    };
    (
        vec![
            s("5h ", "muted"),
            s(n(five), tone(five)),
            s(" · ", "muted"),
            s("wk ", "muted"),
            s(n(week), tone(week)),
        ],
        Some(vec![
            s(format!("{label} "), "muted"),
            s(format!("{}%", n(higher)), tone(higher)),
        ]),
    )
}

/// The sign of a session: waiting or moving, working, idle, or recent.
pub fn session_icon(x: &SessionInfo, g: &Glyphs) -> Span {
    if matches!(x.wrapper_state.as_deref(), Some("waiting" | "countdown")) {
        s(g.diamond, "warn bold")
    } else if x.turn_state.as_deref() == Some("running") {
        s(g.spinner[0], "accent spin")
    } else if x.live {
        s(g.dot, "ok")
    } else {
        s(g.ring, "muted")
    }
}

/// A session's row.
fn session_row(x: &SessionInfo, g: &Glyphs, depth: usize, parent: Option<&str>) -> Row {
    Row {
        id: format!("sess:{}", x.id),
        kind: RowKind::Session,
        depth,
        icon: Some(session_icon(x, g)),
        name: x.name.clone(),
        name_style: if x.live { "bold".to_owned() } else { String::new() },
        tail: match &x.account {
            Some(account) => vec![s(x.account_name.as_deref().unwrap_or(account), "muted")],
            None => Vec::new(),
        },
        short_tail: None,
        parent: parent.map(str::to_owned),
        selectable: true,
    }
}

/// A row of one of the sections that have no sign: every glyph in the sidebar means one thing.
fn section_row(id: &str, kind: RowKind, name: &str, name_style: &str, tail: Line) -> Row {
    Row {
        id: id.to_owned(),
        kind,
        depth: 0,
        icon: Some(p(" ")),
        name: name.to_owned(),
        name_style: name_style.to_owned(),
        tail,
        short_tail: None,
        parent: None,
        selectable: true,
    }
}

/// The sidebar's rows under the state's filter.
pub fn tree_rows(state: &State) -> Vec<Row> {
    rows_of(state, false)
}

/// The sidebar's rows; `everything` ignores the filter and unfolds every group.
pub fn rows_of(state: &State, everything: bool) -> Vec<Row> {
    let g = state.glyphs.glyphs();
    let query = if everything { "" } else { state.filter.trim() };
    let matches = |names: &[&str]| query.is_empty() || fuzzy_matches(query, names);
    let mut sections: Vec<Vec<Row>> = Vec::new();
    let data = state.data.as_deref();
    if matches(&["Overview"]) {
        sections.push(vec![section_row(
            "overview",
            RowKind::Overview,
            "Overview",
            "brand bold",
            Vec::new(),
        )]);
    }
    let list: Vec<&AccountState> = accounts(state)
        .iter()
        .filter(|a| matches(&[a.name.as_str(), a.mark.as_str()]))
        .collect();
    if !list.is_empty() {
        let mut rows = vec![heading("h:accounts", "Accounts", list.len())];
        for a in list {
            let (tail, short_tail) = account_tail(state, a, g);
            let next = data.is_some_and(|data| data.next.account.as_deref() == Some(a.id.as_str()));
            rows.push(Row {
                id: format!("acct:{}", a.id),
                kind: RowKind::Account,
                depth: 0,
                icon: Some(mark_icon(a.mark, g)),
                name: a.name.clone(),
                name_style: if next { "bold".to_owned() } else { String::new() },
                tail,
                short_tail,
                parent: None,
                selectable: true,
            });
        }
        sections.push(rows);
    }
    let shown = |x: &&SessionInfo| {
        matches(&[
            x.name.as_str(),
            x.account_name.as_deref().unwrap_or(""),
            x.id.as_str(),
        ])
    };
    let live: Vec<&SessionInfo> = live_sessions(state).into_iter().filter(shown).collect();
    let recent: Vec<&SessionInfo> = recent_sessions(state).into_iter().filter(shown).collect();
    if !live.is_empty() || !recent.is_empty() {
        let open = everything || !query.is_empty() || state.open_sessions;
        let mut rows = vec![heading("h:sessions", "Sessions", live.len())];
        rows.extend(live.iter().map(|x| session_row(x, g, 0, None)));
        if !recent.is_empty() {
            if open {
                rows.extend(recent.iter().map(|x| session_row(x, g, 1, Some("more:sessions"))));
            }
            rows.push(Row {
                id: "more:sessions".to_owned(),
                kind: RowKind::More,
                depth: 0,
                icon: None,
                name: if open {
                    format!("{} hide {} recent", g.less, recent.len())
                } else {
                    format!("{} {} recent", g.more, recent.len())
                },
                name_style: "muted".to_owned(),
                tail: Vec::new(),
                short_tail: None,
                parent: None,
                selectable: query.is_empty(),
            });
        }
        sections.push(rows);
    }
    let mut tools = Vec::new();
    if matches(&["Policy", "settings"]) {
        let tail = data
            .map(|data| vec![s(data.policy.mode.as_str(), "muted")])
            .unwrap_or_default();
        tools.push(section_row("policy", RowKind::Policy, "Policy", "bold", tail));
    }
    if matches(&["Stats", "statistics"]) {
        tools.push(section_row("stats", RowKind::Stats, "Stats", "bold", Vec::new()));
    }
    if !tools.is_empty() {
        sections.push(tools);
    }
    let mut rows = Vec::new();
    for (i, section) in sections.into_iter().enumerate() {
        if i > 0 {
            rows.push(gap(&format!("gap:{i}")));
        }
        rows.extend(section);
    }
    if rows.is_empty() {
        rows.push(Row {
            kind: RowKind::None,
            name: "Nothing matches.".to_owned(),
            name_style: "muted".to_owned(),
            ..gap("none")
        });
    }
    rows
}

/// The cells of the selection marker.
const MARKER: usize = 2;

/// The cells before a row's name.
fn prefix_width(row: &Row) -> usize {
    MARKER + row.depth * 2 + if row.icon.is_some() { 2 } else { 0 }
}

/// The cells a row takes with this tail.
pub fn row_width(row: &Row, tail: &[Span]) -> usize {
    prefix_width(row) + width(&row.name) + if tail.is_empty() { 0 } else { 2 + line_width(tail) } + 1
}

/// The width of the sidebar.
pub fn side_width(_state: &State) -> usize {
    SIDEBAR_COLS
}

/// How a row is drawn.
#[derive(Clone, Copy, Debug)]
pub struct RowLook<'a> {
    /// Whether the row is the selected one.
    pub selected: bool,
    /// Whether the list has the keyboard.
    pub focused: bool,
    /// The glyphs to draw with.
    pub g: &'a Glyphs,
    /// The room the name and tail are laid out in, when narrower than the row.
    pub span: Option<usize>,
}

/// The words that make a span faint.
fn is_faint(word: &str) -> bool {
    word == "dim" || word == "muted"
}

/// A line with nothing faint in it.
pub fn undimmed(line: &[Span]) -> Line {
    line.iter()
        .map(|span| Span {
            text: span.text.clone(),
            style: span
                .style
                .split(' ')
                .filter(|w| !w.is_empty() && !is_faint(w))
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect()
}

/// A row as lines `cells` wide. The name is never cut: the tail goes first, then the name wraps under
/// itself.
pub fn row_lines(row: &Row, cells: usize, look: RowLook<'_>) -> Vec<Line> {
    if row.kind == RowKind::Gap {
        return vec![Vec::new()];
    }
    let RowLook {
        selected, focused, g, ..
    } = look;
    let indent = " ".repeat(row.depth * 2);
    let mark = if selected {
        s(format!("{} ", g.arrow), if focused { "" } else { "accent bold" })
    } else {
        p("  ")
    };
    let name_style = if selected && !focused {
        format!("{} bold", row.name_style).trim().to_owned()
    } else {
        row.name_style.clone()
    };
    let name = vec![s(&row.name, &name_style)];
    let mut head = vec![mark, p(indent)];
    if let Some(icon) = &row.icon {
        head.push(icon.clone());
        head.push(p(" "));
    }
    let room = cells.min(look.span.unwrap_or(cells));
    // The full tail, else the short one, else none: the name is never cut.
    let tail = [Some(&row.tail), row.short_tail.as_ref()]
        .into_iter()
        .flatten()
        .find(|t| row_width(row, t) <= room);
    let lines: Vec<Line> = if let Some(tail) = tail {
        let pad = room - row_width(row, tail) + if tail.is_empty() { 0 } else { 2 };
        let mut line = head;
        line.extend(name);
        line.push(p(" ".repeat(pad)));
        line.extend(tail.iter().cloned());
        line.push(p(" "));
        vec![line]
    } else if prefix_width(row) + width(&row.name) < cells {
        let mut line = head;
        line.extend(name);
        vec![line]
    } else {
        let lead = prefix_width(row);
        wrap_line(&name, cells.saturating_sub(1 + lead).max(1), 0)
            .into_iter()
            .enumerate()
            .map(|(i, wrapped)| {
                let mut line = if i > 0 {
                    vec![p(" ".repeat(lead))]
                } else {
                    head.clone()
                };
                line.extend(wrapped);
                line
            })
            .collect()
    };
    // The selected row reads at a glance: focused, a solid accent bar with a bright bold foreground for
    // every part of it (no dim, muted or colour of its own); unfocused, the marker and its text at full
    // strength.
    if selected && focused {
        return lines
            .iter()
            .map(|line| fit_line(&restyle(line, "selected bold"), cells, "selected bold"))
            .collect();
    }
    if selected {
        return lines
            .iter()
            .map(|line| fit_line(&undimmed(line), cells, ""))
            .collect();
    }
    lines.iter().map(|line| fit_line(line, cells, "")).collect()
}

/// The rows that can be selected.
pub fn selectable(rows: &[Row]) -> Vec<&Row> {
    rows.iter().filter(|r| r.selectable).collect()
}

/// The selected row, when it is among the rows.
pub fn selected_row<'a>(state: &State, rows: &'a [Row]) -> Option<&'a Row> {
    rows.iter().find(|r| r.id == state.selected && r.selectable)
}

/// After the data or the filter changes the selection stays on its item while it is there; otherwise
/// the row now at its place takes it.
pub fn reselect(state: &State) -> State {
    let all = tree_rows(state);
    let rows = selectable(&all);
    if rows.is_empty() {
        return state.clone();
    }
    let mut next = state.clone();
    if let Some(at) = rows.iter().position(|r| r.id == state.selected) {
        next.index = at;
        return next;
    }
    let index = state.index.min(rows.len() - 1);
    next.selected = rows[index].id.clone();
    next.index = index;
    next
}

/// A step of the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// To the first row.
    First,
    /// To the last row.
    Last,
    /// By this many rows, down positive.
    By(i32),
}

/// The state with the selection moved.
pub fn move_selection(state: &State, step: Step) -> State {
    let all = tree_rows(state);
    let rows = selectable(&all);
    if rows.is_empty() {
        return state.clone();
    }
    let at = rows.iter().position(|r| r.id == state.selected).unwrap_or(0);
    let last = rows.len() - 1;
    let index = match step {
        Step::First => 0,
        Step::Last => last,
        Step::By(by) => (at as i64 + i64::from(by)).clamp(0, last as i64) as usize,
    };
    let mut next = state.clone();
    next.selected = rows[index].id.clone();
    next.index = index;
    next
}

/// The state with row `id` selected.
pub fn select(state: &State, id: &str) -> State {
    let mut next = state.clone();
    next.selected = id.to_owned();
    let all = tree_rows(&next);
    next.index = selectable(&all).iter().position(|r| r.id == id).unwrap_or(0);
    next
}

/// The sidebar's list as lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideView {
    /// Every line of the list.
    pub lines: Vec<Line>,
    /// For each line: the row that a click on it selects.
    pub owners: Vec<Option<String>>,
    /// The selected row's first line.
    pub from: usize,
    /// The line after the selected row's last one.
    pub to: usize,
}

/// The sidebar's rows as lines `cells` wide.
pub fn side_lines(
    state: &State,
    cells: usize,
    focused: bool,
    g: &Glyphs,
    rows: &[Row],
    span: usize,
) -> SideView {
    let mut view = SideView {
        lines: Vec::new(),
        owners: Vec::new(),
        from: 0,
        to: 0,
    };
    for row in rows {
        let selected = row.selectable && row.id == state.selected;
        let own = row_lines(
            row,
            cells,
            RowLook {
                selected,
                focused,
                g,
                span: Some(span),
            },
        );
        if selected {
            view.from = view.lines.len();
            view.to = view.from + own.len();
        }
        for line in own {
            view.lines.push(line);
            view.owners.push(row.selectable.then(|| row.id.clone()));
        }
    }
    view
}

/// The first line shown of a list of `count` lines in `height` rows, keeping lines `from..to` in view
/// and near the middle.
pub fn side_top(count: usize, from: usize, to: usize, height: usize) -> usize {
    if count <= height {
        return 0;
    }
    let room = height.saturating_sub(to - from);
    from.saturating_sub(room / 2).min(count - height)
}

/// The room a row's name and tail are laid out in.
pub fn side_span(state: &State, cells: usize, rows: &[Row]) -> usize {
    if !is_narrow(state) {
        return cells;
    }
    let widest = rows.iter().map(|r| row_width(r, &r.tail)).max().unwrap_or(0);
    cells.min(widest.clamp(30, 48))
}

/// How far a page moves the selection.
pub fn side_page(state: &State) -> usize {
    body_height(state).saturating_sub(4).max(1)
}

/// Whether the list has the keyboard: the focus is on it and the filter is not being typed.
pub fn side_focused(state: &State) -> bool {
    state.focus == Focus::Side && !state.filtering
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::lines::plain;
    use baton_core::theme::RICH;

    fn row() -> Row {
        Row {
            id: "acct:x".into(),
            kind: RowKind::Account,
            depth: 0,
            icon: Some(p(RICH.normal)),
            name: "a-long-account-name".into(),
            name_style: String::new(),
            tail: vec![p("5h "), p("42"), p(" \u{b7} "), p("wk "), p("63")],
            short_tail: Some(vec![p("wk "), p("63%")]),
            parent: None,
            selectable: true,
        }
    }

    fn look(selected: bool, focused: bool) -> RowLook<'static> {
        RowLook {
            selected,
            focused,
            g: &RICH,
            span: None,
        }
    }

    #[test]
    fn an_accounts_tail_gives_way_before_its_name() {
        let wide = plain(&row_lines(&row(), 40, look(false, false))[0]);
        assert_eq!(wide, "  · a-long-account-name   5h 42 · wk 63 ");
        let narrow = plain(&row_lines(&row(), 32, look(false, false))[0]);
        assert_eq!(narrow, "  · a-long-account-name  wk 63% ");
        let tiny = plain(&row_lines(&row(), 26, look(false, false))[0]);
        assert_eq!(
            tiny, "  · a-long-account-name   ",
            "the tail goes, the name stays whole"
        );
        let wrapped: Vec<String> = row_lines(&row(), 18, look(false, false))
            .iter()
            .map(|l| plain(l))
            .collect();
        assert_eq!(
            wrapped,
            [
                format!("{:<18}", "  · a-long-"),
                format!("{:<18}", "    account-name")
            ]
        );
    }

    #[test]
    fn the_selected_row_is_never_faint() {
        let mut faint = row();
        faint.name_style = "muted".into();
        for cells in [18, 24, 40] {
            for line in row_lines(&faint, cells, look(true, true)) {
                assert_eq!(line_width(&line), cells);
                assert!(line.iter().all(|span| span.style == "selected bold"), "{line:?}");
            }
            for line in row_lines(&faint, cells, look(true, false)) {
                assert!(
                    line.iter().all(|span| !span.style.split(' ').any(is_faint)),
                    "{line:?}"
                );
            }
        }
        assert_eq!(
            row_lines(&gap("gap:1"), 10, look(false, false)),
            vec![Vec::<Span>::new()]
        );
        assert_eq!(
            undimmed(&[s("a", "dim ok"), s("b", "muted")]),
            vec![s("a", "ok"), s("b", "")]
        );
    }

    #[test]
    fn keeping_the_selection_in_view() {
        assert_eq!(side_top(10, 4, 5, 20), 0);
        assert_eq!(side_top(30, 0, 1, 10), 0);
        assert_eq!(side_top(30, 15, 16, 10), 11);
        assert_eq!(side_top(30, 29, 30, 10), 20);
        assert_eq!(side_top(30, 12, 26, 10), 12);
    }
}
