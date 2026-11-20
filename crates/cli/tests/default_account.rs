//! Baton never interacts with the default Claude Code account (operator decision, 2026-10-25): its
//! accounts are only the ones the operator adds. Plain `claude` firing Baton's hooks and status line
//! leaves nothing behind, and /hop says where it works.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot run baton or read what it left stops the test at once"
)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use baton_core::env::Env;
use baton_core::fsutil::read_json;
use baton_core::paths::profile_dir;
use baton_install::hooks::status_line_shell;
use baton_testkit::{accounts_home, fixtures, temp_home, test_env, write_json};
use common::baton;
use serde_json::{Value, json};

/// A conversation's id.
const SID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";

/// Runs the real `baton` program with these arguments, `input` on its stdin and this environment.
fn run(exe: &Path, args: &[&str], input: &str, env: &Env) -> Output {
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    env.apply(&mut command);
    let mut child = command.spawn().unwrap();
    // A program that ends without reading its input is no failure of the test.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

/// What a program printed.
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every file under a folder with its content, by its path from that folder.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    /// Adds the files under `dir` to `out`, named from `root`.
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// A status line command of the operator's own, for the shell Claude Code runs them in here: it prints
/// `mine:` and the line it was given on its input.
fn own_status_line(env: &Env) -> &'static str {
    if status_line_shell(env)
        .command
        .to_string_lossy()
        .to_lowercase()
        .contains("powershell")
    {
        "[Console]::Out.Write('mine:' + [Console]::In.ReadToEnd())"
    } else {
        "IFS= read -r line; printf 'mine:%s' \"$line\""
    }
}

#[test]
fn plain_claude_firing_the_installed_hooks_and_status_line_leaves_no_baton_state_and_the_status_line_only_runs_the_operators_own()
 {
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let root = home.home.parent().unwrap();
    let exe = fixtures::baton();
    let env_with = |config_dir: Option<&Path>| {
        let mut env = test_env(&home.home);
        if let Some(dir) = config_dir {
            env.set("CLAUDE_CONFIG_DIR", dir.as_os_str());
        }
        env
    };
    let before = tree(&paths.root);
    let status_input = json!({
        "session_id": SID,
        "model": { "id": "m" },
        "cwd": "E:/w",
        "rate_limits": {
            "five_hour": { "used_percentage": 77, "resets_at": 1_792_920_000 },
            "seven_day": { "used_percentage": 33, "resets_at": 1_793_520_000 },
        },
    })
    .to_string();
    let events = [
        ("session-start", json!({ "source": "startup", "cwd": "E:/w" })),
        ("prompt", json!({ "prompt": "hi", "permission_mode": "default" })),
        ("stop", json!({ "background_tasks": [] })),
        (
            "stop-failure",
            json!({ "error": "rate_limit", "last_assistant_message": "Session limit reached \u{2219} resets 3pm" }),
        ),
        ("session-end", json!({ "reason": "exit" })),
    ];
    // No CLAUDE_CONFIG_DIR; ~/.claude itself; another folder; a profile folder of an account Baton does
    // not have.
    let elsewhere = root.join("elsewhere");
    let ghost = paths.accounts.join("ghost");
    for config_dir in [
        None,
        Some(paths.claude_dir.as_path()),
        Some(elsewhere.as_path()),
        Some(ghost.as_path()),
    ] {
        let env = env_with(config_dir);
        for (event, extra) in &events {
            let mut input = json!({ "session_id": SID });
            input
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            let r = run(&exe, &["hook", event], &input.to_string(), &env);
            assert_eq!(
                r.status.code(),
                Some(0),
                "{event}: {}",
                String::from_utf8_lossy(&r.stderr)
            );
            assert_eq!(stdout(&r), "", "{event}");
        }
        let line = run(&exe, &["statusline"], &status_input, &env);
        assert_eq!(
            line.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&line.stderr)
        );
        assert_eq!(stdout(&line), "", "nothing of Baton's in plain claude");
    }
    assert_eq!(tree(&paths.root), before, "no Baton state at all");
    // With the operator's own status line, that one runs, with the same input.
    let plain = env_with(None);
    write_json(
        &paths.install,
        &json!({ "previousStatusLine": { "type": "command", "command": own_status_line(&plain) } }),
    );
    let theirs = run(&exe, &["statusline"], &status_input, &plain);
    assert_eq!(
        stdout(&theirs),
        format!("mine:{status_input}"),
        "{}",
        String::from_utf8_lossy(&theirs.stderr)
    );
    assert!(!paths.usage.exists() && !paths.sessions.exists());
    // Under one of Baton's profiles the same input is recorded.
    let own = profile_dir(paths, "acct1");
    run(&exe, &["statusline"], &status_input, &env_with(Some(&own)));
    let reading: Value = read_json(&paths.usage.join("acct1.json")).unwrap();
    assert_eq!(reading["five_hour"]["used"], 77);
}

#[test]
fn hop_in_a_conversation_not_started_with_baton_says_it_only_works_there() {
    let home = accounts_home(&["acct1", "acct2"]);
    let r = baton(&home, &["hop", SID, "--from-command"]);
    assert_eq!(r.code, 0, "the /hop command shows the answer to the model");
    assert!(
        r.stdout
            .starts_with("Baton: /hop and baton hop only work in sessions started with baton"),
        "{}",
        r.stdout
    );
    assert_eq!(baton(&home, &["hop", SID]).code, 1);
}

#[test]
fn with_no_accounts_baton_asks_for_one_and_starts_nothing_and_status_and_the_account_list_say_how() {
    let home = temp_home();
    let dir = home.home.parent().unwrap().join("fake");
    write_json(
        &dir.join("scenario.json"),
        &json!({ "session": SID, "runs": [{ "steps": [{ "exit": 0 }] }] }),
    );
    // `baton` alone, with a claude it could start: it asks for an account and starts nothing.
    let env = test_env(&home.home)
        .with(
            "BATON_CLAUDE",
            fixtures::bin(fixtures::FAKE_CLAUDE).into_os_string(),
        )
        .with("FAKE_CLAUDE_DIR", dir.as_os_str());
    let start = run(&fixtures::baton(), &[], "", &env);
    assert_eq!(start.status.code(), Some(1));
    let said = format!("{}{}", stdout(&start), String::from_utf8_lossy(&start.stderr));
    assert!(
        said.contains(
            "No accounts yet: add one with baton account add <name> (or a in the menu), then sign it in with baton login <name>"
        ),
        "{said}"
    );
    assert!(
        !dir.join("invocations.jsonl").exists(),
        "claude was never started"
    );
    let commands: [&[&str]; 3] = [&["status"], &["account", "list"], &["usage"]];
    for command in commands {
        let r = baton(&home, command);
        assert!(
            r.stdout.contains("No accounts yet"),
            "{}: {}",
            command.join(" "),
            r.stdout
        );
        assert!(
            r.stdout.contains("baton account add <name>"),
            "{}: {}",
            command.join(" "),
            r.stdout
        );
    }
    let pick = baton(&home, &["pick"]);
    assert_eq!(pick.code, 3);
    assert!(pick.stdout.starts_with("No accounts yet"), "{}", pick.stdout);
}
