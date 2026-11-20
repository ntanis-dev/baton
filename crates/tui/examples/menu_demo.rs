//! A fully interactive fixture preview of the menu, in this terminal:
//!
//! ```text
//! cargo run -p baton-tui --example menu_demo
//! ```
//!
//! Every change stays in a temporary home that is removed at the end; no service, real Claude process,
//! profile or credential is involved. The home is the fixture of `tests/common` (four accounts, the
//! last one signed out), the clock starts at the fixture's moment and runs on from there, the sign-in
//! is the testkit's stand-in for `claude auth login`, and asking usage answers at once.
//!
//! `BATON_MOUSE=off`, `BATON_SPLASH=off`, `BATON_GLYPHS` and `NO_COLOR` work as in `baton view`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use baton_claude::poller::PollResult;
use baton_core::env::Env;
use baton_core::fsutil::write_json_atomic;
use baton_core::paths::profile_dir;
use baton_testkit::fixtures::{FAKE_LOGIN, launcher};
use baton_testkit::temp_home;
use baton_tui::signin::LoginStdio;
use baton_tui::{MenuOptions, run_menu};
use serde_json::json;

use common::{auth, build_home, now};

fn main() -> ExitCode {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        eprintln!("The menu demo needs a terminal.");
        return ExitCode::from(2);
    }
    // The stand-in for claude is built before the screen goes up.
    let claude = launcher(FAKE_LOGIN);
    let home = temp_home();
    build_home(&home.paths, false);
    for (id, status) in auth() {
        if status.logged_in == Some(true) {
            write_json_atomic(&profile_dir(&home.paths, &id).join("fake-signed-in"), &json!({})).unwrap();
        }
    }
    let started = Instant::now();
    let done = run_menu(MenuOptions {
        poll: false,
        now: Some(Arc::new(move || {
            now() + i64::try_from(started.elapsed().as_millis()).unwrap_or(0)
        })),
        launcher: Some(claude),
        login_stdio: LoginStdio::Ignore,
        handle_signals: true,
        refresh: Some(Arc::new(|account| {
            Ok(vec![PollResult {
                name: account.unwrap_or("Demo accounts").to_owned(),
                status: "ok".to_owned(),
                ..PollResult::default()
            }])
        })),
        ..MenuOptions::new(home.paths.clone(), Env::from_process().with("FAKE_LOGIN_LOG", ""))
    });
    match done {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("The menu stopped: {error}");
            ExitCode::FAILURE
        }
    }
}
