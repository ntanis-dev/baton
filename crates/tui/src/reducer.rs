//! Key handling as a reducer: `reduce(state, action)` gives the new state and the effects the runtime
//! carries out (reload, apply a core function, sign in, redraw, quit).
//!
//! The sidebar keeps navigation focus. Enter opens the selection's visible action menu or the Policy
//! editor; Esc leaves the editor.

use baton_core::clock::Ms;
use serde_json::json;

use crate::actions::{act, open_actions, run_action};
use crate::fuzzy::fuzzy_matches;
use crate::keys::Key;
use crate::layout::{Size, too_small};
use crate::modal::{keys_modal, modal_keys, modal_text};
use crate::mouse::{FieldCommand, Hit, hit_test};
use crate::pane::{Item, current_item, pane_box, pane_model, scroll_pane};
use crate::state::{
    Action, Effect, Focus, ModalKind, MouseEvent, Outcome, State, Tone, expire_toasts, result, result_with,
    toast,
};
use crate::tree::{
    RowKind, Step, move_selection, reselect, select, selectable, selected_row, side_focused, tree_rows,
};
use crate::views::policy::{FIELDS, close_form, form_keys, start_form, with_row};

/// What an action does to the state at the state's own time.
pub fn reduce(state: &State, action: Action) -> Outcome {
    match action {
        Action::SplashEnd => {
            let mut next = state.clone();
            next.splash = false;
            result(next)
        }
        Action::Mouse(event) => on_mouse(state, &event),
        Action::Key(key) => on_key(state, &key),
        Action::Text(text) => on_text(state, &text, false),
        Action::Paste(text) => on_text(state, &text, true),
        Action::Tick => result_with(expire_toasts(state), vec![Effect::Reload]),
        Action::Resize { cols, rows } => {
            let mut next = state.clone();
            next.size = Size { cols, rows };
            result_with(reselect(&next), vec![Effect::Redraw])
        }
        Action::Data(data) => {
            let mut next = state.clone();
            next.data = Some(data);
            result(reselect(&next))
        }
        Action::Auth { account, status } => {
            let mut next = state.clone();
            next.auth.insert(account, status);
            result(next)
        }
        Action::Busy(busy) => {
            let mut next = state.clone();
            next.busy = busy;
            result(next)
        }
        Action::Applied { text, ok } => result_with(
            toast(state, &text, if ok { Tone::Ok } else { Tone::Error }),
            vec![Effect::Reload],
        ),
    }
}

/// What an action does to the state at `now`.
pub fn reduce_at(state: &State, now: Ms, action: Action) -> Outcome {
    if now == state.now {
        return reduce(state, action);
    }
    let mut at = state.clone();
    at.now = now;
    reduce(&at, action)
}

/// Text that arrived at once: typed fast, or pasted.
fn on_text(state: &State, text: &str, pasted: bool) -> Outcome {
    if too_small(state) {
        return result(state.clone());
    }
    if state.splash {
        let mut next = state.clone();
        next.splash = false;
        return result(next);
    }
    if state.modal.is_some() {
        if let Some(typed) = modal_text(state, text) {
            return typed;
        }
    } else if state.filtering || state.focus == Focus::Search {
        let mut next = state.clone();
        next.filtering = true;
        let spaced = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let lead = if text.starts_with(char::is_whitespace) {
            " "
        } else {
            ""
        };
        let trail = if !spaced.is_empty() && text.ends_with(char::is_whitespace) {
            " "
        } else {
            ""
        };
        let filter = format!("{}{lead}{spaced}{trail}", state.filter);
        return set_filter(&next, &filter);
    }
    if pasted {
        return result(state.clone());
    }
    let mut out = result(state.clone());
    let mut effects = Vec::new();
    for ch in text.chars() {
        let key = if ch == '\r' || ch == '\n' {
            Key::Enter
        } else {
            Key::Char(ch)
        };
        out = on_key(&out.state, &key);
        effects.append(&mut out.effects);
    }
    result_with(out.state, effects)
}

/// When the selection moves, the policy form closes.
fn settle(before: &State, out: Outcome) -> Outcome {
    if out.state.selected == before.selected || out.state.form.is_none() {
        return out;
    }
    result_with(close_form(&out.state), out.effects)
}

/// A key press.
fn on_key(state: &State, key: &Key) -> Outcome {
    let quits = |key: &Key| {
        if key.is('q') {
            vec![Effect::Quit]
        } else {
            Vec::new()
        }
    };
    if *key == Key::Ctrl('c') {
        return result_with(state.clone(), vec![Effect::Quit]);
    }
    if too_small(state) {
        return result_with(state.clone(), quits(key));
    }
    if state.splash {
        let mut next = state.clone();
        next.splash = false;
        return result(next);
    }
    if state.modal.is_some() {
        return modal_keys(state, key);
    }
    if state.filtering {
        return settle(state, filter_keys(state, key));
    }
    if state.focus == Focus::Search
        && let Some(typed) = key.printable().filter(|c| *c != '/' && *c != 'q')
    {
        let mut next = state.clone();
        next.filtering = true;
        return set_filter(&next, &format!("{}{typed}", state.filter));
    }
    if state.data.is_none() {
        return result_with(state.clone(), quits(key));
    }
    let own = if state.focus == Focus::Pane {
        pane_keys(state, key)
    } else {
        side_keys(state, key)
    };
    settle(state, own.unwrap_or_else(|| global_keys(state, key)))
}

/// A mouse event.
fn on_mouse(state: &State, event: &MouseEvent) -> Outcome {
    let Some(target) = hit_test(state, event) else {
        return result(state.clone());
    };
    match target {
        Hit::Dismiss => {
            let mut next = state.clone();
            next.splash = false;
            result(next)
        }
        Hit::Toast(id) => {
            let mut next = state.clone();
            next.toasts.retain(|t| t.id != id);
            result(next)
        }
        Hit::ModalScroll(line) => {
            let mut next = state.clone();
            if let Some(modal) = next.modal.as_mut() {
                modal.scroll = Some(line);
            }
            result(next)
        }
        Hit::ModalIndex(index) => {
            let mut next = state.clone();
            next.modal = state.modal.as_ref().map(|modal| {
                let mut moved = modal.with_index(index);
                moved.scroll = None;
                moved
            });
            result(next)
        }
        // A menu, a pick list and a confirmation close (the confirmation as a No). A line of text and
        // the thresholds stay: what was typed or set there would be lost.
        Hit::Outside => match state.modal.as_ref().map(|modal| &modal.kind) {
            Some(
                ModalKind::Actions { .. }
                | ModalKind::Choice { .. }
                | ModalKind::Confirm { .. }
                | ModalKind::Keys,
            ) => modal_keys(state, &Key::Escape),
            _ => result(state.clone()),
        },
        Hit::Choice(index) => {
            let mut next = state.clone();
            next.modal = state.modal.as_ref().map(|modal| modal.with_index(index));
            on_key(&next, &Key::Enter)
        }
        Hit::RunAction { command, .. } => run_action(state, &command),
        Hit::Threshold { index, command } => {
            let mut next = state.clone();
            next.modal = state.modal.as_ref().map(|modal| modal.with_index(index));
            match command {
                Some(key) => on_key(&next, &key),
                None => result(next),
            }
        }
        Hit::Select(id) => {
            if id == "add-account" {
                return global_keys(state, &Key::Named(id));
            }
            // A click on the row that is already selected, while the list has the keys, does what Enter
            // does on it; so does a right-click on any row, at once. The fold acts on its first click.
            let enters =
                id == "more:sessions" || event.button == 2 || (side_focused(state) && state.selected == id);
            let mut next = state.clone();
            if !selectable(&tree_rows(&next)).iter().any(|row| row.id == id) {
                next.filter = String::new();
                next.filtering = false;
            }
            let mut next = close_form(&next);
            next.focus = Focus::Side;
            next.filtering = false;
            let next = select(&next, &id);
            if enters {
                return side_keys(&next, &Key::Enter).unwrap_or_else(|| result(next));
            }
            result(next)
        }
        Hit::Scroll { delta, side } => {
            if delta == 0 {
                return result(state.clone());
            }
            if side {
                let mut next = close_form(state);
                next.focus = Focus::Side;
                return settle(state, result(move_selection(&next, Step::By(delta))));
            }
            let mut next = state.clone();
            for _ in 0..delta.unsigned_abs() {
                next = scroll_from_side(&next, if delta > 0 { &Key::Down } else { &Key::Up });
            }
            result(next)
        }
        Hit::Field { index, command } => {
            let next = with_row(&focus_pane(state), index);
            match command {
                Some(choice @ (FieldCommand::HomeChoice | FieldCommand::EndChoice)) => {
                    let mut next = next;
                    let mode = FIELDS.iter().position(|f| f.key == "mode");
                    if let Some(slot) = next.form.as_mut().and_then(|form| form.values.get_mut(mode?)) {
                        *slot = json!(if choice == FieldCommand::HomeChoice {
                            "smart"
                        } else {
                            "order"
                        });
                    }
                    result(next)
                }
                Some(FieldCommand::Left) => on_key(&next, &Key::Left),
                Some(FieldCommand::Right) => on_key(&next, &Key::Right),
                Some(FieldCommand::Enter) => on_key(&next, &Key::Enter),
                None => result(next),
            }
        }
        Hit::Command(key) => {
            if key.is('q') || key.is('/') || key.is('?') || key.is('r') {
                return global_keys(state, &key);
            }
            on_key(state, &key)
        }
    }
}

/// The state with the pane under the keyboard; the Policy opens its form.
fn focus_pane(state: &State) -> State {
    let mut next = state.clone();
    next.focus = Focus::Pane;
    if current_item(state) == Item::Policy && next.form.is_none() {
        // The form opens on its first field, from the top.
        next.views.remove(&Item::Policy.key());
        next.form = state.data.as_ref().map(|data| start_form(&data.policy));
    }
    next
}

/// PgUp/PgDn from the sidebar scroll the right pane (the sidebar keeps the keys).
fn scroll_from_side(state: &State, key: &Key) -> State {
    let item = current_item(state);
    let pane = pane_box(state);
    scroll_pane(state, key, &pane_model(state, &item, &pane), &pane).unwrap_or_else(|| state.clone())
}

/// Search and Add account stay pinned outside the scrolling list, but are part of its arrow
/// navigation. Focusing either preserves the detail view.
fn step_sidebar(state: &State, step: i32) -> State {
    let to_side = |state: &State, step: Step| {
        let mut next = state.clone();
        next.focus = Focus::Side;
        move_selection(&next, step)
    };
    match state.focus {
        Focus::Search => {
            return if step < 0 {
                state.clone()
            } else {
                to_side(state, Step::First)
            };
        }
        Focus::Add => {
            return if step > 0 {
                state.clone()
            } else {
                to_side(state, Step::Last)
            };
        }
        _ => {}
    }
    let all = tree_rows(state);
    let rows = selectable(&all);
    let at = rows.iter().position(|row| row.id == state.selected);
    let mut next = state.clone();
    if step < 0 && at.is_none_or(|at| at == 0) {
        next.focus = Focus::Search;
        return next;
    }
    if step > 0 && at == rows.len().checked_sub(1) {
        next.focus = Focus::Add;
        return next;
    }
    move_selection(state, Step::By(step))
}

/// The keys of the sidebar, or None for a key it does not use.
fn side_keys(state: &State, key: &Key) -> Option<Outcome> {
    let to_side = |step: Step| {
        let mut next = state.clone();
        next.focus = Focus::Side;
        result(move_selection(&next, step))
    };
    Some(match key {
        Key::Up => result(step_sidebar(state, -1)),
        Key::Down => result(step_sidebar(state, 1)),
        Key::PageUp | Key::PageDown => result(scroll_from_side(state, key)),
        Key::Home => to_side(Step::First),
        Key::End => to_side(Step::Last),
        // Right unfolds the recent sessions, then goes to the first of them; Left folds them, and on
        // one of them goes to their fold.
        Key::Left | Key::Right if state.focus == Focus::Side => {
            let rows = tree_rows(state);
            let row = selected_row(state, &rows)?;
            let right = *key == Key::Right;
            if row.kind == RowKind::More && right != state.open_sessions {
                let mut next = state.clone();
                next.open_sessions = right;
                return Some(result(reselect(&next)));
            }
            let to = if right {
                rows.iter().find(|r| r.parent.as_deref() == Some(row.id.as_str()))
            } else {
                rows.iter().find(|r| Some(r.id.as_str()) == row.parent.as_deref())
            };
            result(select(state, &to.filter(|r| r.selectable)?.id))
        }
        Key::Enter => {
            if state.focus == Focus::Search {
                return Some(global_keys(state, &Key::Char('/')));
            }
            if state.focus == Focus::Add {
                return Some(global_keys(state, &Key::Named("add-account".to_owned())));
            }
            let rows = tree_rows(state);
            let Some(row) = selected_row(state, &rows) else {
                return Some(result(state.clone()));
            };
            if row.kind == RowKind::More {
                let mut next = state.clone();
                next.open_sessions = !state.open_sessions;
                return Some(result(reselect(&next)));
            }
            let item = current_item(state);
            match item {
                Item::Overview | Item::Account(_) | Item::Session(_) => open_actions(state, &item),
                // Only a pane with something to do takes the keys.
                Item::Policy => result(focus_pane(state)),
                _ => result(state.clone()),
            }
        }
        Key::Escape => result(if state.filter.is_empty() {
            state.clone()
        } else {
            clear_filter(state)
        }),
        _ => return None,
    })
}

/// The keys of the pane, or None for a key it does not use.
fn pane_keys(state: &State, key: &Key) -> Option<Outcome> {
    let item = current_item(state);
    if item == Item::Policy
        && state.form.is_some()
        && let Some(used) = form_keys(state, key)
    {
        return Some(used);
    }
    let pane = pane_box(state);
    let model = pane_model(state, &item, &pane);
    if let Some(scrolled) = scroll_pane(state, key, &model, &pane) {
        return Some(result(scrolled));
    }
    if *key == Key::Escape {
        let mut next = close_form(state);
        next.focus = Focus::Side;
        return Some(result(next));
    }
    None
}

/// The keys that work wherever the keyboard is.
fn global_keys(state: &State, key: &Key) -> Outcome {
    match key {
        Key::Named(name) if name == "add-account" => {
            let mut next = close_form(state);
            next.focus = Focus::Add;
            act(&next, &Item::Overview, &Key::Char('a')).unwrap_or_else(|| result(next))
        }
        Key::Char('q') => result_with(state.clone(), vec![Effect::Quit]),
        // The usage now: the account's when one is selected, else every account's.
        Key::Char('r') => {
            let account = match current_item(state) {
                Item::Account(id) => Some(id),
                _ => None,
            };
            result_with(
                state.clone(),
                vec![Effect::Refresh {
                    account,
                    quiet: false,
                }],
            )
        }
        Key::Char('?') => {
            let mut next = state.clone();
            next.modal = Some(keys_modal());
            result(next)
        }
        Key::Ctrl('l') => result_with(state.clone(), vec![Effect::Redraw]),
        Key::Char('/') => {
            let mut next = close_form(state);
            next.filtering = true;
            next.focus = Focus::Search;
            result(next)
        }
        Key::Escape => result(if state.filter.is_empty() {
            state.clone()
        } else {
            clear_filter(state)
        }),
        _ => result(state.clone()),
    }
}

/// The state with this filter; the selection goes to the first row that matches it by name, else to
/// the first row there is.
fn set_filter(state: &State, filter: &str) -> Outcome {
    if filter.is_empty() {
        let mut next = state.clone();
        next.filter = String::new();
        next.filtering = false;
        next.focus = Focus::Search;
        return result(reselect(&next));
    }
    let mut next = state.clone();
    next.filter = filter.to_owned();
    let query = filter.trim();
    if query.is_empty() {
        return result(next);
    }
    let all = tree_rows(&next);
    let rows = selectable(&all);
    let hit = rows
        .iter()
        .find(|row| fuzzy_matches(query, &[row.name.as_str()]))
        .or(rows.first());
    if let Some(hit) = hit {
        next = select(&next, &hit.id);
    }
    result(next)
}

/// The state with the filter gone and the list under the keyboard.
fn clear_filter(state: &State) -> State {
    let mut next = state.clone();
    next.filter = String::new();
    next.filtering = false;
    next.focus = Focus::Side;
    reselect(&next)
}

/// The keys while the filter is typed.
fn filter_keys(state: &State, key: &Key) -> Outcome {
    let to_side = || {
        let mut next = state.clone();
        next.filtering = false;
        next.focus = Focus::Side;
        result(next)
    };
    match key {
        Key::Enter | Key::Down => to_side(),
        Key::Escape => result(clear_filter(state)),
        Key::Backspace => {
            let mut chars: Vec<char> = state.filter.chars().collect();
            chars.pop();
            set_filter(state, &chars.into_iter().collect::<String>())
        }
        Key::Ctrl('u') => set_filter(state, ""),
        other => match other.printable() {
            Some(typed) => set_filter(state, &format!("{}{typed}", state.filter)),
            None => result(state.clone()),
        },
    }
}
