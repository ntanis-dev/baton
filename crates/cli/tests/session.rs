//! The real `baton` program as a process: what it says without accounts, and a whole session around a
//! stand-in for `claude`: its place in the usage poll, and an account renamed while it runs.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot start or read a session stops the test at once"
)]

mod common;
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use baton_claude::terminate::{force_kill, wait_exit};
use baton_core::clock::now_ms;
use baton_core::env::Env;
use baton_core::fsutil::{is_alive, read_json, read_json_lines};
use baton_core::json::Get;
use baton_core::paths::profile_dir;
use baton_core::poll::poll_holder;
use baton_testkit::fixtures::{self, FAKE_CLAUDE_BOTH};
use baton_testkit::{TestHome, accounts_home, temp_home, test_env, until, until_for, write, write_json};
use common::baton;
use serde_json::{Value, json};
use support::spawn;

/// A conversation's id.
const SID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";
/// An email as a name.
const EMAIL: &str = "jane.work+baton@example.com";

/// One session at a time: each is a wrapper, a stand-in claude and its hooks, all on the clock.
fn serial() -> MutexGuard<'static, ()> {
    static ONE: Mutex<()> = Mutex::new(());
    ONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The folder a test home is in: where the stand-in claude keeps its scenario and its logs.
fn root_of(home: &TestHome) -> PathBuf {
    home.home.parent().unwrap().to_path_buf()
}

/// The environment of a session in a test home: `claude` is the stand-in that plays the scenario in
/// `dir` and answers the usage poll.
fn session_env(home: &TestHome, dir: &Path) -> Env {
    test_env(&home.home)
        .with("BATON_CLAUDE", fixtures::bin(FAKE_CLAUDE_BOTH).into_os_string())
        .with("FAKE_CLAUDE_DIR", dir.as_os_str())
        .with("FAKE_BATON", fixtures::baton().into_os_string())
}

/// A `baton` session running as a process. Dropped before its end, it is ended with everything it
/// started, so a failed test leaves nothing running.
struct Session {
    /// The wrapper's process, until its end was waited for.
    child: Option<Child>,
    /// Where what it and its claude print goes.
    log: PathBuf,
}

impl Session {
    /// Starts `baton` with these arguments in `cwd`: no terminal, its output kept in a file there.
    fn start(env: &Env, cwd: &Path, args: &[&str]) -> Session {
        let log = cwd.join("session.log");
        let out = fs::File::create(&log).unwrap();
        let err = out.try_clone().unwrap();
        let mut command = Command::new(fixtures::baton());
        command
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        env.apply(&mut command);
        Session {
            child: Some(command.spawn().unwrap()),
            log,
        }
    }

    /// The wrapper's process id.
    fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    /// Waits for the session to end: its exit code, and what it and its claude printed.
    fn finish(&mut self) -> (Option<i32>, String) {
        let child = self.child.as_mut().unwrap();
        assert!(wait_exit(child, 60_000), "the session did not end");
        let code = child.wait().unwrap().code();
        self.child = None;
        (
            code,
            String::from_utf8_lossy(&fs::read(&self.log).unwrap()).into_owned(),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            force_kill(child, None);
            let _ = child.wait();
        }
    }
}

/// Whether a text starts like a version: a number, a dot, a number, a dot, a digit.
fn starts_like_a_version(text: &str) -> bool {
    let mut parts = text.splitn(3, '.');
    let number =
        |part: Option<&str>| part.is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    number(parts.next())
        && number(parts.next())
        && parts
            .next()
            .is_some_and(|p| p.starts_with(|c: char| c.is_ascii_digit()))
}

/// The process ids a log of starts (one JSON object per line) names.
fn pids(file: &Path) -> Vec<i64> {
    read_json_lines(file)
        .iter()
        .filter_map(|start| start.int("pid"))
        .collect()
}

#[test]
fn the_bin_runs_as_a_process_and_baton_alone_starts_a_session_which_asks_for_an_account_first() {
    let home = temp_home();
    let env = test_env(&home.home);
    let status = spawn(&env, &["status"], "");
    assert_eq!(status.code, 0, "{}", status.stderr);
    assert!(status.stdout.contains("No accounts yet"), "{}", status.stdout);
    assert!(
        status.stdout.contains("baton account add <name>"),
        "{}",
        status.stdout
    );
    assert!(!status.stdout.contains('\x1b'), "piped output carries no escapes");
    let start = spawn(&env, &[], "");
    assert_eq!(start.code, 1, "{}", start.stderr);
    assert!(
        format!("{}{}", start.stdout, start.stderr).contains("No accounts yet"),
        "{start:?}"
    );
    let version = spawn(&env, &["--version"], "").stdout;
    assert!(starts_like_a_version(&version), "{version}");
}

#[test]
fn the_menu_needs_a_terminal() {
    let home = temp_home();
    let view = spawn(&test_env(&home.home), &["view"], "");
    assert_eq!(view.code, 2);
    assert!(
        view.stderr
            .contains("The menu needs a terminal; try baton status"),
        "{}",
        view.stderr
    );
}

#[test]
fn baton_serve_is_gone_not_a_command_not_in_the_help() {
    let home = accounts_home(&["acct1"]);
    let serve = baton(&home, &["serve"]);
    assert_eq!(serve.code, 2);
    assert!(serve.stderr.contains("Unknown command serve"), "{}", serve.stderr);
    let help = baton(&home, &["help"]).stdout;
    let words: Vec<&str> = help.split(|c: char| !c.is_alphanumeric() && c != '_').collect();
    assert!(!words.contains(&"serve") && !help.contains("service"), "{help}");
    let usage = spawn(&test_env(&home.home), &["help", "usage"], "");
    assert!(
        usage.stdout.contains("baton usage [--refresh] [account]"),
        "{}",
        usage.stdout
    );
    let readme = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
        .join("README.md");
    assert!(!fs::read_to_string(readme).unwrap().contains("baton serve"));
}

#[test]
fn a_baton_session_is_an_instance_it_polls_while_it_runs_says_so_in_baton_status_and_leaves_nothing_running()
{
    let _serial = serial();
    let home = accounts_home(&["acct1"]);
    let paths = &home.paths;
    let root = root_of(&home);
    let dir = root.join("fake");
    write_json(
        &dir.join("scenario.json"),
        &json!({ "session": SID, "runs": [{ "steps": [{ "waitFile": "done" }, { "exit": 0 }] }] }),
    );
    let claudes = root.join("claudes.jsonl");
    let env = session_env(&home, &dir).with("FAKE_USAGE_LOG", claudes.as_os_str());
    let mut session = Session::start(&env, &root, &["-p", "hello"]);
    // Its start polls (no poll yet), under the account's profile.
    let usage = paths.usage.join("acct1.json");
    until_for(Duration::from_secs(30), || {
        read_json(&usage)
            .filter(|reading| reading.text("source") == Some("poll"))
            .map(|_| ())
    });
    let status = baton(&home, &["status"]).stdout;
    let by = format!("Usage poll: by baton (pid {}), every 30m, next ", session.pid());
    assert!(status.contains(&by), "{status}");
    write(&dir.join("done"), "");
    let (code, said) = session.finish();
    assert_eq!(code, Some(0), "{said}");
    assert_eq!(said, "", "the poll prints nothing in claude's terminal");
    assert_eq!(poll_holder(paths, now_ms()), None);
    let status = baton(&home, &["status"]).stdout;
    assert!(status.contains("Usage poll: no baton running"), "{status}");
    for pid in pids(&dir.join("invocations.jsonl")) {
        assert!(!is_alive(pid), "the session's claude");
    }
    let reads = pids(&claudes);
    assert!(!reads.is_empty(), "the poll ran a claude of its own");
    for pid in reads {
        assert!(!is_alive(pid), "the poll's claude");
    }
}

#[test]
fn renamed_to_an_email_while_a_session_runs_its_hooks_land_on_the_same_account_and_the_hop_speaks_the_new_name()
 {
    let _serial = serial();
    let home = temp_home();
    let paths = &home.paths;
    let root = root_of(&home);
    let dir = root.join("fake");
    // Strict order with acct2 first: the session starts on acct2. The move is at once, and claude is
    // ended as the tests end it.
    write_json(
        &paths.config,
        &json!({
            "version": 1,
            "accounts": [{ "id": "acct2", "name": "acct2" }, { "id": "acct1", "name": "acct1" }],
            "policy": { "mode": "order", "countdownSeconds": 0, "settleMs": 50 },
            "endMethod": "kill",
        }),
    );
    write_json(
        &dir.join("scenario.json"),
        &json!({
            "session": SID,
            "runs": [
                { "steps": [
                    { "hook": "session-start" },
                    { "waitFile": "renamed" },
                    { "statusline": { "five_hour": [100, 3 * 3600], "seven_day": [30, 72 * 3600] } },
                    { "hook": "prompt" },
                    // The turn takes a moment, as a turn does: the wrapper looks at it four times a
                    // second, and this stand-in runs its hooks faster than that.
                    { "sleep": 700 },
                    { "hook": "stop-failure", "error": "rate_limit" },
                    { "wait": "end" },
                ] },
                { "steps": [{ "hook": "session-start" }, { "sleep": 700 }, { "exit": 0 }] },
            ],
        }),
    );
    // Nobody is signed in as far as the usage poll can tell: it asks, and records nothing.
    let env = session_env(&home, &dir).with("FAKE_LOGGED_IN", "0");
    let mut session = Session::start(&env, &root, &[]);
    until(|| {
        let started = fs::read_dir(&paths.wrappers).ok()?;
        started
            .flatten()
            .any(|file| file.file_name().to_string_lossy().ends_with(".session.json"))
            .then_some(())
    });
    assert_eq!(baton(&home, &["account", "rename", "acct2", EMAIL]).code, 0);
    write(&dir.join("renamed"), "");
    let (code, said) = session.finish();
    assert_eq!(code, Some(0), "{said}");
    // The status line under acct2's folder wrote acct2's reading; the refusal is acct2's.
    assert_eq!(
        read_json(&paths.usage.join("acct2.json")).unwrap()["five_hour"]["used"],
        100
    );
    assert!(paths.rejected.join("acct2.five_hour.json").exists());
    // What the operator reads uses the new name, as typed.
    let moved = said.lines().any(|line| {
        line.split_once("jane.work+baton@example.com reached its 5-hour limit ")
            .is_some_and(|(_, rest)| rest.contains(" moving this conversation to acct1"))
    });
    assert!(moved, "{said}");
    let invocations = read_json_lines(&dir.join("invocations.jsonl"));
    assert_eq!(invocations.len(), 2, "{invocations:?}");
    let config_dir = |n: usize| PathBuf::from(invocations[n].text("configDir").unwrap());
    assert_eq!(config_dir(0), profile_dir(paths, "acct2"));
    assert_eq!(config_dir(1), profile_dir(paths, "acct1"));
    let argv = |n: usize| invocations[n]["argv"].as_array().unwrap().clone();
    let prompt = argv(1).last().and_then(Value::as_str).unwrap().to_owned();
    assert!(
        prompt.contains("moved to another account because the account it ran on reached its 5-hour limit"),
        "{prompt}"
    );
    // Nothing Claude Code is started with says the name: folders and the environment carry ids
    // only, and the continue prompt, text for the model, names no account.
    for invocation in &invocations {
        assert!(
            !invocation.to_string().contains(EMAIL),
            "no name in arguments, folders or the prompt: {invocation}"
        );
    }
    let stats = baton(&home, &["stats", "--json"]).json();
    let hops: Vec<Value> = stats["hops"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| json!([h["from"], h["to"], h["fromId"]]))
        .collect();
    assert_eq!(hops, [json!([EMAIL, "acct1", "acct2"])]);
}
