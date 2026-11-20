//! A stand-in for the `claude` executable in the usage poller's tests, run the way Baton runs it for a
//! usage read (stream-json on stdin and stdout) or as `claude auth status --json`. It speaks just
//! enough of the protocol: it answers the initialize request, and answers the "/usage" prompt with
//! what Claude Code sends for it (a synthetic assistant message carrying usage_report, then a result).
//!
//! ```text
//! FAKE_USAGE_MODE   ok (default) | hang (never answers) | null (rate_limits null) | ratelimited
//!                   (rate_limits null, "rate limited until 3:14am") | none (no usage_report at all)
//! FAKE_USAGE_JSON   the rate_limits to send (JSON), for ok
//! FAKE_USAGE_LOG    a file to log each start to (argv, config dir, env names)
//! FAKE_USAGE_PID    a file to write this process's pid to
//! FAKE_TRANSCRIPT   1: write a transcript for the working folder, the way a Claude Code that ignored
//!                   --no-session-persistence would
//! FAKE_LOGGED_IN    0: `auth status` says signed out
//! ```

use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use baton_core::clock::{HOUR, iso, now_ms};
use baton_core::fsutil::append_json_line;
use serde_json::{Value, json};

/// An environment variable, when it is set to something.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Writes one message to stdout.
fn out(message: &Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

/// The rate_limits of an ok answer.
fn rate_limits() -> Value {
    if let Some(given) = var("FAKE_USAGE_JSON") {
        return serde_json::from_str(&given).expect("FAKE_USAGE_JSON is JSON");
    }
    json!({
        "limits": [
            {
                "kind": "session",
                "group": "session",
                "percent": 42,
                "resets_at": iso(now_ms() + 2 * HOUR),
                "severity": "normal",
                "is_active": true,
            },
            {
                "kind": "weekly_all",
                "group": "weekly",
                "percent": 63,
                "resets_at": iso(now_ms() + 72 * HOUR),
                "severity": "normal",
                "is_active": false,
            },
        ],
        "extra_usage": null,
    })
}

/// What Claude Code sends for "/usage".
fn answer(mode: &str, cwd: &str) {
    let text = match mode {
        "ratelimited" => "Usage data is rate limited until 3:14am. Showing cached data.",
        "null" => "Could not load your plan usage.",
        _ => "Current session 42% used",
    };
    let mut message = json!({
        "type": "assistant",
        "session_id": "probe",
        "uuid": "u1",
        "parent_tool_use_id": null,
        "message": {
            "id": "m1",
            "type": "message",
            "role": "assistant",
            "model": "<synthetic>",
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
            "usage": {},
        },
    });
    if mode != "none" {
        message["usage_report"] = json!({
            "session": {},
            "rate_limits": if mode == "ok" { rate_limits() } else { Value::Null },
        });
    }
    out(&json!({
        "type": "system",
        "subtype": "init",
        "session_id": "probe",
        "uuid": "u0",
        "tools": [],
        "mcp_servers": [],
        "model": "x",
        "permissionMode": "default",
        "slash_commands": [],
        "apiKeySource": "none",
        "cwd": cwd,
        "output_style": "default",
    }));
    out(&message);
    out(&json!({
        "type": "result",
        "subtype": "success",
        "session_id": "probe",
        "uuid": "u2",
        "is_error": false,
        "result": text,
        "duration_ms": 1,
        "duration_api_ms": 0,
        "num_turns": 0,
        "total_cost_usd": 0,
        "usage": {},
        "modelUsage": {},
        "permission_denials": [],
    }));
}

/// Whether a variable's name starts with CLAUDE, BATON or ANTHROPIC, in any case.
fn is_ours(name: &str) -> bool {
    let upper = name.to_uppercase();
    ["CLAUDE", "BATON", "ANTHROPIC"]
        .iter()
        .any(|prefix| upper.starts_with(prefix))
}

/// Runs the stand-in with these arguments; returns its exit code.
pub fn run(argv: &[String]) -> i32 {
    let signed_in = std::env::var("FAKE_LOGGED_IN").ok().as_deref() != Some("0");
    if argv.first().map(String::as_str) == Some("auth") && argv.get(1).map(String::as_str) == Some("status") {
        let said = json!({
            "loggedIn": signed_in,
            "authMethod": if signed_in { "claude.ai" } else { "none" },
        });
        println!("{said}");
        return 0;
    }
    let pid = std::process::id();
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let config_dir = var("CLAUDE_CONFIG_DIR");
    if let Some(file) = var("FAKE_USAGE_PID") {
        fs::write(file, pid.to_string()).expect("the pid file");
    }
    if let Some(file) = var("FAKE_USAGE_LOG") {
        let names: Vec<String> = std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| is_ours(name))
            .collect();
        let start =
            json!({ "argv": argv, "configDir": config_dir, "cwd": cwd, "envNames": names, "pid": pid });
        append_json_line(&PathBuf::from(file), &start).expect("the log");
    }
    if var("FAKE_TRANSCRIPT").as_deref() == Some("1") {
        let config = match &config_dir {
            Some(dir) => PathBuf::from(dir),
            None => std::env::home_dir().expect("a home folder").join(".claude"),
        };
        let name: String = cwd
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let dir = config.join("projects").join(name);
        fs::create_dir_all(&dir).expect("the transcript folder");
        fs::write(dir.join("probe-session.jsonl"), "{\"type\":\"user\"}\n").expect("the transcript");
    }
    let mode = var("FAKE_USAGE_MODE").unwrap_or_else(|| "ok".to_owned());
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if mode == "hang" {
            continue;
        }
        match message["type"].as_str() {
            Some("control_request") => out(&json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": message["request_id"],
                    "response": { "commands": [], "models": [], "account": {} },
                },
            })),
            Some("user") => answer(&mode, &cwd),
            _ => {}
        }
    }
    if mode == "hang" {
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    0
}
