//! The store: actions run through the core functions and the menu reads the result back.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use baton_claude::poller::PollResult;
use baton_core::config::load_config;
use baton_core::usage::{Reading, WindowReading};
use baton_testkit::{TestHome, temp_home, test_env};
use baton_tui::keys::Key;
use baton_tui::layout::Size;
use baton_tui::state::{Action, Auth, Effect, ModalKind, Op, State, Submit, last_toast};
use baton_tui::store::{Host, NoHost, Store, StoreOptions, apply_op};
use baton_tui::ui::chrome::footer_hints;
use serde_json::json;

use common::{S1, build_home, now};

const EMAIL: &str = "jane.work+baton@example.com";

/// A host that notes what the store asked of it.
#[derive(Default)]
struct Noted {
    quitted: bool,
    redrawn: usize,
    signed_in: Vec<String>,
    code: i32,
}

impl Host for Noted {
    fn quit(&mut self) {
        self.quitted = true;
    }

    fn redraw(&mut self) {
        self.redrawn += 1;
    }

    fn sign_in(&mut self, account: &str) -> i32 {
        self.signed_in.push(account.to_owned());
        self.code
    }
}

fn home() -> TestHome {
    let home = temp_home();
    build_home(&home.paths, false);
    home
}

fn options(home: &TestHome) -> StoreOptions {
    StoreOptions {
        size: Size { cols: 120, rows: 30 },
        now: Arc::new(now),
        color: false,
        ..StoreOptions::new(home.paths.clone(), test_env(&home.home))
    }
}

fn keys(store: &mut Store, host: &mut dyn Host, names: &[&str]) {
    for name in names {
        store.dispatch(Action::Key(Key::from_name(name)), host);
    }
}

fn input_of(state: &State) -> Option<(&Submit, &str)> {
    match state.modal.as_ref().map(|modal| &modal.kind) {
        Some(ModalKind::Input { value, submit, .. }) => Some((submit, value.as_str())),
        _ => None,
    }
}

#[test]
fn actions_run_through_the_core_functions_and_the_menu_reads_the_result_back() {
    let home = home();
    let paths = &home.paths;
    let mut host = Noted::default();
    let mut store = Store::new(options(&home));
    assert!(store.state().data.is_none());
    store.dispatch(Action::Tick, &mut host);
    assert!(store.state().data.is_some());
    keys(
        &mut store,
        &mut host,
        &["down", "down", "enter", "down", "enter", "3"],
    );
    let account = |name: &str| {
        load_config(paths)
            .accounts
            .into_iter()
            .find(|a| a.name == name)
            .unwrap()
    };
    assert_eq!(account("acct2").mark.as_str(), "last-resort");
    assert!(
        last_toast(store.state())
            .unwrap()
            .contains("Updated the mode of acct2")
    );
    let shown = store
        .state()
        .data
        .as_ref()
        .unwrap()
        .states
        .iter()
        .find(|a| a.name == "acct2")
        .unwrap()
        .mark;
    assert_eq!(shown.as_str(), "last-resort");
    keys(&mut store, &mut host, &["enter", "J"]);
    assert_eq!(
        load_config(paths)
            .accounts
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["acct1", "acct3", "acct2", "acct4"]
    );
    assert_eq!(
        store.state().selected,
        "acct:acct2",
        "the selection follows the account"
    );
    keys(&mut store, &mut host, &["enter", "t", "right", "enter"]);
    assert_eq!(account("acct2").five_hour, Some(91.0));
    // A hop request for a live session is refused when no wrapper holds it.
    keys(
        &mut store,
        &mut host,
        &["end", "up", "up", "up", "up", "enter", "h"],
    );
    assert_eq!(store.state().selected, format!("sess:{S1}"));
    assert!(
        last_toast(store.state())
            .unwrap()
            .contains("only work in sessions started with baton")
    );
    assert!(!host.quitted);
    keys(&mut store, &mut host, &["q"]);
    assert!(host.quitted);
    // The policy goes through the same operations.
    let env = test_env(&home.home);
    let changes = vec![("startReserve".to_owned(), json!(25))];
    assert_eq!(
        apply_op(paths, &env, now(), &Op::Policy { changes }),
        Ok(Default::default())
    );
    assert_eq!(load_config(paths).policy.get("startReserve"), Some(&json!(25)));
    let bad = apply_op(
        paths,
        &env,
        now(),
        &Op::Policy {
            changes: vec![("fiveHour".to_owned(), json!(500))],
        },
    );
    assert_eq!(bad.unwrap_err(), "fiveHour takes a number from 1 to 100.");
    // A closed store changes no more.
    store.close();
    let before = store.version();
    keys(&mut store, &mut host, &["down"]);
    assert_eq!(store.version(), before);
}

#[test]
fn usage_refresh_reports_the_polling_hosts_array_of_results() {
    let home = home();
    let mut store = Store::new(StoreOptions {
        refresh: Some(Arc::new(|account| {
            assert_eq!(account, None, "the Overview asks every account");
            Ok(vec![PollResult {
                id: "acct2".to_owned(),
                name: "acct2".to_owned(),
                status: "ok".to_owned(),
                reading: Some(Reading {
                    five_hour: Some(WindowReading {
                        used: 42.0,
                        resets_at: None,
                        read_at: None,
                        source: None,
                        label: None,
                    }),
                    ..Reading::default()
                }),
                ..PollResult::default()
            }])
        })),
        ..options(&home)
    });
    let mut host = NoHost;
    store.dispatch(Action::Tick, &mut host);
    keys(&mut store, &mut host, &["enter", "r"]);
    assert_eq!(last_toast(store.state()), Some("Asking every account's usage."));
    assert!(store.settle(&mut host, Duration::from_secs(5)));
    assert_eq!(last_toast(store.state()), Some("acct2: 5-hour 42%."));
    // A refresh that fails says so; one account's is asked by its name.
    let mut store = Store::new(StoreOptions {
        refresh: Some(Arc::new(|account| {
            Err(format!("no claude for {}", account.unwrap_or("anyone")))
        })),
        ..options(&home)
    });
    store.dispatch(Action::Tick, &mut host);
    keys(&mut store, &mut host, &["down", "down", "enter", "r"]);
    assert_eq!(last_toast(store.state()), Some("Asking acct2's usage."));
    assert!(store.settle(&mut host, Duration::from_secs(5)));
    assert_eq!(
        last_toast(store.state()),
        Some("Could not ask acct2's usage: no claude for acct2")
    );
    // Without anyone to ask, asking does nothing.
    let mut store = Store::new(options(&home));
    store.dispatch(Action::Tick, &mut host);
    keys(&mut store, &mut host, &["enter", "r"]);
    assert_eq!(last_toast(store.state()), None);
    assert!(!store.working());
}

#[test]
fn rename_in_the_menu_enter_then_e_opens_the_name_field_enter_renames_esc_cancels_the_row_stays_selected() {
    let home = home();
    let paths = &home.paths;
    let mut host = NoHost;
    let mut store = Store::new(options(&home));
    store.dispatch(Action::Tick, &mut host);
    keys(&mut store, &mut host, &["down", "down", "enter", "e"]);
    assert_eq!(
        input_of(store.state()),
        Some((&Submit::Rename("acct2".to_owned()), "acct2"))
    );
    keys(&mut store, &mut host, &["escape"]);
    assert!(store.state().modal.is_none());
    keys(&mut store, &mut host, &["enter", "e", "ctrl+u"]);
    store.dispatch(Action::Text("ACCT3".to_owned()), &mut host);
    keys(&mut store, &mut host, &["enter"]);
    assert!(
        last_toast(store.state())
            .unwrap()
            .contains("acct3 is the name of another account")
    );
    assert!(input_of(store.state()).is_some(), "the field stays open");
    keys(&mut store, &mut host, &["ctrl+u"]);
    // The field never holds a control character.
    store.dispatch(Action::Text("a\u{7}b".to_owned()), &mut host);
    assert_eq!(input_of(store.state()).map(|(_, value)| value), Some("ab"));
    keys(&mut store, &mut host, &["ctrl+u"]);
    store.dispatch(Action::Text(EMAIL.to_owned()), &mut host);
    keys(&mut store, &mut host, &["enter"]);
    let state = store.state();
    assert!(state.modal.is_none());
    assert!(
        last_toast(state)
            .unwrap()
            .contains("Renamed acct2 to jane.work+baton@")
    );
    assert_eq!(
        load_config(paths)
            .accounts
            .iter()
            .find(|a| a.id == "acct2")
            .unwrap()
            .name,
        EMAIL
    );
    assert_eq!(state.selected, "acct:acct2");
    assert_eq!(
        state
            .data
            .as_ref()
            .unwrap()
            .states
            .iter()
            .find(|a| a.id == "acct2")
            .unwrap()
            .name,
        EMAIL
    );
    // The footer offers the contextual account menu.
    assert!(footer_hints(state).iter().any(|hint| hint.key == "enter"));
}

#[test]
fn adding_an_account_is_followed_by_its_sign_in_and_the_sign_in_by_a_look_at_it() {
    let home = home();
    let paths = &home.paths;
    let asked: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let polled: Arc<std::sync::Mutex<Vec<Option<String>>>> = Arc::default();
    let (auth, usage) = (Arc::clone(&asked), Arc::clone(&polled));
    let mut store = Store::new(StoreOptions {
        auth_of: Some(Arc::new(move |id| {
            auth.lock().unwrap().push(id.to_owned());
            Auth {
                logged_in: Some(true),
                auth_method: Some("claude.ai".to_owned()),
                error: None,
            }
        })),
        refresh: Some(Arc::new(move |account| {
            usage.lock().unwrap().push(account.map(str::to_owned));
            Ok(Vec::new())
        })),
        ..options(&home)
    });
    let mut host = Noted::default();
    store.dispatch(Action::Tick, &mut host);
    keys(&mut store, &mut host, &["end", "down", "enter"]);
    store.dispatch(Action::Text("Work account".to_owned()), &mut host);
    keys(&mut store, &mut host, &["enter"]);
    // The new account's profile is there, and Claude Code's sign-in ran for it, by its id.
    let added = load_config(paths)
        .accounts
        .into_iter()
        .find(|a| a.name == "Work account")
        .unwrap();
    assert_eq!(added.id, "work-account");
    assert_eq!(host.signed_in, ["work-account"]);
    assert!(store.settle(&mut host, Duration::from_secs(5)));
    assert_eq!(
        last_toast(store.state()),
        Some("Work account: Claude Code's sign-in finished.")
    );
    assert!(store.state().busy.is_none());
    // Its sign-in was asked again, and its usage quietly.
    assert_eq!(*asked.lock().unwrap(), ["work-account"]);
    assert_eq!(*polled.lock().unwrap(), [Some("work-account".to_owned())]);
    assert_eq!(store.state().auth["work-account"].logged_in, Some(true));
    // A sign-in that does not finish says so, and asks no usage.
    host.code = 3;
    store.perform(
        Effect::SignIn {
            account: "acct4".to_owned(),
        },
        &mut host,
    );
    assert!(store.settle(&mut host, Duration::from_secs(5)));
    assert_eq!(
        last_toast(store.state()),
        Some("acct4: the sign-in did not finish (code 3).")
    );
    assert_eq!(polled.lock().unwrap().len(), 1);
    // Every account's sign-in is asked when the menu starts.
    asked.lock().unwrap().clear();
    store.perform(Effect::Auth { accounts: None }, &mut host);
    assert!(store.settle(&mut host, Duration::from_secs(5)));
    let mut all = asked.lock().unwrap().clone();
    all.sort();
    assert_eq!(all, ["acct1", "acct2", "acct3", "acct4", "work-account"]);
    // A resize asks for the screen to be drawn again.
    store.dispatch(Action::Resize { cols: 100, rows: 28 }, &mut host);
    assert_eq!(host.redrawn, 1);
}
