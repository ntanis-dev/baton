//! A stand-in for the `claude` executable in the session wrapper's tests. It logs how it was started,
//! then plays the steps of its run from `$FAKE_CLAUDE_DIR/scenario.json` the way Claude Code would: it
//! runs Baton's real hook and status line commands with hook JSON on stdin, under the
//! `CLAUDE_CONFIG_DIR` / `BATON_WRAPPER` / `BATON_HOME` it was given.
//!
//! The `baton` it runs is the one named by `FAKE_BATON`, else the `baton` program beside this one
//! (where [`crate::fixtures::baton`] builds it).
//!
//! scenario.json: `{ "runs": [ { "steps": [ ... ] }, ... ] }` (run n is the n-th start; the last run
//! repeats). Steps:
//!
//! ```text
//! { "hook": "session-start" | "prompt" | "stop" | "stop-failure" | "session-end", ...fields }
//! { "nested": { "hook": "...", "session_id": "...", ...fields } }
//!                                        a hook of a claude started inside this one
//!                                        (same profile and BATON_WRAPPER, its own pid)
//! { "statusline": { "five_hour": [used, resetsInSeconds], "seven_day": [...] }, "model": "...", "effort": "..." }
//! { "hop": "<account or empty>" }        what /hop runs
//! { "sleep": ms }
//! { "waitFile": "<name in FAKE_CLAUDE_DIR>" }
//! { "exit": code }
//! { "keys": true }                       read the console in raw mode from now on (logs keys-ready)
//! { "wait": "end" }                      run until ended (a kill, or two Ctrl+C keys)
//! ```
//!
//! What it does goes to `steps.jsonl`, each start to `invocations.jsonl`, both in `FAKE_CLAUDE_DIR`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::time::Duration;

use baton_core::clock::now_ms;
use baton_core::fsutil::append_json_line;
use baton_core::json::{Object, is_truthy};
use crossterm::event::{self, Event};
use serde_json::{Value, json};

use crate::raw_reader::key_text;

/// What a run of the stand-in knows.
struct Run {
    /// `FAKE_CLAUDE_DIR`.
    dir: PathBuf,
    /// The arguments it was started with.
    argv: Vec<String>,
    /// How many times it was started before.
    invocations: usize,
    /// The conversation.
    session: String,
    /// The working folder.
    cwd: String,
    /// The pid Claude Code gives its hooks: this process's, or the next one's when the scenario plays
    /// a claude started through a launcher.
    own_pid: u32,
    /// The pid of a claude started inside this one.
    nested_pid: u32,
    /// The `baton` program.
    baton: PathBuf,
}

/// Whether the stand-in is ending.
static ENDING: AtomicBool = AtomicBool::new(false);
/// Whether the console is in raw mode.
static RAW: AtomicBool = AtomicBool::new(false);
/// When the last Ctrl+C was typed.
static LAST_CTRL_C: AtomicI64 = AtomicI64::new(0);

/// What a `baton` command came to.
struct Ran {
    /// Its exit code.
    code: Option<i32>,
    /// What it printed.
    stdout: String,
}

impl Run {
    /// Appends an entry to steps.jsonl.
    fn log(&self, kind: &str, data: Value) {
        let mut entry = Object::new();
        entry.insert("at".to_owned(), json!(now_ms()));
        entry.insert("pid".to_owned(), json!(std::process::id()));
        entry.insert("kind".to_owned(), json!(kind));
        if let Value::Object(fields) = data {
            entry.extend(fields);
        }
        append_json_line(&self.dir.join("steps.jsonl"), &entry).expect("steps.jsonl");
    }

    /// Runs Baton with these arguments, `input` as JSON on stdin, and Claude Code's pid.
    fn baton(&self, args: &[&str], input: Option<&Value>, pid: u32) -> Ran {
        let mut child = Command::new(&self.baton)
            .args(args)
            .env("CLAUDE_PID", pid.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("could not run {}: {error}", self.baton.display()));
        if let Some(mut stdin) = child.stdin.take() {
            let text = input.map(Value::to_string).unwrap_or_default();
            // A command that does not read its input closes the pipe; that is fine.
            let _ = stdin.write_all(text.as_bytes());
        }
        let output = child.wait_with_output().expect("baton's output");
        Ran {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        }
    }

    /// The fields every hook's input has.
    fn base(&self) -> Object {
        let mut base = Object::new();
        base.insert("session_id".to_owned(), json!(self.session));
        base.insert("cwd".to_owned(), json!(self.cwd));
        let transcript = self.dir.join(format!("{}.jsonl", self.session));
        base.insert("transcript_path".to_owned(), json!(transcript.to_string_lossy()));
        base
    }

    /// Ends the stand-in, once.
    fn end(&self, how: &str, code: i32) -> ! {
        if ENDING.swap(true, Ordering::SeqCst) {
            // Another thread is ending it already.
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        self.log("ended", json!({ "how": how, "run": self.invocations }));
        exit(code)
    }
}

/// Leaves, with the console put back.
fn exit(code: i32) -> ! {
    if RAW.swap(false, Ordering::SeqCst) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    std::process::exit(code)
}

/// Reads the console in raw mode from now on: two Ctrl+C within a second and a half end the run.
fn watch_keys(run: &'static Run) {
    if RAW.load(Ordering::SeqCst) {
        return;
    }
    if let Err(error) = crossterm::terminal::enable_raw_mode() {
        run.log(
            "keys-unavailable",
            json!({ "error": error.to_string(), "run": run.invocations }),
        );
        return;
    }
    RAW.store(true, Ordering::SeqCst);
    run.log("keys-ready", json!({ "run": run.invocations }));
    thread::spawn(move || {
        loop {
            let Ok(read) = event::read() else {
                return;
            };
            let Event::Key(key) = read else {
                continue;
            };
            if key_text(&key).as_deref() != Some("\x03") {
                continue;
            }
            let now = now_ms();
            run.log("ctrl-c", json!({ "run": run.invocations }));
            if now - LAST_CTRL_C.swap(now, Ordering::SeqCst) < 1500 {
                run.end("keys", 0);
            }
        }
    });
}

/// A hook step's fields over the base input.
fn hook_input(run: &Run, hook: &str, fields: &Object) -> Object {
    let mut input = run.base();
    input.insert("hook_event_name".to_owned(), json!(hook));
    for (key, value) in fields {
        if key != "hook" {
            input.insert(key.clone(), value.clone());
        }
    }
    input
}

/// Plays one step.
fn play(run: &'static Run, step: &Value) {
    let empty = Object::new();
    let fields = step.as_object().unwrap_or(&empty);
    let truthy = |key: &str| fields.get(key).is_some_and(is_truthy);
    if truthy("hook") {
        let hook = step["hook"].as_str().expect("a hook's name");
        let mut input = hook_input(run, hook, fields);
        if hook == "session-start" && !input.get("source").is_some_and(is_truthy) {
            let source = if run.argv.iter().any(|arg| arg == "--resume") {
                "resume"
            } else {
                "startup"
            };
            input.insert("source".to_owned(), json!(source));
        }
        let ran = run.baton(&["hook", hook], Some(&Value::Object(input)), run.own_pid);
        run.log(
            "hook",
            json!({ "hook": hook, "code": ran.code, "stdout": ran.stdout, "run": run.invocations }),
        );
    } else if truthy("nested") {
        let nested = step["nested"].as_object().expect("a nested hook");
        let hook = nested
            .get("hook")
            .and_then(Value::as_str)
            .expect("a nested hook's name");
        let mut input = run.base();
        input.insert("hook_event_name".to_owned(), json!(hook));
        if hook == "session-start" {
            input.insert("source".to_owned(), json!("startup"));
        }
        for (key, value) in nested {
            if key != "hook" {
                input.insert(key.clone(), value.clone());
            }
        }
        let ran = run.baton(&["hook", hook], Some(&Value::Object(input)), run.nested_pid);
        run.log(
            "nested",
            json!({ "hook": hook, "code": ran.code, "run": run.invocations }),
        );
    } else if truthy("statusline") {
        let now = now_ms() / 1000;
        let mut limits = Object::new();
        for (window, value) in step["statusline"].as_object().expect("the status line's windows") {
            let mut limit = Object::new();
            limit.insert("used_percentage".to_owned(), value[0].clone());
            if let Some(resets_in) = value[1].as_i64() {
                limit.insert("resets_at".to_owned(), json!(now + resets_in));
            }
            limits.insert(window.clone(), Value::Object(limit));
        }
        let mut input = run.base();
        let model = step["model"].as_str().unwrap_or("claude-opus-5-5");
        input.insert("model".to_owned(), json!({ "id": model, "display_name": "Opus" }));
        if truthy("effort") {
            input.insert("effort".to_owned(), json!({ "level": step["effort"] }));
        }
        input.insert("workspace".to_owned(), json!({ "current_dir": run.cwd }));
        input.insert("rate_limits".to_owned(), Value::Object(limits));
        let ran = run.baton(&["statusline"], Some(&Value::Object(input)), run.own_pid);
        run.log(
            "statusline",
            json!({ "code": ran.code, "stdout": ran.stdout, "run": run.invocations }),
        );
    } else if fields.contains_key("hop") {
        let to = format!("--to={}", step["hop"].as_str().unwrap_or_default());
        let ran = run.baton(
            &["hop", "--session", &run.session, "--from-command", &to],
            None,
            run.own_pid,
        );
        run.log(
            "hop",
            json!({ "code": ran.code, "stdout": ran.stdout, "run": run.invocations }),
        );
    } else if truthy("keys") {
        watch_keys(run);
    } else if truthy("sleep") {
        thread::sleep(Duration::from_millis(step["sleep"].as_u64().unwrap_or(0)));
    } else if truthy("waitFile") {
        let file = run.dir.join(step["waitFile"].as_str().expect("a file's name"));
        while !file.exists() {
            thread::sleep(Duration::from_millis(20));
        }
    } else if fields.contains_key("exit") {
        let code = step["exit"].as_i64().unwrap_or(0) as i32;
        run.log("exit", json!({ "code": code, "run": run.invocations }));
        exit(code);
    } else if step["wait"] == "end" {
        run.log(
            "waiting",
            json!({ "run": run.invocations, "session": run.session }),
        );
        // Never outlive a test that failed (well inside the tests' own timeout, so a failure shows as
        // one).
        thread::sleep(Duration::from_secs(25));
        run.end("abandoned", 99);
    }
}

/// The value after the last `name` on the command line.
fn option(argv: &[String], name: &str) -> Option<String> {
    let at = argv.iter().rposition(|arg| arg == name)?;
    argv.get(at + 1).cloned()
}

/// The `baton` program: `FAKE_BATON`, else the one beside this program.
fn baton_program() -> PathBuf {
    if let Some(given) = std::env::var_os("FAKE_BATON").filter(|given| !given.is_empty()) {
        return PathBuf::from(given);
    }
    let exe = std::env::current_exe().expect("this program's own path");
    exe.with_file_name(format!("baton{}", std::env::consts::EXE_SUFFIX))
}

/// How many lines a file has that are not empty.
fn lines_of(file: &Path) -> usize {
    fs::read_to_string(file)
        .map(|text| text.split('\n').filter(|line| !line.is_empty()).count())
        .unwrap_or(0)
}

/// Runs the stand-in with these arguments; returns its exit code.
pub fn run(argv: &[String]) -> i32 {
    let dir = PathBuf::from(std::env::var_os("FAKE_CLAUDE_DIR").expect("FAKE_CLAUDE_DIR"));
    let pid = std::process::id();
    let cwd = std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let started = dir.join("invocations.jsonl");
    let invocations = lines_of(&started);
    let start = json!({
        "argv": argv,
        "configDir": std::env::var("CLAUDE_CONFIG_DIR").ok(),
        "wrapper": std::env::var("BATON_WRAPPER").ok(),
        "cwd": cwd,
        "pid": pid,
    });
    append_json_line(&started, &start).expect("invocations.jsonl");
    let scenario: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("scenario.json")).expect("scenario.json"))
            .expect("scenario.json is JSON");
    let runs = scenario["runs"].as_array().expect("the scenario's runs");
    let steps = runs[invocations.min(runs.len() - 1)]["steps"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let session = option(argv, "--resume")
        .or_else(|| option(argv, "--session-id"))
        .or_else(|| scenario["session"].as_str().map(str::to_owned))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    // Claude Code gives its hooks and status line its own pid (CLAUDE_PID): this process's, or another
    // one's when the scenario plays a claude started through a launcher ("launcher": true) that is the
    // process Baton spawned.
    let run: &'static Run = Box::leak(Box::new(Run {
        dir,
        argv: argv.to_vec(),
        invocations,
        session,
        cwd,
        own_pid: pid + u32::from(is_truthy(&scenario["launcher"])),
        nested_pid: pid + 2,
        baton: baton_program(),
    }));
    for step in &steps {
        play(run, step);
    }
    run.log("done", json!({ "run": run.invocations }));
    // A console that is still being read keeps the stand-in running, as it does Claude Code.
    while RAW.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(50));
    }
    0
}
