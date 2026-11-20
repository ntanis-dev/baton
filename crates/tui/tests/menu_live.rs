//! The menu in a (virtual) terminal: raw key bytes go in, the menu draws on a virtual screen, ~/.baton
//! is a fixture home, and `claude` is a stand-in that answers auth status and runs a pretend sign-in.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::io::{PipeWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use baton_core::clock::iso;
use baton_core::fsutil::{read_json_lines, write_json_atomic};
use baton_core::paths::{Paths, profile_dir};
use baton_core::theme::GlyphSet;
use baton_testkit::fixtures::{FAKE_LOGIN, launcher};
use baton_testkit::{TestHome, temp_home, test_env_with, until_for};
use baton_tui::signin::LoginStdio;
use baton_tui::{MenuOptions, run_menu};
use regex::Regex;
use serde_json::json;

use common::vt::VirtualTerminal;
use common::{build_home, now};

const UP: &str = "\x1b[A";
const DOWN: &str = "\x1b[B";
const ENTER: &str = "\r";
const ESCAPE: &str = "\x1b";
const CTRL_C: &str = "\x03";

/// What a live test starts the menu with.
#[derive(Clone, Copy)]
struct Start {
    columns: usize,
    rows: usize,
    refresh_ms: u64,
    splash: bool,
    mouse: bool,
}

impl Default for Start {
    fn default() -> Start {
        Start {
            columns: 120,
            rows: 30,
            refresh_ms: 60_000,
            splash: false,
            mouse: true,
        }
    }
}

/// A menu running on a thread of its own.
struct Menu {
    home: TestHome,
    input: Option<PipeWriter>,
    output: VirtualTerminal,
    done: Receiver<std::io::Result<i32>>,
    code: Option<i32>,
    log: PathBuf,
    refreshed: Arc<Mutex<Vec<Option<String>>>>,
}

impl Menu {
    fn paths(&self) -> &Paths {
        &self.home.paths
    }

    /// Waits until the screen shows a text.
    fn shows(&self, text: &str) {
        self.shows_where(|screen| screen.contains(text), text);
    }

    /// Waits until the screen matches a pattern.
    fn matches(&self, pattern: &str) {
        let regex = Regex::new(pattern).unwrap();
        self.shows_where(|screen| regex.is_match(screen), pattern);
    }

    fn shows_where(&self, found: impl Fn(&str) -> bool, what: &str) {
        let started = std::time::Instant::now();
        while !found(&self.output.text()) {
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "The menu never showed {what}. The screen:\n{}",
                self.output.text()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Types keys as the bytes a terminal sends, one after the other.
    fn type_keys(&mut self, list: &[&str]) {
        for bytes in list {
            let input = self.input.as_mut().unwrap();
            input.write_all(bytes.as_bytes()).unwrap();
            input.flush().unwrap();
            thread::sleep(Duration::from_millis(if *bytes == ESCAPE { 120 } else { 25 }));
        }
    }

    /// Waits for the menu to end; its exit code.
    fn finish(&mut self) -> i32 {
        if let Some(code) = self.code {
            return code;
        }
        let code = self
            .done
            .recv_timeout(Duration::from_secs(10))
            .expect("the menu ended")
            .unwrap();
        self.code = Some(code);
        code
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        if self.code.is_none() {
            if let Some(input) = self.input.as_mut() {
                let _ = input.write_all(CTRL_C.as_bytes());
            }
            let _ = self.done.recv_timeout(Duration::from_secs(10));
        }
    }
}

fn start(options: Start) -> Menu {
    let home = temp_home();
    build_home(&home.paths, false);
    // The stand-in's sign-ins: acct2 and acct3 signed in, acct4 not.
    for name in ["acct2", "acct3"] {
        write_json_atomic(&profile_dir(&home.paths, name).join("fake-signed-in"), &json!({})).unwrap();
    }
    let log = home.root.path().join("logins.jsonl");
    // The usage polls it asks for (the poller has its own tests).
    let refreshed: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
    let log_text = log.to_string_lossy().into_owned();
    let (reader, writer) = std::io::pipe().unwrap();
    let output = VirtualTerminal::new(options.columns, options.rows);
    let asked = Arc::clone(&refreshed);
    let menu = MenuOptions {
        console: Some(Box::new(output.clone())),
        input: Some(Box::new(reader)),
        refresh_ms: options.refresh_ms,
        watch_ms: 40,
        now: Some(Arc::new(now)),
        color: Some(false),
        glyphs: Some(GlyphSet::Rich),
        launcher: Some(launcher(FAKE_LOGIN)),
        login_stdio: LoginStdio::Ignore,
        splash: Some(options.splash),
        mouse: Some(options.mouse),
        poll: false,
        refresh: Some(Arc::new(move |account| {
            asked.lock().unwrap().push(account.map(str::to_owned));
            Ok(Vec::new())
        })),
        ..MenuOptions::new(
            home.paths.clone(),
            test_env_with(&home.home, &[("FAKE_LOGIN_LOG", &log_text)]),
        )
    };
    let (tx, done) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(run_menu(menu));
    });
    Menu {
        home,
        input: Some(writer),
        output,
        done,
        code: None,
        log,
        refreshed,
    }
}

fn screen_matches(menu: &Menu, pattern: &str) -> bool {
    Regex::new(pattern).unwrap().is_match(&menu.output.text())
}

#[test]
fn the_splash_waits_for_input_and_mouse_actions_open_directly() {
    let mut menu = start(Start {
        splash: true,
        ..Start::default()
    });
    menu.shows("Press any key to start");
    thread::sleep(Duration::from_millis(1100));
    assert!(menu.output.text().contains("Press any key to start"));
    menu.type_keys(&["\x1b[<0;8;8M"]);
    menu.shows("NEXT ACCOUNT");
    until_for(Duration::from_secs(8), || {
        (menu.output.mode("1000") == Some(true)).then_some(())
    });
    let screen = menu.output.text();
    let y = screen
        .split('\n')
        .position(|line| line.find("acct2").is_some_and(|at| at > 0 && at < 38) && line.contains("5h"))
        .unwrap();
    menu.type_keys(&[&format!("\x1b[<0;8;{}M", y + 1)]);
    menu.matches("#2 of 4");
    assert!(!screen_matches(&menu, "Enter.*open"));
    assert!(!screen_matches(&menu, "Account relay"));
    menu.type_keys(&[ENTER, "e"]);
    menu.shows("Rename acct2");
    menu.type_keys(&[ESCAPE, "q"]);
    assert_eq!(menu.finish(), 0);
    assert_eq!(menu.output.mode("1000"), Some(false));
}

#[test]
fn a_right_click_or_a_second_click_opens_the_actions_and_a_click_beside_closes_them() {
    let mut menu = start(Start::default());
    menu.shows("NEXT ACCOUNT");
    until_for(Duration::from_secs(8), || {
        (menu.output.mode("1000") == Some(true)).then_some(())
    });
    let screen = menu.output.text();
    let y = screen
        .split('\n')
        .position(|line| line.find("acct2").is_some_and(|at| at > 0 && at < 38) && line.contains("5h"))
        .unwrap();
    // The right button selects the row and opens its actions at once.
    menu.type_keys(&[&format!("\x1b[<2;8;{}M", y + 1)]);
    menu.shows("Account actions");
    // A click beside the menu closes it; the row stays selected.
    menu.type_keys(&["\x1b[<0;3;3M"]);
    menu.shows_where(|screen| !screen.contains("Account actions"), "the menu closed");
    menu.matches("#2 of 4");
    // A click on the row that is already selected opens its actions; the wheel moves the highlight.
    menu.type_keys(&[&format!("\x1b[<0;8;{}M", y + 1)]);
    menu.shows("Account actions");
    menu.type_keys(&["\x1b[<65;60;12M", ENTER]);
    menu.shows("Account mode");
    menu.type_keys(&["\x1b[<0;3;3M"]);
    menu.shows_where(|screen| !screen.contains("Account mode"), "the pick list closed");
    menu.type_keys(&["q"]);
    assert_eq!(menu.finish(), 0);
}

#[test]
fn mouse_reporting_can_be_disabled_while_keyboard_navigation_remains_direct() {
    let mut menu = start(Start {
        mouse: false,
        ..Start::default()
    });
    menu.shows("NEXT ACCOUNT");
    assert_ne!(menu.output.mode("1000"), Some(true));
    menu.type_keys(&["\x1b[<0;8;13M"]);
    assert!(menu.output.text().contains("NEXT ACCOUNT"));
    menu.type_keys(&[DOWN, DOWN]);
    menu.matches("#2 of 4");
    menu.type_keys(&["q"]);
    assert_eq!(menu.finish(), 0);
    assert_ne!(menu.output.mode("1000"), Some(true));
}

#[test]
fn usage_updates_live_in_the_fixed_workspace() {
    let mut menu = start(Start::default());
    menu.shows("NEXT ACCOUNT");
    // The week every model draws on: Fable's own limit (71%) is said when it blocks.
    menu.matches("acct2 +5h 42 · wk 63");
    write_json_atomic(
        &menu.paths().usage.join("acct2.json"),
        &json!({
            "account": "acct2",
            "at": iso(now()),
            "five_hour": { "used": 77, "resetsAt": iso(now() + 2 * 3_600_000), "readAt": iso(now()) },
            "seven_day": { "used": 64, "resetsAt": iso(now() + 4 * 86_400_000), "readAt": iso(now()) }
        }),
    )
    .unwrap();
    menu.matches("acct2 +5h 77 · wk 64");
    menu.type_keys(&["q"]);
    assert_eq!(menu.finish(), 0);
    assert_eq!(menu.output.mode("1049"), Some(false));
    assert_eq!(menu.output.mode("1000"), Some(false));
    assert_eq!(menu.output.mode("1006"), Some(false));
    assert!(menu.output.written().ends_with("\x1b[?7h"));
    // The screen that was there before the menu is back, the prompt and all.
    assert!(menu.output.text().starts_with("PS E:\\> x"));
}

#[test]
fn claude_sign_in_gets_a_branded_screen_and_returns_to_the_same_account() {
    let mut menu = start(Start::default());
    menu.matches("acct4 +sign in");
    menu.type_keys(&[DOWN, DOWN, DOWN, DOWN]);
    menu.matches("#4 of 4 · last-resort");
    menu.type_keys(&[ENTER, "s"]);
    menu.shows("CONNECT ACCOUNT");
    menu.shows("YOUR SIGN-IN LINK");
    menu.shows("Open Claude sign-in");
    assert_eq!(menu.output.mode("1000"), Some(false));
    until_for(Duration::from_secs(8), || {
        (read_json_lines(&menu.log).len() == 1).then_some(())
    });
    assert_eq!(
        read_json_lines(&menu.log)[0],
        json!({ "args": "auth login", "dir": profile_dir(menu.paths(), "acct4").to_string_lossy() })
    );
    menu.shows("finished");
    until_for(Duration::from_secs(8), || {
        (menu.output.mode("1000") == Some(true)).then_some(())
    });
    menu.matches("acct4 +5h 5 · wk 18");
    until_for(Duration::from_secs(8), || {
        menu.refreshed
            .lock()
            .unwrap()
            .contains(&Some("acct4".to_owned()))
            .then_some(())
    });
    menu.matches(r"signed in \(claude\.ai\)");
    assert!(!screen_matches(&menu, r"example\.invalid"));
    // The link is a hyperlink on the screen; its text never is.
    let written = menu.output.written();
    assert!(written.contains(
        "\x1b]8;;https://claude.ai/oauth/authorize?client_id=baton-fixture&state=public-fixture\x1b\\"
    ));
    menu.type_keys(&[UP]);
    menu.matches("#3 of 4 · normal");
    menu.type_keys(&["q"]);
    assert_eq!(menu.finish(), 0);
}

#[test]
fn a_small_terminal_shows_the_branded_error_and_resumes_when_resized() {
    let mut menu = start(Start {
        refresh_ms: 50,
        ..Start::default()
    });
    menu.shows("NEXT ACCOUNT");
    menu.output.resize(80, 24);
    menu.shows("Baton needs 100 x 28");
    menu.type_keys(&[DOWN, ENTER]);
    assert!(menu.output.text().contains("Baton needs 100 x 28"));
    menu.output.resize(120, 30);
    menu.shows("NEXT ACCOUNT");
    menu.type_keys(&["q"]);
    assert_eq!(menu.finish(), 0);
}

#[test]
fn the_end_of_the_input_ends_the_menu_and_gives_the_terminal_back() {
    let mut menu = start(Start::default());
    menu.shows("NEXT ACCOUNT");
    assert_eq!(menu.output.mode("1049"), Some(true));
    assert_eq!(menu.output.mode("25"), Some(false), "the cursor is hidden");
    assert_eq!(menu.output.mode("7"), Some(false), "line wrap is off");
    drop(menu.input.take());
    assert_eq!(menu.finish(), 0);
    assert_eq!(menu.output.mode("1049"), Some(false));
    assert_eq!(menu.output.mode("25"), Some(true));
    assert_eq!(menu.output.mode("7"), Some(true));
    assert_eq!(menu.output.mode("2004"), Some(false));
}

#[test]
fn only_the_rows_that_changed_are_drawn_again() {
    let mut menu = start(Start::default());
    menu.shows("NEXT ACCOUNT");
    // Let the first frames settle (the sign-ins arrive one by one).
    menu.matches("acct4 +sign in");
    thread::sleep(Duration::from_millis(300));
    let before = menu.output.written().len();
    // Up from the first row goes to the pinned search control: one row of the sidebar changes, and
    // the footer's hints.
    menu.type_keys(&[UP]);
    menu.shows("\u{203a} / Search");
    thread::sleep(Duration::from_millis(150));
    let moved = menu.output.written()[before..].to_owned();
    assert!(
        !moved.contains("\x1b[2J"),
        "the screen is never cleared for a change"
    );
    let placed = Regex::new("\x1b\\[[0-9]+;1H").unwrap().find_iter(&moved).count();
    assert!((2..20).contains(&placed), "{placed} rows were drawn again");
    assert!(moved.contains("Search") && moved.contains("Activate"));
    assert!(
        !moved.contains("Keep the conversation moving."),
        "the brand did not change"
    );
    assert!(!moved.contains("NEXT ACCOUNT"), "nor did the pane");
    menu.type_keys(&["q"]);
    assert_eq!(menu.finish(), 0);
}
