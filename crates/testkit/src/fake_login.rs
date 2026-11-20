//! A stand-in for `claude auth status --json` and `claude auth login` in the menu's tests: a profile
//! counts as signed in once "login" ran under it (a marker file in its `CLAUDE_CONFIG_DIR`); plain
//! claude (no `CLAUDE_CONFIG_DIR`) is always signed in. `FAKE_LOGIN_LOG` names a file each login is
//! logged to.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use baton_core::fsutil::append_json_line;
use serde_json::json;

/// Runs the stand-in with these arguments; returns its exit code.
pub fn run(argv: &[String]) -> i32 {
    let args = argv.join(" ");
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from);
    let marker = dir.as_ref().map(|dir| dir.join("fake-signed-in"));
    if args == "auth status --json" {
        let logged_in = marker.as_ref().is_none_or(|marker| marker.exists());
        let said = json!({
            "loggedIn": logged_in,
            "authMethod": if logged_in { "claude.ai" } else { "none" },
            "email": "someone@example.invalid",
        });
        print!("{said}");
        let _ = std::io::stdout().flush();
        return if logged_in { 0 } else { 1 };
    }
    if args != "auth login" {
        return 2;
    }
    if let (Some(dir), Some(marker)) = (&dir, &marker) {
        fs::create_dir_all(dir).expect("the profile folder");
        fs::write(marker, "").expect("the sign-in marker");
    }
    if let Some(log) = std::env::var_os("FAKE_LOGIN_LOG").filter(|log| !log.is_empty()) {
        let entry = json!({ "args": args, "dir": dir.as_ref().map(|dir| dir.to_string_lossy()) });
        append_json_line(&PathBuf::from(log), &entry).expect("the login log");
    }
    println!("https://claude.ai/oauth/authorize?client_id=baton-fixture&state=public-fixture");
    print!("Paste the verification code here: ");
    let _ = std::io::stdout().flush();
    // Long enough for the test to see the menu step aside.
    thread::sleep(Duration::from_millis(300));
    0
}
