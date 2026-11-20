//! The stand-ins themselves: the scenario player runs the commands a real claude would, and the
//! sign-in stand-in remembers a login.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use baton_core::fsutil::read_json_lines;
use baton_testkit::fixtures::{FAKE_CLAUDE, FAKE_CLAUDE_BOTH, FAKE_LOGIN, FAKE_USAGE_CLAUDE, bin};
use baton_testkit::{temp_home, until_for, write_json};
use serde_json::{Value, json};

const SID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";

/// A fake claude in `dir`, with a stand-in for `baton` that logs how it was run.
fn fake_claude(dir: &Path, baton_log: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(bin(FAKE_CLAUDE));
    command
        .args(args)
        .current_dir(dir)
        .env("FAKE_CLAUDE_DIR", dir)
        .env("FAKE_BATON", bin(FAKE_USAGE_CLAUDE))
        .env("FAKE_USAGE_LOG", baton_log)
        .env("CLAUDE_CONFIG_DIR", dir.join("profile"))
        .env("BATON_WRAPPER", "w1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn kinds(steps: &[Value]) -> Vec<&str> {
    steps.iter().map(|step| step["kind"].as_str().unwrap()).collect()
}

#[test]
fn the_fake_claude_plays_its_scenario_and_runs_batons_commands_the_way_claude_code_does() {
    let home = temp_home();
    let dir = home.home.join("fake");
    let baton_log = home.home.join("baton.jsonl");
    write_json(
        &dir.join("scenario.json"),
        &json!({
            "runs": [
                { "steps": [
                    { "hook": "session-start" },
                    { "statusline": { "five_hour": [42, 7200], "seven_day": [63, null] }, "effort": "high" },
                    { "hook": "prompt", "permission_mode": "default" },
                    { "nested": { "hook": "session-start", "session_id": "inner" } },
                    { "hop": "acct2" },
                    { "sleep": 10 },
                    { "exit": 3 },
                ] },
                { "steps": [{ "hook": "session-start" }, { "waitFile": "go" }] },
            ],
        }),
    );
    let first = fake_claude(&dir, &baton_log, &["--session-id", SID, "--model", "opus"])
        .status()
        .unwrap();
    assert_eq!(first.code(), Some(3));
    let steps = read_json_lines(&dir.join("steps.jsonl"));
    assert_eq!(
        kinds(&steps),
        ["hook", "statusline", "hook", "nested", "hop", "exit"]
    );
    assert!(
        steps
            .iter()
            .all(|step| step["kind"] == "exit" || step["code"] == 0),
        "{steps:?}"
    );
    assert_eq!(steps[0]["hook"], "session-start");
    assert_eq!(
        steps[5],
        json!({ "at": steps[5]["at"], "pid": steps[5]["pid"], "kind": "exit", "code": 3, "run": 0 })
    );
    // What it ran, with Claude Code's pid for its own hooks and another for a claude inside it.
    let ran = read_json_lines(&baton_log);
    let argv: Vec<&Value> = ran.iter().map(|run| &run["argv"]).collect();
    assert_eq!(
        argv,
        [
            &json!(["hook", "session-start"]),
            &json!(["statusline"]),
            &json!(["hook", "prompt"]),
            &json!(["hook", "session-start"]),
            &json!(["hop", "--session", SID, "--from-command", "--to=acct2"]),
        ]
    );
    for run in &ran {
        let names = run["envNames"].as_array().unwrap();
        assert!(names.contains(&json!("CLAUDE_PID")) && names.contains(&json!("BATON_WRAPPER")));
    }
    // The second start plays the second run, as a resume, and waits for its file.
    let mut second = fake_claude(&dir, &baton_log, &["--resume", SID]).spawn().unwrap();
    until_for(Duration::from_secs(20), || {
        (read_json_lines(&dir.join("steps.jsonl")).len() == 7).then_some(())
    });
    assert_eq!(second.try_wait().unwrap(), None, "it waits for the file");
    std::fs::write(dir.join("go"), "").unwrap();
    assert_eq!(second.wait().unwrap().code(), Some(0));
    let steps = read_json_lines(&dir.join("steps.jsonl"));
    assert_eq!(kinds(&steps[6..]), ["hook", "done"]);
    assert_eq!(steps[7]["run"], 1);
    let starts = read_json_lines(&dir.join("invocations.jsonl"));
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["argv"], json!(["--session-id", SID, "--model", "opus"]));
    assert_eq!(starts[0]["wrapper"], "w1");
    assert_eq!(starts[1]["argv"], json!(["--resume", SID]));
    assert!(starts[0]["configDir"].as_str().unwrap().ends_with("profile"));
    // A third start repeats the last run.
    std::fs::remove_file(dir.join("go")).unwrap();
    let mut third = fake_claude(&dir, &baton_log, &[]).spawn().unwrap();
    until_for(Duration::from_secs(20), || {
        (read_json_lines(&dir.join("steps.jsonl")).len() == 9).then_some(())
    });
    third.kill().unwrap();
    third.wait().unwrap();
}

#[test]
fn the_fake_claude_for_a_whole_instance_is_both_and_runs_until_it_is_ended() {
    let home = temp_home();
    let dir = home.home.join("fake");
    write_json(
        &dir.join("scenario.json"),
        &json!({ "session": SID, "runs": [{ "steps": [{ "wait": "end" }] }] }),
    );
    let both = bin(FAKE_CLAUDE_BOTH);
    let said = Command::new(&both)
        .args(["auth", "status", "--json"])
        .env("FAKE_LOGGED_IN", "0")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&said.stdout).trim(),
        r#"{"loggedIn":false,"authMethod":"none"}"#
    );
    let mut session = Command::new(&both)
        .env("FAKE_CLAUDE_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let waiting = until_for(Duration::from_secs(20), || {
        read_json_lines(&dir.join("steps.jsonl")).into_iter().next()
    });
    assert_eq!(
        (waiting["kind"].as_str(), waiting["session"].as_str()),
        (Some("waiting"), Some(SID))
    );
    assert_eq!(session.try_wait().unwrap(), None);
    session.kill().unwrap();
    session.wait().unwrap();
}

#[test]
fn the_sign_in_stand_in_remembers_a_login_under_a_profile() {
    let home = temp_home();
    let profile = home.home.join("profile");
    let log = home.home.join("logins.jsonl");
    let run = |args: &[&str], profile: Option<&Path>| {
        let mut command = Command::new(bin(FAKE_LOGIN));
        command
            .args(args)
            .env_remove("CLAUDE_CONFIG_DIR")
            .env("FAKE_LOGIN_LOG", &log);
        if let Some(profile) = profile {
            command.env("CLAUDE_CONFIG_DIR", profile);
        }
        let output = command.output().unwrap();
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    };
    let status = ["auth", "status", "--json"];
    let (code, said) = run(&status, Some(&profile));
    assert_eq!(code, Some(1));
    assert_eq!(
        said,
        r#"{"loggedIn":false,"authMethod":"none","email":"someone@example.invalid"}"#
    );
    assert_eq!(run(&status, None).0, Some(0), "plain claude is always signed in");
    let (code, said) = run(&["auth", "login"], Some(&profile));
    assert_eq!(code, Some(0));
    assert_eq!(
        said,
        "https://claude.ai/oauth/authorize?client_id=baton-fixture&state=public-fixture\nPaste the verification code here: "
    );
    assert!(profile.join("fake-signed-in").exists());
    assert_eq!(run(&status, Some(&profile)).0, Some(0));
    assert_eq!(read_json_lines(&log).len(), 1);
    assert_eq!(run(&["something", "else"], None).0, Some(2));
}
