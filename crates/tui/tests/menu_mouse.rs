//! The menu under the mouse: clicks on what the frame shows, the wheel, the splash screen.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::sync::Arc;

use baton_core::art::{LOGO, MOTTO, SLOGAN};
use baton_core::text::width;
use baton_core::theme::GlyphSet;
use baton_testkit::{TestHome, temp_home};
use baton_tui::keys::Key;
use baton_tui::layout::Size;
use baton_tui::reducer::reduce;
use baton_tui::state::{Action, Effect, Focus, ModalKind, MouseEvent, Outcome, State, Submit};
use baton_tui::tree::side_width;
use regex::Regex;
use serde_json::json;

use common::{build_home, frame, menu_state, menu_state_with, press, screen, screens};

fn setup() -> (TestHome, State) {
    let home = temp_home();
    build_home(&home.paths, false);
    let state = menu_state(&home.paths, &[]);
    (home, state)
}

fn mouse(state: &State, x: usize, y: usize) -> Outcome {
    reduce(state, Action::Mouse(MouseEvent::press(x as i32, y as i32)))
}

/// A click of another button: 1 is the middle one, 2 the right one.
fn button(state: &State, button: u8, x: usize, y: usize) -> Outcome {
    let event = MouseEvent {
        button,
        ..MouseEvent::press(x as i32, y as i32)
    };
    reduce(state, Action::Mouse(event))
}

fn wheel(state: &State, x: usize, y: usize, delta: i32) -> Outcome {
    reduce(state, Action::Mouse(MouseEvent::wheel(x as i32, y as i32, delta)))
}

/// Where a frame shows a text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Where {
    Anywhere,
    Side,
    Pane,
}

fn locate(state: &State, text: &str, place: Where) -> (usize, usize) {
    let lines = frame(state);
    for (y, line) in lines.iter().enumerate() {
        for (index, _) in line.match_indices(text) {
            let x = width(&line[..index]);
            let side = x < side_width(state);
            if place == Where::Anywhere || (place == Where::Side) == side {
                return (x, y);
            }
        }
    }
    panic!("Missing control: {text}\n{}", lines.join("\n"));
}

fn click(state: &State, text: &str, place: Where) -> Outcome {
    let (x, y) = locate(state, text, place);
    mouse(state, x, y)
}

fn right_click(state: &State, text: &str, place: Where) -> Outcome {
    let (x, y) = locate(state, text, place);
    button(state, 2, x, y)
}

fn key(state: &State, name: &str) -> Outcome {
    reduce(state, Action::Key(Key::from_name(name)))
}

/// The selection's Actions dialog, then a command of it.
fn action(state: &State, name: &str) -> Outcome {
    key(&key(state, "enter").state, name)
}

fn modal_kind(state: &State) -> Option<&'static str> {
    state.modal.as_ref().map(|modal| modal.kind_name())
}

fn args(out: &Outcome) -> serde_json::Value {
    out.effects[0].to_json()["args"].clone()
}

fn matches(text: &str, pattern: &str) -> bool {
    Regex::new(pattern).unwrap().is_match(text)
}

#[test]
fn accounts_display_on_click_and_there_is_no_enter_navigation_or_resizable_rail() {
    let (home, state) = setup();
    let next = click(&state, "acct2", Where::Side).state;
    assert_eq!(next.selected, "acct:acct2");
    assert_eq!(next.focus, Focus::Side);
    assert!(frame(&next).join("\n").contains("#2 of 4"));
    assert_eq!(press(&next, &home.paths, "enter").state.focus, Focus::Side);
    assert_eq!(side_width(&next), 46);
    let resized = reduce(&next, Action::Resize { cols: 160, rows: 45 }).state;
    assert_eq!(side_width(&resized), 46);
    let arrow = press(&resized, &home.paths, "ctrl+right").state;
    assert_eq!(side_width(&arrow), 46);
    assert!(!matches(
        &frame(&next).join("\n"),
        "Sidebar|Enter.*open|Account relay"
    ));
    assert_eq!(click(&resized, "Policy", Where::Side).state.selected, "policy");
}

#[test]
fn account_menu_commands_preserve_their_dialogs_and_confirmation() {
    let (_home, state) = setup();
    let account = click(&state, "acct2", Where::Side).state;
    assert!(!matches(
        &frame(&account).join("\n"),
        "Remove account|Rename account"
    ));
    let rename = action(&account, "e").state;
    assert_eq!(modal_kind(&rename), Some("input"));
    let edited = key(&rename, "ctrl+u").state;
    let edited = reduce(&edited, Action::Text("Work account".to_owned())).state;
    // The title is not a button.
    let saved = click(&edited, "Rename", Where::Anywhere);
    assert_eq!(saved.effects.len(), 0);
    let (x, y) = locate(&edited, "Rename  Enter", Where::Anywhere);
    let applied = mouse(&edited, x, y);
    assert_eq!(args(&applied), json!({ "id": "acct2", "name": "Work account" }));
    let confirmation = action(&account, "d").state;
    assert_eq!(modal_kind(&confirmation), Some("confirm"));
    // A click beside a confirmation is a No.
    let beside = mouse(&confirmation, 2, 2);
    assert_eq!((modal_kind(&beside.state), beside.effects.len()), (None, 0));
    assert_eq!(click(&confirmation, "No  n", Where::Anywhere).effects.len(), 0);
    assert_eq!(
        args(&click(&confirmation, "Yes  y", Where::Anywhere)),
        json!({ "id": "acct2" })
    );
}

#[test]
fn the_footer_actions_menu_contains_refresh_and_the_header_stays_clear() {
    let (_home, state) = setup();
    let account = click(&state, "acct2", Where::Side).state;
    assert!(!matches(&frame(&account)[..3].join("\n"), "Refresh|Actions"));
    let actions = click(&account, "Actions", Where::Anywhere).state;
    assert_eq!(modal_kind(&actions), Some("actions"));
    assert_eq!(
        click(&actions, "Refresh usage", Where::Anywhere).effects,
        vec![Effect::Refresh {
            account: Some("acct2".to_owned()),
            quiet: false
        }]
    );
    assert!(matches!(
        click(&actions, "Rename", Where::Anywhere)
            .state
            .modal
            .map(|m| m.kind),
        Some(ModalKind::Input {
            submit: Submit::Rename(_),
            ..
        })
    ));
    let mode = click(&actions, "Change mark", Where::Anywhere).state;
    assert_eq!(args(&click(&mode, "2 Normal", Where::Anywhere))["mark"], "normal");
    assert_eq!(
        modal_kind(&click(&actions, "Delete account", Where::Anywhere).state),
        Some("confirm")
    );
}

#[test]
fn policy_controls_edit_directly_and_apply_still_confirms() {
    let (home, state) = setup();
    let paths = &home.paths;
    let visible = press(&press(&state, paths, "end").state, paths, "up").state;
    let mut policy = click(&visible, "Policy", Where::Side).state;
    let original = locate(&policy, "5-hour limit", Where::Pane);
    policy = click(&policy, "order", Where::Pane).state;
    assert_eq!(locate(&policy, "5-hour limit", Where::Pane), original);
    assert!(frame(&policy)[2].contains("1 unsaved"));
    assert_eq!(policy.form.as_ref().unwrap().value("mode"), Some(&json!("order")));
    let field = locate(&policy, "5-hour limit", Where::Pane);
    let line = frame(&policy)[field.1].clone();
    let arrow = width(&line[..line.rfind('\u{2192}').unwrap()]);
    policy = mouse(&policy, arrow, field.1).state;
    assert_eq!(policy.form.as_ref().unwrap().value("fiveHour"), Some(&json!(91)));
    assert_eq!(locate(&policy, "5-hour limit", Where::Pane), original);
    assert!(frame(&policy)[2].contains("2 unsaved"));
    let confirm = click(&policy, "Apply 2 changes", Where::Pane).state;
    assert_eq!(modal_kind(&confirm), Some("confirm"));
    assert_eq!(
        args(&click(&confirm, "Yes  y", Where::Anywhere))["changes"],
        json!({ "mode": "order", "fiveHour": 91 })
    );
}

#[test]
fn dashboard_entries_select_directly_and_the_wheel_scrolls_the_pointed_pane() {
    let (home, state) = setup();
    let account = click(&state, "acct2", Where::Pane).state;
    assert_eq!(account.selected, "acct:acct2");
    let mut stats = press(&state, &home.paths, "end").state;
    let mut data = (**stats.data.as_ref().unwrap()).clone();
    let again = data.stats.accounts.clone();
    data.stats.accounts.extend(again);
    stats.data = Some(Arc::new(data));
    let scrolled = wheel(&stats, side_width(&stats) + 5, 12, 3).state;
    assert_eq!(scrolled.views["stats"], 3);
    let selection = wheel(&state, 3, 12, 2).state;
    assert_eq!(selection.selected, "acct:acct2");
}

#[test]
fn thresholds_hop_choices_and_add_account_remain_clickable() {
    let (home, state) = setup();
    let account = click(&state, "acct2", Where::Side).state;
    let mut thresholds = action(&account, "t").state;
    let row = locate(&thresholds, "› 5-hour", Where::Anywhere);
    let line = frame(&thresholds)[row.1].clone();
    let arrow = width(&line[..line.find('\u{2192}').unwrap()]);
    thresholds = mouse(&thresholds, arrow, row.1).state;
    let Some(ModalKind::Thresholds { values, .. }) = thresholds.modal.as_ref().map(|m| &m.kind) else {
        panic!("the thresholds dialog");
    };
    assert_eq!(values[0], Some(91.0));
    assert_eq!(
        args(&click(&thresholds, "Save  Enter", Where::Anywhere))["fiveHour"],
        91
    );
    let session = menu_state(&home.paths, &screen("session"));
    let choices = action(&session, "H").state;
    assert_eq!(args(&click(&choices, "1 acct1", Where::Anywhere))["to"], "acct1");
    assert_eq!(
        modal_kind(&click(&state, "+ Add account", Where::Side).state),
        Some("input")
    );
}

#[test]
fn a_click_on_the_selected_row_or_a_right_click_does_what_enter_does() {
    let (home, state) = setup();
    let paths = &home.paths;
    // The first click selects; the second, on the row that has the keys, opens its actions.
    let account = click(&state, "acct2", Where::Side).state;
    assert_eq!(
        (account.selected.as_str(), modal_kind(&account)),
        ("acct:acct2", None)
    );
    let again = click(&account, "acct2", Where::Side).state;
    assert_eq!(
        (again.selected.as_str(), modal_kind(&again)),
        ("acct:acct2", Some("actions"))
    );
    // While the list is without the keys, a click on its selected row only gives them back.
    let search = press(&state, paths, "up").state;
    assert_eq!(
        (search.focus, search.selected.as_str()),
        (Focus::Search, "overview")
    );
    let back = click(&search, "Overview", Where::Side).state;
    assert_eq!((back.focus, modal_kind(&back)), (Focus::Side, None));
    assert_eq!(
        modal_kind(&click(&back, "Overview", Where::Side).state),
        Some("actions")
    );
    // The right button selects and acts at once, on a row of the list and on an entry of the dashboard.
    let at_once = right_click(&state, "acct3", Where::Side).state;
    assert_eq!(
        (at_once.selected.as_str(), modal_kind(&at_once)),
        ("acct:acct3", Some("actions"))
    );
    let entry = right_click(&state, "acct2", Where::Pane).state;
    assert_eq!(
        (entry.selected.as_str(), modal_kind(&entry)),
        ("acct:acct2", Some("actions"))
    );
    // The Policy opens its editor the same way; the Stats have nothing to open.
    let stats = press(&state, paths, "end").state;
    let policy = click(&stats, "Policy", Where::Side).state;
    assert_eq!(
        (policy.selected.as_str(), policy.focus, policy.form.is_some()),
        ("policy", Focus::Side, false)
    );
    let editor = click(&policy, "Policy", Where::Side).state;
    assert_eq!((editor.focus, editor.form.is_some()), (Focus::Pane, true));
    let editor = right_click(&stats, "Policy", Where::Side).state;
    assert_eq!((editor.focus, editor.form.is_some()), (Focus::Pane, true));
    assert_eq!(click(&stats, "Stats", Where::Side).state, stats);
    assert_eq!(right_click(&policy, "Stats", Where::Side).state, stats);
    // The fold of the recent sessions acts on its first click, once, with either button.
    let open = click(&state, "2 recent", Where::Side).state;
    assert!(open.open_sessions);
    assert!(right_click(&state, "2 recent", Where::Side).state.open_sessions);
    assert!(!click(&open, "hide 2 recent", Where::Side).state.open_sessions);
    // A button acts with the right button too; the middle one does nothing.
    assert_eq!(
        modal_kind(&right_click(&state, "+ Add account", Where::Side).state),
        Some("input")
    );
    let (x, y) = locate(&state, "acct2", Where::Side);
    assert_eq!(button(&state, 1, x, y).state, state);
}

#[test]
fn a_click_beside_a_menu_or_a_pick_list_closes_it_and_what_is_typed_stays() {
    let (_home, state) = setup();
    let account = click(&state, "acct2", Where::Side).state;
    let actions = key(&account, "enter").state;
    assert_eq!(modal_kind(&actions), Some("actions"));
    for beside in [mouse(&actions, 2, 2), button(&actions, 2, 2, 2)] {
        assert_eq!(beside.state, account);
        assert_eq!(beside.effects.len(), 0);
    }
    // In the dialog, on nothing: it stays.
    let (x, y) = locate(&actions, "Account actions", Where::Anywhere);
    assert_eq!(mouse(&actions, x, y).state, actions);
    let mode = click(&actions, "Change mark", Where::Anywhere).state;
    assert_eq!(modal_kind(&mode), Some("choice"));
    assert_eq!(mouse(&mode, 2, 2).state, account);
    // A line of text and the thresholds stay: what was typed or set would be lost.
    let rename = reduce(&action(&account, "e").state, Action::Text("x".to_owned())).state;
    assert_eq!(modal_kind(&rename), Some("input"));
    assert_eq!(mouse(&rename, 2, 2).state, rename);
    let thresholds = action(&account, "t").state;
    assert_eq!(modal_kind(&thresholds), Some("thresholds"));
    assert_eq!(mouse(&thresholds, 2, 2).state, thresholds);
    // Under a dialog the footer's hints are the dialog's keys, and a click sends them.
    let moved = click(&actions, "Choose", Where::Anywhere).state;
    assert_eq!(moved.modal.as_ref().and_then(|modal| modal.index()), Some(1));
    assert_eq!(click(&actions, "Esc Close", Where::Anywhere).state, account);
    let cleared = click(&rename, "Ctrl+U Clear", Where::Anywhere).state;
    assert!(matches!(
        cleared.modal.map(|m| m.kind),
        Some(ModalKind::Input { value, .. }) if value.is_empty()
    ));
}

#[test]
fn the_wheel_moves_the_highlight_of_a_menu_and_the_selection_never_past_their_ends() {
    let (_home, state) = setup();
    let account = click(&state, "acct2", Where::Side).state;
    let actions = key(&account, "enter").state;
    let Some(ModalKind::Actions { options, .. }) = actions.modal.as_ref().map(|m| &m.kind) else {
        panic!("the actions dialog");
    };
    let last = options.len() - 1;
    let at = |state: &State| state.modal.as_ref().and_then(|modal| modal.index());
    assert_eq!(at(&wheel(&actions, 60, 12, -1).state), Some(0));
    let down = wheel(&actions, 60, 12, 1).state;
    assert_eq!(at(&down), Some(1));
    let end = wheel(&down, 60, 12, 100).state;
    assert_eq!(at(&end), Some(last));
    assert_eq!(at(&wheel(&end, 60, 12, 1).state), Some(last));
    // The highlighted command is the one Enter runs.
    assert_eq!(
        key(&wheel(&actions, 60, 12, 3).state, "enter").effects,
        vec![Effect::Refresh {
            account: Some("acct2".to_owned()),
            quiet: false
        }]
    );
    // The list's selection stops at its first and last rows.
    assert_eq!(wheel(&state, 3, 12, -3).state.selected, "overview");
    assert_eq!(wheel(&state, 3, 12, 100).state.selected, "stats");
}

#[test]
fn the_splash_and_minimum_size_error_use_the_same_ascii_identity() {
    let (home, state) = setup();
    let paths = &home.paths;
    let mut splash = state.clone();
    splash.splash = true;
    let rows = frame(&splash);
    let text = rows.join("\n");
    let offsets: Vec<i64> = LOGO
        .iter()
        .map(|line| {
            let row = rows.iter().find(|row| row.contains(line.trim())).unwrap();
            let lead = line.len() - line.trim_start().len();
            row.find(line.trim()).unwrap() as i64 - lead as i64
        })
        .collect();
    assert!(
        offsets.iter().all(|offset| *offset == offsets[0]),
        "ASCII rows retain their original alignment"
    );
    for line in LOGO {
        assert!(text.contains(line.trim()));
    }
    let main = frame(&state);
    assert!(matches(
        &main.join("\n"),
        r"(?s)Your Claude accounts\..*One continuous conversation\."
    ));
    assert!(text.contains("Press any key to start"));
    let caption = [SLOGAN, MOTTO[0], MOTTO[1]];
    let at = rows.iter().position(|line| line.contains(SLOGAN)).unwrap();
    assert_eq!(
        rows[at..at + 3]
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>(),
        caption
    );
    let starts: Vec<usize> = caption
        .iter()
        .enumerate()
        .map(|(i, line)| rows[at + i].find(line).unwrap())
        .collect();
    assert!(starts.iter().all(|start| *start == starts[0]));
    let rail: Vec<String> = main
        .iter()
        .map(|line| line.chars().take(45).collect::<String>().trim().to_owned())
        .collect();
    let main_at = rail.iter().position(|line| line == SLOGAN).unwrap();
    assert_eq!(rail[main_at..main_at + 3], caption);
    assert!(main.join("\n").contains("Keep the conversation moving."));
    let dismissed = press(&splash, paths, "a");
    assert!(!dismissed.state.splash);
    assert!(dismissed.state.modal.is_none());
    let mut error = splash.clone();
    error.size = Size { cols: 70, rows: 20 };
    assert!(frame(&error).join("\n").contains("Baton needs 80 x 24"));
    assert!(press(&error, paths, "enter").state.splash);
    assert_eq!(press(&error, paths, "q").effects, vec![Effect::Quit]);
    // A click leaves the splash screen too.
    assert!(!mouse(&splash, 5, 5).state.splash);
}

#[test]
fn every_supported_view_fits_the_fixed_design() {
    let (home, _state) = setup();
    for size in [
        Size { cols: 100, rows: 28 },
        Size { cols: 120, rows: 30 },
        Size { cols: 160, rows: 45 },
    ] {
        for (name, keys) in screens() {
            let lines = frame(&menu_state_with(&home.paths, size, &keys, GlyphSet::Rich));
            assert_eq!(lines.len(), size.rows - 1, "{name}");
            assert!(lines.iter().all(|line| width(line) <= size.cols), "{name}");
        }
    }
}
