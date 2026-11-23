//! The menu on the fixture home: every screen at every size, the keys and what they ask the runtime to
//! do.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::sync::Arc;

use baton_core::art::LOGO;
use baton_core::lines::{fit_line, p, plain, wrap_line};
use baton_core::paths::Paths;
use baton_core::text::width;
use baton_core::theme::{GlyphSet, RICH, make_theme, pick_glyphs_on};
use baton_testkit::{TestHome, temp_home};
use baton_tui::data::load_menu_data;
use baton_tui::keys::Key;
use baton_tui::layout::Size;
use baton_tui::legend::legend_line;
use baton_tui::reducer::reduce;
use baton_tui::state::{Action, Effect, Focus, ModalKind, State, Submit, last_toast};
use baton_tui::tree::{RowKind, RowLook, row_lines, tree_rows};
use baton_tui::ui::chrome::footer_hints;
use regex::Regex;
use serde_json::{Value, json};

use common::{
    Input, S1, build_home, frame, menu_state, menu_state_with, now, press, screen, screen_state, screens,
    sizes, text,
};

fn home(all_blocked: bool) -> TestHome {
    let home = temp_home();
    build_home(&home.paths, all_blocked);
    home
}

fn keys_of(state: &State) -> Vec<&'static str> {
    footer_hints(state).iter().map(|hint| hint.key).collect()
}

fn matches(text: &str, pattern: &str) -> bool {
    Regex::new(pattern).unwrap().is_match(text)
}

/// The state and every effect after some keys.
struct Went {
    state: State,
    effects: Vec<Effect>,
}

impl Went {
    /// The effects that act: operations and sign-ins, as JSON.
    fn applied(&self) -> Vec<Value> {
        self.effects
            .iter()
            .filter(|e| matches!(e, Effect::Apply { .. } | Effect::SignIn { .. }))
            .map(Effect::to_json)
            .collect()
    }

    fn effects(&self) -> Vec<Value> {
        self.effects.iter().map(Effect::to_json).collect()
    }
}

fn go_all(paths: &Paths, state: &State, inputs: Vec<Input>) -> Went {
    let mut state = state.clone();
    let mut effects = Vec::new();
    for input in inputs {
        let out = press(&state, paths, input);
        state = out.state;
        effects.extend(out.effects);
    }
    Went { state, effects }
}

macro_rules! go {
    ($paths:expr, $state:expr $(, $input:expr)* $(,)?) => {
        go_all($paths, &$state, vec![$(Input::from($input)),*])
    };
}

fn modal_kind(state: &State) -> Option<&'static str> {
    state.modal.as_ref().map(|modal| modal.kind_name())
}

#[test]
fn every_screen_fills_its_supported_terminal_and_smaller_terminals_show_the_branded_size_error() {
    let home = home(false);
    for (_, size) in sizes() {
        for (name, keys) in screens() {
            let state = menu_state_with(&home.paths, size, &keys, GlyphSet::Rich);
            let lines = frame(&state);
            assert_eq!(lines.len(), size.rows - 1, "{name}");
            assert!(lines.iter().all(|line| width(line) <= size.cols), "{name}");
            let text = lines.join("\n");
            if size.cols < 100 || size.rows < 28 {
                // Less room: the name in place of the wordmark, and the same screen.
                assert!(!text.contains("Baton needs"), "{name}");
                assert!(text.contains("BATON") || state.modal.is_some(), "{name}");
            } else {
                assert!(
                    text.contains(LOGO[1].trim()) || state.modal.is_some(),
                    "the shared ASCII identity: {name}"
                );
            }
        }
    }
}

#[test]
fn the_overview_is_concise_while_detail_views_retain_the_full_information() {
    let home = home(false);
    let at = |name: &str| frame(&screen_state(&home.paths, "120x30", name)).join("\n");
    let overview = at("overview");
    assert!(matches(&overview, r"(?s)NEXT ACCOUNT.*acct2"));
    assert!(matches(&overview, r"Account +5-hour +Weekly +Status"));
    assert!(matches(&overview, r"1 handoff +· +2 refusals"));
    assert!(matches(&overview, r"(?s)sports-odds.*workflow running"));
    assert!(at("account").contains("signed in (claude.ai)"));
    // Of the week every model draws on (63%); Fable's own limit (71%) has a bar of its own.
    assert!(at("account").contains("loses 32% in 4d if unused"));
    assert!(at("account-blocked").contains("Blocked by the weekly limit until"));
    let session = at("session");
    assert!(
        matches(&session, r"Per turn +3% of 5h +· +0\.5% of weekly"),
        "{session}"
    );
    assert!(session.contains("no planned hop while these run"));
    assert!(matches(&at("policy"), r"Plan factor .*1\.5x"));
    assert!(at("thresholds").contains("Thresholds of acct2"));
    assert!(at("remove").contains("Remove acct3 from Baton"));
    assert!(at("rename").contains("Rename acct2"));
}

#[test]
fn contextual_label_bars_explain_symbols_without_a_help_page() {
    let home = home(false);
    let state = menu_state(&home.paths, &[]);
    let g = &RICH;
    let rows = tree_rows(&state);
    for id in ["overview", "policy", "stats"] {
        let row = rows.iter().find(|r| r.id == id).unwrap();
        assert_eq!(row.icon.as_ref().unwrap().text.trim(), "", "{id} has no sign");
    }
    let signs: Vec<&str> = rows
        .iter()
        .filter(|r| r.kind == RowKind::Session)
        .map(|r| r.icon.as_ref().unwrap().text.as_str())
        .collect();
    assert_eq!(
        signs,
        [g.spinner[0], g.diamond],
        "working; moving to another account"
    );
    let bars: [(&str, Vec<(&str, &str)>); 3] = [
        (
            "overview",
            vec![
                (g.preferred, "Preferred"),
                (g.last_resort, "Last resort"),
                (g.normal, "Normal"),
            ],
        ),
        (
            "account",
            vec![
                (g.limit, "Threshold"),
                (g.baton, "Next account"),
                (g.blocked, "Blocked"),
            ],
        ),
        (
            "session",
            vec![
                (g.spinner[0], "Working"),
                (g.dot, "Idle"),
                (g.diamond, "Wait / move"),
                (g.ring, "Recent"),
            ],
        ),
    ];
    for (kind, signs) in bars {
        let bar = plain(&legend_line(&state, kind, 100));
        for (sign, label) in signs {
            assert!(bar.contains(&format!("{sign} {label}")), "{kind}: {bar}");
        }
        let lines = frame(&screen_state(&home.paths, "120x30", kind));
        assert!(
            matches(&lines[lines.len() - 2], "Preferred|Threshold|Working"),
            "{kind}"
        );
    }
    assert!(go!(&home.paths, state, "?").state.modal.is_none());
}

#[test]
fn an_accounts_tail_shows_both_windows_when_they_fit_else_the_higher_one_with_its_window() {
    let g = &RICH;
    let row = baton_tui::tree::Row {
        id: "acct:x".to_owned(),
        kind: RowKind::Account,
        depth: 0,
        icon: Some(p(g.normal)),
        name: "a-long-account-name".to_owned(),
        name_style: String::new(),
        tail: vec![p("5h "), p("42"), p(" \u{b7} "), p("wk "), p("63")],
        short_tail: Some(vec![p("wk "), p("63%")]),
        parent: None,
        selectable: true,
    };
    let look = RowLook {
        selected: false,
        focused: false,
        g,
        span: None,
    };
    let first = |cells: usize| plain(&row_lines(&row, cells, look)[0]);
    assert!(matches(&first(40), "a-long-account-name +5h 42 \u{b7} wk 63"));
    assert!(matches(&first(32), "a-long-account-name +wk 63%"));
    assert!(
        matches(&first(26), "a-long-account-name *$"),
        "the tail goes, the name stays whole"
    );
}

#[test]
fn the_workspace_has_a_rail_beside_the_pane_from_80_by_24_up_and_a_branded_minimum_size_error() {
    let home = home(false);
    let state = menu_state(&home.paths, &[]);
    let text = frame(&state).join("\n");
    assert!(matches(
        &text,
        r"(?s)Accounts.*NEXT ACCOUNT|NEXT ACCOUNT.*Accounts"
    ));
    // The window a terminal opens with on macOS and Linux: a narrower rail, the pane whole.
    let default = menu_state_with(&home.paths, Size { cols: 80, rows: 24 }, &[], GlyphSet::Rich);
    let drawn = frame(&default).join("\n");
    assert!(matches(
        &drawn,
        r"(?s)Accounts.*NEXT ACCOUNT|NEXT ACCOUNT.*Accounts"
    ));
    assert!(
        drawn.contains("Account         5-hour    Weekly    Status"),
        "{drawn}"
    );
    let small = menu_state_with(&home.paths, Size { cols: 79, rows: 24 }, &[], GlyphSet::Rich);
    let drawn = frame(&small).join("\n");
    assert!(drawn.contains("Baton needs 80 x 24 terminal cells."), "{drawn}");
    assert!(drawn.contains("Current size: 79 x 24"));
    assert_eq!(go!(&home.paths, small, "q").effects, vec![Effect::Quit]);
}

#[test]
fn accounts_show_immediately_without_an_enter_step_and_only_policy_opens_an_editor() {
    let home = home(false);
    let paths = &home.paths;
    let state = menu_state(paths, &[]);
    let account = go!(paths, state, "down", "down").state;
    assert_eq!(account.selected, "acct:acct2");
    for key in ["enter", "right", "tab", "shift+tab"] {
        assert_eq!(go!(paths, account, key).state.focus, Focus::Side, "{key}");
    }
    assert!(keys_of(&account).contains(&"enter") && !keys_of(&account).contains(&"Tab"));
    let policy = go!(paths, state, "end", "up").state;
    assert!(go!(paths, policy, "enter").state.form.is_some());
    assert_eq!(go!(paths, policy, "enter", "escape").state.focus, Focus::Side);
    let mut stats = go!(paths, state, "end").state;
    let mut data = (**stats.data.as_ref().unwrap()).clone();
    let again = data.stats.accounts.clone();
    data.stats.accounts.extend(again);
    stats.data = Some(Arc::new(data));
    let scrolled = go!(paths, stats, "pagedown").state;
    assert!(scrolled.views["stats"] > 0);
    assert_eq!(scrolled.selected, "stats");
}

#[test]
fn the_selected_sidebar_row_reads_clearly_focused_or_not_at_every_size() {
    let home = home(false);
    let g = &RICH;
    let ways: [&[&str]; 8] = [
        &[],
        &["down"],
        &["down", "down"],
        &["down", "down", "down", "down"],
        &["down", "down", "down", "down", "down"],
        &["down", "down", "down", "down", "down", "down"],
        &["end", "up"],
        &["end"],
    ];
    for (size_name, size) in sizes() {
        for keys in ways {
            let state = menu_state_with(&home.paths, size, keys, GlyphSet::Rich);
            let rows = tree_rows(&state);
            let row = rows.iter().find(|r| r.id == state.selected).unwrap();
            for focused in [true, false] {
                for cells in [18, 24, 40] {
                    for line in row_lines(
                        row,
                        cells,
                        RowLook {
                            selected: true,
                            focused,
                            g,
                            span: None,
                        },
                    ) {
                        for span in line {
                            let words: Vec<&str> = span.style.split(' ').collect();
                            assert!(
                                !words.contains(&"dim") && !words.contains(&"muted"),
                                "{size_name} {} focused={focused}: \"{}\" is \"{}\"",
                                row.id,
                                span.text,
                                span.style
                            );
                            if focused {
                                assert!(
                                    words.contains(&"selected") && words.contains(&"bold"),
                                    "{size_name} {}: \"{}\" is \"{}\"",
                                    row.id,
                                    span.text,
                                    span.style
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    // A bright bold bar; NO_COLOR: inverse and bold.
    let colour = make_theme(true, GlyphSet::Rich).props("selected bold");
    assert_eq!(
        (
            colour.color.as_deref(),
            colour.background.as_deref(),
            colour.bold,
            colour.dim
        ),
        (Some("#f0d5b3"), Some("#35302a"), true, false)
    );
    for style in ["selected", "selected bold", "selected dim muted"] {
        let mono = make_theme(false, GlyphSet::Rich).props(style);
        assert_eq!(
            (mono.inverse, mono.bold, mono.dim, mono.color),
            (true, true, false, None),
            "{style}"
        );
    }
}

fn sidebar_of(lines: &[String]) -> String {
    lines[1..lines.len() - 1]
        .iter()
        .map(|l| l.chars().take(27).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_filter_narrows_the_sidebar_as_you_type_and_esc_clears_it() {
    let home = home(false);
    let paths = &home.paths;
    let state = go!(paths, menu_state(paths, &[]), "/", "a", "c", "c", "t", "3").state;
    assert!(state.filtering);
    assert_eq!(state.selected, "acct:acct3");
    let lines = frame(&state);
    assert!(sidebar_of(&lines).contains("acct3"));
    assert!(!sidebar_of(&lines).contains("acct2"));
    assert!(lines.join("\n").contains("/ acct3"));
    let cleared = go!(paths, state, "escape").state;
    assert_eq!((cleared.filter.as_str(), cleared.filtering), ("", false));
    assert!(sidebar_of(&frame(&cleared)).contains("acct2"));
}

#[test]
fn quit_and_undisclosed_shortcuts() {
    let home = home(false);
    let paths = &home.paths;
    assert_eq!(
        go!(paths, menu_state(paths, &[]), "q").effects,
        vec![Effect::Quit]
    );
    assert_eq!(
        go!(paths, menu_state(paths, &[]), "ctrl+c").effects,
        vec![Effect::Quit]
    );
    assert_eq!(go!(paths, menu_state(paths, &[]), "ctrl+l").effects, vec![]);
}

#[test]
fn arrows_reach_the_pinned_search_and_add_account_controls_without_changing_the_open_view() {
    let home = home(false);
    let paths = &home.paths;
    let initial = menu_state(paths, &[]);
    let search = go!(paths, initial, "up").state;
    assert_eq!(search.focus, Focus::Search);
    assert_eq!(search.selected, "overview");
    assert!(frame(&search).join("\n").contains("› / Search"));
    let filtered = go!(paths, search, "enter", "a", "c", "c", "t", "3").state;
    assert!(filtered.filtering);
    assert_eq!(filtered.selected, "acct:acct3");
    let chosen = go!(paths, filtered, "down").state;
    assert_eq!(chosen.focus, Focus::Side);
    assert!(!chosen.filtering);
    assert_eq!(chosen.selected, "acct:acct3");
    let last = go!(paths, initial, "end").state;
    let add = go!(paths, last, "down").state;
    assert_eq!(add.focus, Focus::Add);
    assert_eq!(add.selected, "stats");
    assert!(frame(&add).join("\n").contains("› + Add account"));
    let dialog = go!(paths, add, "enter").state;
    assert!(matches!(
        dialog.modal.as_ref().map(|m| &m.kind),
        Some(ModalKind::Input {
            submit: Submit::Add,
            ..
        })
    ));
    assert_eq!(go!(paths, dialog, "escape", "up").state.focus, Focus::Side);
}

#[test]
fn left_and_right_fold_the_recent_sessions_and_go_between_the_fold_and_its_sessions() {
    let home = home(false);
    let paths = &home.paths;
    let initial = menu_state(paths, &[]);
    let fold = go!(
        paths, initial, "down", "down", "down", "down", "down", "down", "down"
    )
    .state;
    assert_eq!(
        (fold.selected.as_str(), fold.open_sessions),
        ("more:sessions", false)
    );
    assert_eq!(go!(paths, fold, "left").state, fold);
    let open = go!(paths, fold, "right").state;
    assert_eq!(
        (open.selected.as_str(), open.open_sessions),
        ("more:sessions", true)
    );
    // Right again goes to the first of the recent sessions, Left comes back to their fold.
    let child = go!(paths, open, "right").state;
    let rows = tree_rows(&child);
    let recent: Vec<&str> = rows
        .iter()
        .filter(|row| row.parent.is_some())
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(recent.len(), 2);
    assert_eq!(child.selected, recent[0]);
    assert_eq!(go!(paths, child, "right").state, child);
    let back = go!(paths, child, "left").state;
    assert_eq!(
        (back.selected.as_str(), back.open_sessions),
        ("more:sessions", true)
    );
    assert_eq!(go!(paths, back, "left").state, fold);
    // A row that is no fold and has none: nothing. Nor under a filter, where the fold is not a row.
    let account = go!(paths, initial, "down").state;
    for key in ["left", "right"] {
        assert_eq!(go!(paths, account, key).state, account, "{key}");
    }
    let found = go!(paths, initial, "/", text("royal"), "enter").state;
    assert_eq!((found.selected.as_str(), found.focus), (recent[0], Focus::Side));
    assert_eq!(go!(paths, found, "left").state, found);
}

#[test]
fn the_footer_offers_actions_and_undisclosed_direct_shortcuts_do_nothing() {
    let home = home(false);
    let paths = &home.paths;
    let account = menu_state(paths, &["down", "down"]);
    let hints = keys_of(&account);
    assert!(!hints.contains(&"Space") && !hints.contains(&"?") && hints.contains(&"q"));
    assert!(!hints.contains(&"d") && !hints.contains(&"s") && hints.contains(&"enter"));
    assert!(go!(paths, account, " ").state.modal.is_none());
    for key in [
        "e",
        "r",
        "s",
        "d",
        "p",
        "l",
        "n",
        "a",
        "J",
        "K",
        "h",
        "H",
        "?",
        "tab",
        "ctrl+l",
        "j",
        "k",
        "g",
        "G",
        "shift+down",
    ] {
        let ignored = go!(paths, account, key);
        assert_eq!(ignored.state, account, "{key}");
        assert_eq!(ignored.effects, vec![], "{key}");
    }
    assert_eq!(
        modal_kind(&go!(paths, account, "enter", "e").state),
        Some("input")
    );
    assert_eq!(
        modal_kind(&go!(paths, account, "enter", "d").state),
        Some("confirm")
    );
    let form = menu_state(paths, &["end", "up", "enter"]);
    assert_eq!(keys_of(&form)[0], "updown");
}

#[test]
fn backspacing_an_empty_search_restores_the_highlighted_placeholder_and_typing_resumes_it() {
    let home = home(false);
    let paths = &home.paths;
    let initial = menu_state(paths, &[]);
    let cleared = go!(paths, initial, "/", "a", "backspace").state;
    assert_eq!(
        (cleared.filter.as_str(), cleared.filtering, cleared.focus),
        ("", false, Focus::Search)
    );
    assert!(frame(&cleared).join("\n").contains("› / Search"));
    let typed = go!(paths, cleared, "a", "c", "c", "t", "3").state;
    assert!(typed.filtering);
    assert_eq!(typed.selected, "acct:acct3");
    let emptied = go!(paths, typed, "ctrl+u").state;
    assert!(!emptied.filtering);
    assert!(frame(&emptied).join("\n").contains("› / Search"));
}

#[test]
fn an_unmatched_search_clears_the_detail_pane_and_cannot_act_on_the_previous_account() {
    let home = home(false);
    let paths = &home.paths;
    let account = menu_state(paths, &["down", "down"]);
    let empty = go!(paths, account, "/", text("zzzz-no-matches")).state;
    let lines = frame(&empty);
    assert!(lines.join("\n").contains("Nothing matches"));
    assert!(
        lines[..lines.len() - 1].iter().all(|line| line
            .chars()
            .skip(46)
            .collect::<String>()
            .trim()
            .is_empty())
    );
    let inactive = go!(paths, empty, "enter", "d", "s");
    assert!(inactive.state.modal.is_none());
    assert_eq!(inactive.effects.len(), 0);
    assert!(!keys_of(&inactive.state).contains(&"enter"));
    let restored = go!(paths, empty, "ctrl+u").state;
    assert!(frame(&restored).join("\n").contains("#2 of 4"));
}

#[test]
fn account_actions_open_contextually_and_mode_changes_use_the_same_account_id() {
    let home = home(false);
    let paths = &home.paths;
    let initial = menu_state(paths, &[]);
    let above_footer = |state: &State| {
        let lines = frame(state);
        lines[..lines.len() - 1].join("\n")
    };
    assert!(!matches(&above_footer(&initial), "Actions|Help|Refresh"));
    let account = go!(paths, initial, "down", "down").state;
    assert!(!matches(&above_footer(&account), "Actions|Refresh"));
    let menu = go!(paths, account, "enter").state;
    assert_eq!(modal_kind(&menu), Some("actions"));
    let drawn = frame(&menu).join("\n");
    assert!(matches(&drawn, r"Change mark\s+m"));
    assert_eq!(go!(paths, menu, "m").state.modal.unwrap().title, "Account mark");
    for label in ["Rename", "Change mark", "Delete account", "Sign in again"] {
        assert!(drawn.contains(label), "{label}");
    }
    assert!(matches!(
        go!(paths, menu, "enter").state.modal.map(|m| m.kind),
        Some(ModalKind::Input {
            submit: Submit::Rename(_),
            ..
        })
    ));
    let mode = go!(paths, menu, "down", "enter").state;
    assert_eq!(mode.modal.as_ref().unwrap().title, "Account mark");
    assert_eq!(
        go!(paths, mode, "down", "enter").applied()[0]["args"],
        json!({ "id": "acct2", "mark": "normal" })
    );
    assert_eq!(modal_kind(&go!(paths, menu, "d").state), Some("confirm"));
    assert!(go!(paths, menu, "escape").state.modal.is_none());
    assert_eq!(
        go!(paths, account, "enter", "r").effects(),
        vec![json!({ "type": "refresh", "account": "acct2" })]
    );
}

#[test]
fn account_actions_marks_order_thresholds_sign_in_add_remove() {
    let home = home(false);
    let paths = &home.paths;
    let acct2 = menu_state(paths, &["down", "down"]);
    let mode = go!(paths, acct2, "enter", "down", "enter").state;
    assert_eq!(
        go!(paths, mode, "3").applied()[0]["args"],
        json!({ "id": "acct2", "mark": "last-resort" })
    );
    assert_eq!(
        go!(paths, mode, "2").applied()[0]["args"],
        json!({ "id": "acct2", "mark": "normal" })
    );
    assert_eq!(
        go!(paths, acct2, "enter", "J").applied()[0]["args"],
        json!({ "id": "acct2", "position": 3 })
    );
    assert_eq!(
        go!(paths, acct2, "enter", "K").applied()[0]["args"],
        json!({ "id": "acct2", "position": 1 })
    );
    assert_eq!(
        go!(paths, acct2, "enter", "s").applied(),
        vec![json!({ "type": "signin", "account": "acct2" })]
    );
    // Thresholds: a small form.
    let form = go!(paths, acct2, "enter", "t").state;
    assert_eq!(modal_kind(&form), Some("thresholds"));
    let edited = go!(
        paths, form, "right", "right", "down", "left", "left", "left", "left", "left", "enter"
    );
    assert_eq!(
        edited.applied()[0],
        json!({
            "type": "apply",
            "op": "thresholds",
            "args": { "id": "acct2", "fiveHour": 92, "weekly": 90 },
            "done": "Saved the thresholds of acct2."
        })
    );
    assert_eq!(
        go!(paths, form, "right", "d", "enter").applied()[0]["args"],
        json!({ "id": "acct2", "fiveHour": null, "weekly": null })
    );
    assert_eq!(go!(paths, form, "escape").applied().len(), 0);
    // Remove asks first.
    let acct3 = menu_state(paths, &["down", "down", "down"]);
    let confirm = go!(paths, acct3, "enter", "d").state;
    assert_eq!(modal_kind(&confirm), Some("confirm"));
    assert_eq!(go!(paths, confirm, "n").applied().len(), 0);
    assert_eq!(
        go!(paths, confirm, "y").applied()[0]["args"],
        json!({ "id": "acct3" })
    );
    // Add: a name, then Claude Code's sign-in.
    let added = go!(paths, acct2, "end", "down", "enter", text("acct5"), "enter");
    assert_eq!(
        added.applied()[0],
        json!({ "type": "apply", "op": "add", "args": { "name": "acct5" }, "done": null })
    );
    // Any name; never one taken in another case.
    let email = go!(
        paths,
        acct2,
        "end",
        "down",
        "enter",
        text("jane@example.com"),
        "enter"
    );
    assert_eq!(email.applied()[0]["args"], json!({ "name": "jane@example.com" }));
    let bad = go!(paths, acct2, "end", "down", "enter", text("ACCT3"), "enter");
    assert_eq!(bad.applied().len(), 0);
    assert!(
        last_toast(&bad.state)
            .unwrap()
            .contains("acct3 is the name of another account")
    );
}

#[test]
fn session_actions_hop_at_the_end_of_the_turn_or_to_a_chosen_account() {
    let home = home(false);
    let paths = &home.paths;
    let s1 = menu_state(paths, &["down", "down", "down", "down", "down"]);
    assert_eq!(s1.selected, format!("sess:{S1}"));
    assert_eq!(
        go!(paths, s1, "enter", "h").applied(),
        vec![json!({ "type": "apply", "op": "hop", "args": { "session": S1, "to": null }, "done": null })]
    );
    let choice = go!(paths, s1, "enter", "H").state;
    let Some(ModalKind::Choice { options, .. }) = choice.modal.as_ref().map(|m| &m.kind) else {
        panic!("a choice of accounts");
    };
    assert_eq!(
        options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(),
        ["acct1", "acct3", "acct4"]
    );
    assert_eq!(
        go!(paths, choice, "down", "down", "enter").applied()[0]["args"],
        json!({ "session": S1, "to": "acct4" })
    );
    assert_eq!(
        go!(paths, choice, "2").applied()[0]["args"],
        json!({ "session": S1, "to": "acct3" })
    );
}

#[test]
fn the_policy_form_changes_settings_after_a_confirmation_and_esc_leaves_them() {
    let home = home(false);
    let paths = &home.paths;
    let form = menu_state(paths, &["end", "up", "enter"]);
    assert!(form.form.is_some());
    let changed = go!(
        paths, form, "down", "right", "down", "down", "down", "right", "end", "enter"
    );
    assert_eq!(modal_kind(&changed.state), Some("confirm"));
    assert_eq!(
        changed.state.modal.as_ref().unwrap().lines,
        [
            "5-hour limit: 90% \u{2192} 91%",
            "Plan factor: 1.5x \u{2192} 1.6x"
        ]
    );
    assert_eq!(
        go!(paths, changed.state, "y").applied()[0]["args"],
        json!({ "changes": { "fiveHour": 91, "planFactor": 1.6 } })
    );
    let value = |state: &State, key: &str| state.form.as_ref().unwrap().value(key).cloned().unwrap();
    let toggled = go!(paths, form, "end", "up", "up", "up", "up", "left");
    assert_eq!(value(&toggled.state, "compactionRebalance"), json!(false));
    // Under the usage poll: Baton's usage after your own bar.
    let bar = go!(paths, form, "end", "up", "up", "right");
    assert_eq!(value(&bar.state, "statusLineUsage"), json!(true));
    // The last field: full sync, on unless the operator turns it off.
    let sync = go!(paths, form, "end", "up", "left");
    assert_eq!(value(&form, "fullSync"), json!(true));
    assert_eq!(value(&sync.state, "fullSync"), json!(false));
    assert_eq!(value(&go!(paths, form, "right").state, "mode"), json!("order"));
    let left = go!(paths, form, "down", "right", "escape");
    assert!(left.state.form.is_none());
    assert_eq!(left.applied().len(), 0);
    assert!(last_toast(&left.state).unwrap().contains("left unchanged"));
}

#[test]
fn keys_text_and_glyphs() {
    assert_eq!(Key::from_name("shift+up").name(), "shift+up");
    assert_eq!(Key::from_name("shift+tab").name(), "shift+tab");
    assert_eq!(Key::from_name("J"), Key::Char('J'));
    // The same keys as a terminal sends them, and text that arrives at once.
    let sent = |bytes: &str| {
        let mut decoder = baton_tui::input::Decoder::new();
        let mut actions = decoder.push(bytes.as_bytes());
        actions.extend(decoder.flush());
        actions
    };
    assert_eq!(sent("\x1b[1;2A"), vec![Action::Key(Key::from_name("shift+up"))]);
    assert_eq!(sent("\x1b[Z"), vec![Action::Key(Key::from_name("shift+tab"))]);
    assert_eq!(sent("J"), vec![Action::Key(Key::Char('J'))]);
    assert_eq!(sent("acct5"), vec![Action::Text("acct5".to_owned())]);
    let env = |pairs: &[(&str, &str)]| baton_core::env::Env::from_pairs(pairs.iter().map(|(k, v)| (*k, *v)));
    assert_eq!(
        pick_glyphs_on(&env(&[("BATON_GLYPHS", "plain")]), false),
        GlyphSet::Plain
    );
    assert_eq!(pick_glyphs_on(&env(&[]), true), GlyphSet::Plain);
    assert_eq!(pick_glyphs_on(&env(&[("WT_SESSION", "1")]), true), GlyphSet::Rich);
    // A line too wide is cut at the edge as a last resort, without an ellipsis; wrapping keeps
    // everything.
    assert_eq!(plain(&fit_line(&[p("abcdefgh")], 5, "")), "abcde");
    assert_eq!(
        wrap_line(&[p("one two three")], 7, 0)
            .iter()
            .map(|l| plain(l))
            .collect::<Vec<_>>(),
        ["one two", "three"]
    );
}

#[test]
fn the_plain_glyphs_draw_every_screen_too() {
    let home = home(false);
    let paths = &home.paths;
    for name in ["overview", "account", "stats"] {
        let lines = frame(&menu_state_with(
            paths,
            Size { cols: 120, rows: 30 },
            &screen(name),
            GlyphSet::Plain,
        ));
        assert_eq!(lines.len(), 29);
        assert!(!lines.join("\n").contains('\u{2026}'));
    }
    assert!(load_menu_data(paths, now()).sessions.len() >= 2);
    // A reducer state without data says it is reading.
    let mut without = menu_state(paths, &[]);
    without.data = None;
    let empty = reduce(&without, Action::Key(Key::Down)).state;
    assert!(frame(&empty).join("\n").contains("Loading your accounts"));
}

#[test]
fn the_data_is_everything_the_menu_shows_read_in_one_go() {
    let home = home(false);
    let paths = &home.paths;
    let data = load_menu_data(paths, now());
    assert_eq!(data.now, now());
    assert_eq!(
        (data.next.account.as_deref(), data.next.reason.as_str()),
        (Some("acct2"), "preferred")
    );
    assert_eq!(data.states.len(), 4);
    assert_eq!(data.readings.len(), 4);
    assert_eq!(data.poll.as_ref().unwrap().holder.as_ref().unwrap().kind, "menu");
    // The live sessions first, then the recent ones, the newest first.
    let sessions: Vec<(bool, bool)> = data.sessions.iter().map(|x| (x.live, x.ended)).collect();
    assert_eq!(
        sessions,
        [(true, false), (true, false), (false, true), (false, true)]
    );
    for (session, folder) in data
        .sessions
        .iter()
        .zip(["sports-odds", "pelican", "royal-flush", "baton"])
    {
        assert!(session.name.ends_with(folder), "{}", session.name);
    }
    let first = &data.sessions[0];
    assert_eq!(first.id, S1);
    assert_eq!(
        (first.account.as_deref(), first.account_name.as_deref()),
        (Some("acct2"), Some("acct2"))
    );
    assert_eq!((first.turn_state.as_deref(), first.turns), (Some("running"), 5));
    assert_eq!(
        (first.cost.five_hour, first.cost.seven_day),
        (Some(3.0), Some(0.5))
    );
    assert_eq!(first.background_tasks.len(), 4);
    assert_eq!(first.hops.len(), 1);
    let second = &data.sessions[1];
    assert_eq!(
        (
            second.wrapper_state.as_deref(),
            second.target_name.as_deref(),
            second.error.as_deref()
        ),
        (Some("countdown"), Some("acct2"), Some("rate_limit"))
    );
    assert_eq!(data.stats.days.len(), 7);
    // Every account blocked: nothing to start on, and the one that is back first.
    let blocked = home_blocked();
    let data = load_menu_data(&blocked.paths, now());
    assert_eq!(
        (
            data.next.account.as_deref(),
            data.next.earliest.as_deref(),
            data.next.available_known
        ),
        (None, Some("acct1"), true)
    );
    let drawn = frame(&menu_state(&blocked.paths, &[])).join("\n");
    assert!(drawn.contains("All accounts are at their limit"));
    assert!(drawn.contains("acct1 returns in 40m"));
}

fn home_blocked() -> TestHome {
    home(true)
}

#[test]
fn long_wide_and_email_names_are_never_cut_and_keep_every_row_exact() {
    let home = home(false);
    let paths = &home.paths;
    let long = "A very long account name that goes on and on, far past the width of the sidebar";
    baton_testkit::write_json(
        &paths.config,
        &json!({
            "version": 1,
            "accounts": [
                { "id": "acct1", "name": long, "mark": "normal" },
                { "id": "acct2", "name": "日本語アカウント", "mark": "preferred" },
                { "id": "acct3", "name": "jane.doe+claude@example.com", "mark": "normal", "fiveHour": 80 },
                { "id": "acct4", "name": "acct4", "mark": "last-resort", "weekly": 90 }
            ],
            "policy": { "mode": "order", "usagePollMinutes": 0 }
        }),
    );
    let small = Size { cols: 100, rows: 28 };
    let ways: [&[&str]; 8] = [
        &[],
        &["down"],
        &["down", "down"],
        &["down", "down", "down"],
        &["end"],
        &["down", "enter", "e"],
        &["down", "down", "down", "down", "down", "enter", "H"],
        &["down", "enter", "d"],
    ];
    for size in [small, Size { cols: 120, rows: 30 }] {
        for keys in ways {
            let state = menu_state_with(paths, size, keys, GlyphSet::Rich);
            let rows = baton_tui::ui::app::frame(&state, 0);
            assert_eq!(rows.len(), size.rows - 1);
            assert!(
                rows.iter()
                    .all(|row| baton_core::lines::line_width(row) == size.cols),
                "{keys:?}"
            );
            assert!(!frame(&state).join("\n").contains('\u{2026}'), "{keys:?}");
        }
    }
    // In the sidebar the long name wraps under itself, whole.
    let lines = frame(&menu_state_with(paths, small, &["down"], GlyphSet::Rich));
    let rail: Vec<String> = lines
        .iter()
        .map(|line| line.chars().take(45).collect::<String>().trim().to_owned())
        .collect();
    let rail = rail.join(" ");
    assert!(rail.contains(&format!("\u{203a} \u{b7} {long}")), "{rail}");
    // In the pane it is too long for a title: the title says what it is and the name leads the body.
    let pane: Vec<String> = lines
        .iter()
        .map(|line| line.chars().skip(49).collect::<String>().trim_end().to_owned())
        .collect();
    assert_eq!(pane[1], "Account");
    assert_eq!(
        pane[4..6],
        [
            "A very long account name that goes on and on,",
            "far past the width of the sidebar"
        ]
    );
    // A wide name takes two cells a character: its row still ends at the rule.
    let lines = frame(&menu_state_with(paths, small, &[], GlyphSet::Rich));
    let row = lines
        .iter()
        .find(|line| line.contains("日本語アカウント"))
        .unwrap();
    let rule = row.find('\u{2502}').unwrap();
    assert_eq!(width(&row[..rule]), 45, "{row}");
    // The account's own threshold and the other policy show.
    let acct3 = frame(&menu_state_with(
        paths,
        small,
        &["down", "down", "down"],
        GlyphSet::Rich,
    ))
    .join("\n");
    assert!(acct3.contains("5-hour 80% \u{b7} weekly 95%"), "{acct3}");
    let overview = lines.join("\n");
    assert!(overview.contains("first open account in order"));
    assert!(overview.contains("Refresh on request"));
}

#[test]
fn a_home_without_accounts_invites_the_first_one() {
    let home = temp_home();
    let paths = &home.paths;
    baton_testkit::write_json(
        &paths.config,
        &json!({ "version": 1, "accounts": [], "policy": {} }),
    );
    let state = menu_state(paths, &[]);
    let lines = frame(&state);
    let text = lines.join("\n");
    assert!(text.contains("Your next conversation starts here."));
    assert!(text.contains("Usage updates while Baton is running"));
    assert!(!text.contains("Accounts"));
    // The invitation is a link to the Add account dialog.
    let y = lines
        .iter()
        .position(|line| line.contains("+ Add an account"))
        .unwrap();
    let x = width(&lines[y][..lines[y].find("+ Add an account").unwrap()]);
    let clicked = reduce(
        &state,
        Action::Mouse(baton_tui::state::MouseEvent::press(x as i32, y as i32)),
    );
    assert!(matches!(
        clicked.state.modal.as_ref().map(|m| &m.kind),
        Some(ModalKind::Input {
            submit: Submit::Add,
            ..
        })
    ));
    assert_eq!(clicked.state.focus, Focus::Add);
    // The Stats and the Policy draw without accounts too.
    for keys in [&["end"][..], &["end", "up", "enter"][..]] {
        let rows = frame(&menu_state(paths, keys));
        assert_eq!(rows.len(), 29);
    }
}

#[test]
fn a_long_name_such_as_an_email_wraps_in_the_sidebar_instead_of_being_cut_and_every_frame_keeps_its_width() {
    let home = home(false);
    let paths = &home.paths;
    let name = "alexandra.longlastname.work.account@company.example.org";
    let mut config: Value = serde_json::from_str(&std::fs::read_to_string(&paths.config).unwrap()).unwrap();
    config["accounts"][1]["name"] = json!(name);
    baton_testkit::write_json(&paths.config, &config);
    let marks =
        Regex::new("[\\s\u{2502}\u{256d}\u{256e}\u{2570}\u{256f}\u{2500}\u{203a}\u{2605}\u{25bf}\u{b7}]+")
            .unwrap();
    let both: &[&str] = &["down", "down"];
    for (size_name, keys) in [
        ("120x30", both),
        ("100x30", both),
        ("80x24", both),
        ("80x24", &[]),
    ] {
        let state = menu_state_with(paths, common::size(size_name), keys, GlyphSet::Rich);
        let lines = frame(&state);
        for line in &lines {
            assert!(
                width(line) <= state.size.cols,
                "{size_name}: {} > {}: {line}",
                width(line),
                state.size.cols
            );
        }
        let text = lines.join("\n");
        if state.size.cols < 100 {
            // A narrower rail: nothing is wider than the terminal, and the name is cut, not wrapped
            // into the pane.
            assert!(!text.contains("Baton needs"));
            continue;
        }
        // Every part of the name is there, in order, across the wrapped lines.
        let rail: String = lines
            .iter()
            .map(|line| line.chars().take(46).collect::<String>())
            .collect();
        let squeezed = marks.replace_all(&rail, "");
        assert!(squeezed.contains(name), "{size_name} shows the whole name");
    }
}
