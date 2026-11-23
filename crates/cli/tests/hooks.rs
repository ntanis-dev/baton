//! `baton hook`, `baton statusline` and `baton policy --status-line-usage` as Claude Code and the
//! operator run them: the real program as a process, with what it is given on its stdin.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

mod common;
mod support;

use std::time::Instant;

use baton_core::clock::now_ms;
use baton_core::config::load_config;
use baton_core::env::Env;
use baton_core::paths::{Paths, profile_dir};
use baton_core::sessions::read_session_file;
use baton_core::usage::read_reading;
use baton_install::hooks::status_line_shell;
use baton_testkit::{TestHome, four_accounts_home, test_env, write_json};
use serde_json::{Value, json};
use support::spawn;

const SID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";

/// The environment of a claude running under acct2's profile in a test home.
fn env2(home: &TestHome) -> Env {
    let mut env = test_env(&home.home);
    env.remove("COLUMNS");
    env.set(
        "CLAUDE_CONFIG_DIR",
        profile_dir(&home.paths, "acct2").into_os_string(),
    );
    env
}

/// The same, for a status line as plain text with the rich glyphs.
fn plain_env(home: &TestHome, extra: &[(&str, &str)]) -> Env {
    let mut env = env2(home);
    for (name, value) in [("NO_COLOR", "1"), ("BATON_GLYPHS", "rich")].iter().chain(extra) {
        env.set(*name, *value);
    }
    env
}

/// The accounts of a home as config.json holds them, with this policy.
fn set_policy(paths: &Paths, policy: Value) {
    let accounts: Vec<Value> = load_config(paths)
        .accounts
        .iter()
        .map(|a| json!({ "id": a.id, "name": a.name, "mark": a.mark.as_str() }))
        .collect();
    write_json(
        &paths.config,
        &json!({ "version": 1, "accounts": accounts, "policy": policy }),
    );
}

/// A status line command for the shell Claude Code runs them in here: it prints `mine:`, the line it
/// was given on its input, and whether it runs under a profile.
fn echo_command(env: &Env) -> String {
    if status_line_shell(env)
        .command
        .to_string_lossy()
        .to_lowercase()
        .contains("powershell")
    {
        "[Console]::Out.Write('mine:' + [Console]::In.ReadToEnd() + ':' + $(if ($env:CLAUDE_CONFIG_DIR) { 'profile' } else { 'default' }))".to_owned()
    } else {
        "IFS= read -r line; printf 'mine:%s:%s' \"$line\" \"${CLAUDE_CONFIG_DIR:+profile}\"".to_owned()
    }
}

#[test]
fn the_hook_command_exits_0_silently_on_garbage() {
    let home = four_accounts_home();
    let env = test_env(&home.home);
    for input in ["", "{", "null", "[1,2]", "{\"session_id\": 3}"] {
        let run = spawn(&env, &["hook", "stop-failure"], input);
        assert_eq!(run.code, 0, "{input}");
        assert_eq!(format!("{}{}", run.stdout, run.stderr), "", "{input}");
    }
    // The same under one of Baton's profiles, and for an event Baton does not know.
    for (event, input) in [
        ("stop-failure", "{"),
        ("unknown-event", "{}"),
        ("stop", "\u{feff}nonsense"),
    ] {
        let run = spawn(&env2(&home), &["hook", event], input);
        assert_eq!(
            (run.code, run.stdout.as_str(), run.stderr.as_str()),
            (0, "", ""),
            "{event} {input}"
        );
    }
    let none = spawn(&env2(&home), &["hook"], "{}");
    assert_eq!(
        (none.code, none.stdout.as_str(), none.stderr.as_str()),
        (0, "", "")
    );
    assert!(!home.paths.sessions.exists(), "nothing was recorded");
}

#[test]
fn a_config_that_cannot_be_read_is_said_by_the_hooks_and_the_status_line_and_fails_nothing() {
    let home = four_accounts_home();
    std::fs::write(&home.paths.config, "{ half a config").unwrap();
    let input = json!({ "session_id": SID }).to_string();
    // Under one of Baton's profiles: the hook still exits 0, and says why it recorded nothing.
    let hook = spawn(&env2(&home), &["hook", "session-start"], &input);
    assert_eq!((hook.code, hook.stdout.as_str()), (0, ""));
    assert!(
        hook.stderr.starts_with("baton: ") && hook.stderr.contains("is not valid JSON"),
        "{}",
        hook.stderr
    );
    // The status line has nothing to show, and shows that.
    let line = spawn(&plain_env(&home, &[]), &["statusline"], &input);
    assert_eq!(line.code, 0);
    assert!(
        line.stdout.contains("is not valid JSON") && line.stdout.contains("baton doctor"),
        "{}",
        line.stdout
    );
    // Plain claude, in no profile of Baton's, hears nothing of it.
    let plain = spawn(&test_env(&home.home), &["hook", "session-start"], &input);
    assert_eq!(
        (plain.code, plain.stdout.as_str(), plain.stderr.as_str()),
        (0, "", "")
    );
    // `baton` itself says what is wrong, not that there are no accounts.
    let start = spawn(&test_env(&home.home), &["--version-of-nothing"], "");
    let said = format!("{}{}", start.stdout, start.stderr);
    assert!(said.contains("is not valid JSON"), "{said}");
    assert!(!said.contains("No accounts yet"), "{said}");
}

#[test]
fn a_settings_change_under_one_account_reaches_the_others_as_soon_as_claude_code_says_so() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let settings = |who: &str| {
        if who == "home" {
            paths.claude_dir.join("settings.json")
        } else {
            profile_dir(paths, who).join("settings.json")
        }
    };
    let theme = |who: &str| baton_core::fsutil::read_json(&settings(who)).unwrap()["theme"].clone();
    // Every profile holds a copy of the settings, as where file links are not allowed.
    for who in ["home", "acct1", "acct2", "acct3"] {
        let _ = std::fs::remove_file(settings(who));
        write_json(&settings(who), &json!({ "model": "opus", "theme": "dark" }));
    }
    let changed = |source: &str| {
        json!({
            "session_id": SID, "hook_event_name": "ConfigChange", "source": source,
            "file_path": settings("acct2").to_string_lossy()
        })
        .to_string()
    };
    // Baton has seen them in step (as it does whenever an account is used).
    spawn(
        &env2(&home),
        &["hook", "config-change"],
        &changed("user_settings"),
    );
    // /config under acct2 saves that profile's settings.json, and Claude Code runs the hook.
    write_json(&settings("acct2"), &json!({ "model": "opus", "theme": "light" }));
    let hook = spawn(
        &env2(&home),
        &["hook", "config-change"],
        &changed("user_settings"),
    );
    // It says nothing: a word from it could stand in the way of the change.
    assert_eq!(
        (hook.code, hook.stdout.as_str(), hook.stderr.as_str()),
        (0, "", "")
    );
    for who in ["home", "acct1", "acct3"] {
        assert_eq!(theme(who), "light", "{who}");
    }
    // A project's settings are not shared between accounts, and plain claude is not Baton's.
    write_json(&settings("acct2"), &json!({ "model": "opus", "theme": "blue" }));
    spawn(
        &env2(&home),
        &["hook", "config-change"],
        &changed("project_settings"),
    );
    spawn(
        &test_env(&home.home),
        &["hook", "config-change"],
        &changed("user_settings"),
    );
    assert_eq!(theme("acct3"), "light");
    spawn(
        &env2(&home),
        &["hook", "config-change"],
        &changed("user_settings"),
    );
    assert_eq!(theme("acct3"), "blue");
}

#[test]
fn hooks_are_fast_as_a_process() {
    let home = four_accounts_home();
    let env = env2(&home);
    let input = json!({ "session_id": SID }).to_string();
    // Built before the clock starts.
    assert!(baton_testkit::fixtures::baton().is_file());
    let mut runs = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let run = spawn(&env, &["hook", "stop"], &input);
        runs.push(started.elapsed().as_millis());
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert_eq!(run.stdout, "", "a hook prints nothing");
    }
    let best = runs.iter().min().copied().unwrap_or(u128::MAX);
    assert!(best < 1500, "{runs:?} ms");
    // And it did its work: the turn is recorded for the session.
    let turn = read_session_file(&home.paths, SID, "turn.json").unwrap();
    assert_eq!(
        (&turn["state"], &turn["event"], &turn["account"]),
        (&json!("idle"), &json!("stop"), &json!("acct2"))
    );
}

#[test]
fn the_status_line_runs_the_operators_previous_command_with_the_same_input() {
    let home = four_accounts_home();
    let paths = &home.paths;
    let env = env2(&home);
    let command = echo_command(&env);
    write_json(
        &paths.install,
        &json!({ "previousStatusLine": { "type": "command", "command": command } }),
    );
    let input = json!({
        "session_id": SID,
        "rate_limits": { "five_hour": { "used_percentage": 10, "resets_at": now_ms() / 1000 + 3600 } }
    })
    .to_string();
    let run = spawn(&env, &["statusline"], &input);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, format!("mine:{input}:profile"));
    assert_eq!(
        read_reading(paths, "acct2").unwrap().five_hour.unwrap().used,
        10.0,
        "recorded before passing on"
    );
    // Their command's exit code is the status line's.
    write_json(
        &paths.install,
        &json!({ "previousStatusLine": { "type": "command", "command": "exit 3" } }),
    );
    let failing = spawn(&env, &["statusline"], &input);
    assert_eq!((failing.code, failing.stdout.as_str()), (3, ""));
}

#[test]
fn baton_statusline_prints_batons_own_line_when_the_operator_had_none() {
    let home = four_accounts_home();
    let input = json!({
        "session_id": SID,
        "rate_limits": {
            "five_hour": { "used_percentage": 42.4, "resets_at": now_ms() / 1000 + 3600 },
            "seven_day": { "used_percentage": 63, "resets_at": now_ms() / 1000 + 86400 }
        }
    })
    .to_string();
    let run = spawn(&plain_env(&home, &[]), &["statusline"], &input);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.stdout,
        "\u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 42% \u{b7} week 63%\n"
    );
    assert_eq!(
        spawn(&plain_env(&home, &[("COLUMNS", "12")]), &["statusline"], &input).stdout,
        "\u{257a}\u{2501}\u{2578} acct2\n"
    );
    // Garbage says nothing new: the account alone. Plain claude: nothing at all. Never a failure.
    for (env, input, said) in [
        (plain_env(&home, &[]), "{", "\u{257a}\u{2501}\u{2578} acct2\n"),
        (test_env(&home.home), input.as_str(), ""),
        (test_env(&home.home), "", ""),
    ] {
        let run = spawn(&env, &["statusline"], input);
        assert_eq!(
            (run.code, run.stdout.as_str(), run.stderr.as_str()),
            (0, said, ""),
            "{input}"
        );
    }
}

#[test]
fn baton_statusline_prints_the_operators_line_and_with_status_line_usage_on_batons_after_it() {
    let home = four_accounts_home();
    let paths = &home.paths;
    write_json(
        &paths.install,
        &json!({ "previousStatusLine": { "type": "command", "command": "echo mine-ok" } }),
    );
    let input = json!({
        "session_id": SID,
        "rate_limits": { "five_hour": { "used_percentage": 10, "resets_at": now_ms() / 1000 + 3600 } }
    })
    .to_string();
    let run = |extra: &[(&str, &str)]| {
        let run = spawn(&plain_env(&home, extra), &["statusline"], &input);
        assert_eq!(run.code, 0, "{}", run.stderr);
        run.stdout.replace("\r\n", "\n")
    };
    set_policy(paths, json!({}));
    assert_eq!(run(&[]), "mine-ok\n", "off by default: theirs as it is");
    set_policy(paths, json!({ "statusLineUsage": true }));
    assert_eq!(
        run(&[]),
        "mine-ok  \u{257a}\u{2501}\u{2578} acct2 \u{b7} 5h 10%\n"
    );
    // Too narrow for any of it: theirs only.
    assert_eq!(run(&[("COLUMNS", "10")]), "mine-ok\n");
}

#[test]
fn baton_policy_status_line_usage_turns_the_addition_on_and_off_and_it_is_off_by_default() {
    let home = four_accounts_home();
    let env = test_env(&home.home);
    let policy = |args: &[&str]| spawn(&env, &[&["policy"], args].concat(), "");
    let value = || policy(&["--json"]).json()["statusLineUsage"].clone();
    assert_eq!(value(), false);
    assert_eq!(policy(&["--status-line-usage", "true"]).code, 0);
    assert_eq!(value(), true);
    let refused = policy(&["--status-line-usage=maybe"]);
    assert_eq!(refused.code, 2, "{}", refused.stdout);
    assert!(
        refused.stderr.contains("statusLineUsage is true or false."),
        "{}",
        refused.stderr
    );
    // The option alone turns it on; a word after it that is neither on nor off is not its value.
    assert_eq!(policy(&["--status-line-usage", "off"]).code, 0);
    assert_eq!(value(), false);
    assert_eq!(policy(&["--status-line-usage"]).code, 0);
    assert_eq!(value(), true);
    let stray = policy(&["--status-line-usage", "maybe"]);
    assert_eq!(stray.code, 2);
    assert!(
        stray.stderr.contains("Unexpected argument maybe"),
        "{}",
        stray.stderr
    );
    assert_eq!(value(), true, "a refused value changes nothing");
    assert_eq!(policy(&["--status-line-usage", "default"]).code, 0);
    assert_eq!(value(), false);
}
