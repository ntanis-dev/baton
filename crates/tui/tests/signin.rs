//! Claude Code's sign-in with Baton's screen around it: `baton login <account>`.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::io;
use std::sync::{Arc, Mutex};

use baton_core::claude_path::Launcher;
use baton_core::fsutil::read_json_lines;
use baton_core::paths::{profile_dir, profile_env};
use baton_core::theme::GlyphSet;
use baton_testkit::fixtures::{FAKE_LOGIN, launcher};
use baton_testkit::{temp_home, test_env_with};
use baton_tui::console::Console;
use baton_tui::signin::LoginStdio;
use baton_tui::{SignInOptions, sign_in};
use serde_json::json;

use common::vt::VirtualTerminal;

const LINK: &str = "https://claude.ai/oauth/authorize?client_id=baton-fixture&state=public-fixture";

/// Something that is no terminal: a pipe, a file.
#[derive(Clone, Default)]
struct Piped {
    written: Arc<Mutex<String>>,
}

impl Console for Piped {
    fn is_tty(&self) -> bool {
        false
    }

    fn size(&self) -> Option<(usize, usize)> {
        None
    }

    fn write(&mut self, text: &str) -> io::Result<()> {
        self.written.lock().unwrap().push_str(text);
        Ok(())
    }
}

#[test]
fn the_sign_in_runs_under_the_accounts_profile_with_batons_screen_around_it() {
    let home = temp_home();
    let log = home.root.path().join("logins.jsonl");
    let log_text = log.to_string_lossy().into_owned();
    let env = profile_env(
        &home.paths,
        "work",
        &test_env_with(&home.home, &[("FAKE_LOGIN_LOG", &log_text)]),
    );
    let screen = VirtualTerminal::new(120, 30);
    let code = sign_in(SignInOptions {
        console: Some(Box::new(screen.clone())),
        stdio: LoginStdio::Ignore,
        color: false,
        glyphs: Some(GlyphSet::Rich),
        ..SignInOptions::new(launcher(FAKE_LOGIN), env, "work@example.com")
    });
    assert_eq!(code, 0);
    // Claude Code's own sign-in ran, under the profile.
    assert_eq!(
        read_json_lines(&log),
        vec![json!({ "args": "auth login", "dir": profile_dir(&home.paths, "work").to_string_lossy() })]
    );
    assert!(profile_dir(&home.paths, "work").join("fake-signed-in").exists());
    let written = screen.written();
    // The screen: on the alternate screen, with the link as a hyperlink and never as text.
    assert!(written.starts_with("\x1b[?1049h\x1b[?7l\x1b[2J\x1b[H"));
    for shown in [
        "CONNECT ACCOUNT",
        "work@example.com",
        "YOUR SIGN-IN LINK",
        "Open Claude sign-in",
        "VERIFICATION CODE",
    ] {
        assert!(written.contains(shown), "{shown}");
    }
    assert!(written.contains(&format!("\x1b]8;;{LINK}\x1b\\")));
    assert!(
        !written
            .replace(&format!("\x1b]8;;{LINK}\x1b\\"), "")
            .contains("baton-fixture")
    );
    assert!(written.contains("claude.ai"));
    // Once it asks for a code the cursor shows after the prompt.
    assert!(written.contains("\x1b[27;52H\x1b[?25h"));
    // The terminal is given back as it was.
    assert!(written.ends_with("\x1b[0m\x1b[?25h\x1b[?1049l\x1b[?7h"));
    assert_eq!(screen.mode("1049"), Some(false));
    assert!(screen.text().starts_with("PS E:\\> x"));
}

#[test]
fn a_sign_in_that_cannot_start_or_fails_says_so_by_its_code() {
    let home = temp_home();
    let env = profile_env(&home.paths, "work", &test_env_with(&home.home, &[]));
    let screen = VirtualTerminal::new(120, 30);
    let missing = Launcher::program(home.home.join("no-such-claude"));
    let code = sign_in(SignInOptions {
        console: Some(Box::new(screen.clone())),
        stdio: LoginStdio::Ignore,
        ..SignInOptions::new(missing, env.clone(), "work")
    });
    assert_eq!(code, 1);
    assert_eq!(
        screen.mode("1049"),
        Some(false),
        "the terminal is given back either way"
    );
    // The stand-in ends with 2 for anything but `auth login` and `auth status`; a launcher that puts
    // something in front of those shows the code coming through.
    let mut odd = launcher(FAKE_LOGIN);
    odd.prefix = vec!["something".to_owned()];
    let code = sign_in(SignInOptions {
        console: Some(Box::new(screen.clone())),
        stdio: LoginStdio::Ignore,
        ..SignInOptions::new(odd, env, "work")
    });
    assert_eq!(code, 2);
}

#[test]
fn where_there_is_no_terminal_claude_code_writes_for_itself() {
    let home = temp_home();
    let env = profile_env(&home.paths, "work", &test_env_with(&home.home, &[]));
    let piped = Piped::default();
    let code = sign_in(SignInOptions {
        console: Some(Box::new(piped.clone())),
        stdio: LoginStdio::Ignore,
        ..SignInOptions::new(launcher(FAKE_LOGIN), env, "work")
    });
    assert_eq!(code, 0);
    assert_eq!(
        piped.written.lock().unwrap().as_str(),
        "",
        "no screen is drawn into a pipe"
    );
    assert!(profile_dir(&home.paths, "work").join("fake-signed-in").exists());
}
