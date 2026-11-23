//! `baton doctor` from the command line: Claude Code and its version, an account's links and sign-in
//! (the method only, never who), and whether Baton is installed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

mod common;
mod support;

use std::path::PathBuf;

use baton_testkit::{TestHome, fixtures, temp_home, write, write_json};
use serde_json::{Value, json};
use support::baton;

/// The stand-in claude: it says its version, and for `auth status --json` whether it runs under a
/// profile, with an email that Baton must never repeat.
fn claude_stub() -> PathBuf {
    fixtures::bin(fixtures::CLAUDE_STUB)
}

/// A test home whose Claude Code is the stand-in.
fn home_with_claude() -> TestHome {
    let home = temp_home();
    write_json(
        &home.paths.config,
        &json!({ "version": 1, "accounts": [], "claudePath": claude_stub().to_string_lossy() }),
    );
    home
}

/// The checks of a report, by name.
fn check<'a>(report: &'a Value, name: &str) -> Option<&'a Value> {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == name)
}

#[test]
fn doctor_reports_claude_links_sign_in_by_method_only_and_the_install() {
    let home = home_with_claude();
    assert_eq!(baton(&home, &["account", "add", "acct2"]).code, 0);
    let run = baton(&home, &["doctor", "--json"]);
    let report = run.json();
    let claude = check(&report, "claude").unwrap();
    assert_eq!(claude["ok"], true, "{claude}");
    assert!(claude["detail"].as_str().unwrap().contains("2.1.302"), "{claude}");
    assert_eq!(
        check(&report, "account acct2 sign-in").unwrap()["detail"],
        "signed in (claude.ai)"
    );
    // Only Baton's own accounts, never the default Claude Code account.
    assert_eq!(check(&report, "account default sign-in"), None);
    let sign_ins: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|check| check["name"].as_str())
        .filter(|name| name.ends_with("sign-in"))
        .collect();
    assert_eq!(sign_ins, ["account acct2 sign-in"]);
    assert_eq!(check(&report, "status line").unwrap()["ok"], false);
    assert_eq!(report["ok"], false);
    assert_eq!(run.code, 1);
    assert!(!run.stdout.contains("example.invalid"), "no email is printed");
    let screen = baton(&home, &["doctor"]);
    assert_eq!(screen.code, 1);
    assert!(!screen.stdout.contains("example.invalid"), "{}", screen.stdout);
    assert!(
        screen.stdout.contains("signed in (claude.ai)"),
        "{}",
        screen.stdout
    );
}

#[test]
fn doctor_does_not_flag_a_fresh_account_for_history_jsonl_whether_it_is_a_link_or_a_copy() {
    let home = home_with_claude();
    // The shared base has a prompt history, as soon as Claude Code has run.
    write(&home.paths.claude_dir.join("history.jsonl"), "{}\n");
    // Adding an account links its profile, as on a fresh install.
    assert_eq!(baton(&home, &["account", "add", "acct2"]).code, 0);
    let report = baton(&home, &["doctor", "--json"]).json();
    let links = check(&report, "account acct2 links").unwrap();
    let detail = links["detail"].as_str().unwrap();
    assert_eq!(links["ok"], true, "{detail}");
    assert!(!detail.contains("history.jsonl missing"), "{detail}");
    // Where file links are off, the history is a copy like the other shared files.
    let file_links = check(&report, "file links").unwrap()["detail"].as_str().unwrap();
    if !file_links.starts_with("allowed") {
        assert!(detail.contains("1 copied"), "{detail}");
        assert!(file_links.contains("history.jsonl are copies"), "{file_links}");
    }
}

#[test]
fn doctor_says_what_went_wrong_with_an_accounts_usage_check() {
    let home = home_with_claude();
    for name in ["acct2", "acct3", "acct4"] {
        assert_eq!(baton(&home, &["account", "add", name]).code, 0);
    }
    write_json(
        &baton_core::poll::poll_file(&home.paths),
        &json!({ "accounts": {
            "acct2": {
                "status": "failed", "reason": "claude exited with 1", "failures": 3,
                "okAt": "2026-11-20T10:00:00.000Z", "backoffUntil": "2026-11-23T12:00:00.000Z"
            },
            "acct3": { "status": "rate-limited", "reason": "asked too often" },
            "acct4": { "status": "ok", "okAt": "2026-11-23T10:00:00.000Z" }
        } }),
    );
    let report = baton(&home, &["doctor", "--json"]).json();
    let failed = check(&report, "account acct2 usage check").unwrap();
    assert_eq!(failed["ok"], false);
    assert_eq!(
        failed["detail"],
        "failed: claude exited with 1; it last answered 2026-11-20T10:00:00.000Z; 3 times in a row; asked again after 2026-11-23T12:00:00.000Z"
    );
    // Patience asked for by the endpoint is said, and is no fault.
    let limited = check(&report, "account acct3 usage check").unwrap();
    assert_eq!(limited["ok"], Value::Null);
    assert_eq!(
        limited["detail"],
        "rate-limited: asked too often; it has not answered yet"
    );
    assert!(check(&report, "account acct4 usage check").is_none());
}

#[test]
fn doctor_says_which_baton_it_is_and_what_is_missing_before_and_after_install() {
    let home = home_with_claude();
    let before = baton(&home, &["doctor", "--json"]).json();
    let first = &before["checks"][0];
    assert_eq!(first["name"], "baton");
    assert_eq!(
        first["detail"],
        format!("{} ({})", env!("CARGO_PKG_VERSION"), support::EXE)
    );
    assert_eq!(check(&before, "accounts").unwrap()["ok"], false);
    assert_eq!(
        check(&before, "status line").unwrap()["detail"],
        "not installed: baton install"
    );
    assert_eq!(
        check(&before, "hooks").unwrap()["detail"],
        "missing SessionStart, UserPromptSubmit, Stop, StopFailure, SessionEnd, ConfigChange: baton install"
    );
    assert_eq!(
        check(&before, "/hop command").unwrap()["detail"],
        "not installed (baton hop still works)"
    );
    assert_eq!(
        check(&before, "Ferry").unwrap(),
        &json!({ "name": "Ferry", "ok": null, "detail": "not installed" })
    );
    assert_eq!(baton(&home, &["install"]).code, 0);
    let after = baton(&home, &["doctor", "--json"]).json();
    assert_eq!(
        check(&after, "status line").unwrap(),
        &json!({ "name": "status line", "ok": true, "detail": "installed" })
    );
    assert_eq!(check(&after, "/hop command").unwrap()["ok"], true);
    // The hooks name a baton that is not on this machine: doctor says so.
    assert_eq!(
        check(&after, "hooks").unwrap()["detail"],
        "paths gone for SessionStart, UserPromptSubmit, Stop, StopFailure, SessionEnd, ConfigChange: baton install again"
    );
}
