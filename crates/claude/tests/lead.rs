//! The session wrapper against a stand-in for claude that plays a scenario and runs Baton's real hook,
//! status line and hop commands: hops, stays, waits, re-blocks, and whose conversation it follows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use baton_claude::countdown::channel_keys;
use baton_claude::lead::{Lead, LeadOptions, LeadStdio, Signals};
use baton_claude::terminate::{EndOptions, end_claude};
use baton_core::clock::{Ms, iso, now_ms};
use baton_core::config::EndMethod;
use baton_core::format::poll_line;
use baton_core::fsutil::{is_alive, read_json, read_json_lines};
use baton_core::json::Get;
use baton_core::paths::{Paths, profile_dir};
use baton_core::poll::poll_holder;
use baton_core::sessions::list_wrappers;
use baton_core::state::{Want, load_snapshot};
use baton_core::usage::history_file;
use baton_testkit::fixtures::{FAKE_CLAUDE, FAKE_CLAUDE_BOTH, baton, bin, launcher};
use baton_testkit::{TestHome, accounts_home, temp_home, test_env_with, until, until_for, write, write_json};
use serde_json::{Value, json};

const SID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";
const NESTED: &str = "0d9c8b7a-6f5e-4d3c-2b1a-0f9e8d7c6b5a";
const CLEARED: &str = "1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";
const H: i64 = 3600;

/// One wrapper test at a time, as the suite has always run: they share the console some of them type
/// into, and their timings (a window that resets in five seconds) are real.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the wrapper wrote, shared with the test while it runs.
#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Output {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The interrupts the wrapper ignores at the moment.
#[derive(Clone, Default)]
struct Ignored(Arc<Mutex<Vec<&'static str>>>);

impl Ignored {
    fn names(&self) -> Vec<&'static str> {
        self.0.lock().unwrap().clone()
    }
}

impl Signals for Ignored {
    fn ignore(&mut self, names: &[&'static str]) {
        self.0.lock().unwrap().extend_from_slice(names);
    }

    fn restore(&mut self, names: &[&'static str]) {
        self.0.lock().unwrap().retain(|name| !names.contains(name));
    }
}

/// What a test starts a wrapper with.
struct Setup {
    accounts: Vec<&'static str>,
    policy: Value,
    runs: Value,
    args: Vec<&'static str>,
    readings: Vec<(&'static str, Reading)>,
    countdown: f64,
    method: EndMethod,
    launcher: bool,
}

impl Setup {
    fn new(runs: Value) -> Setup {
        Setup {
            accounts: vec!["acct1", "acct2"],
            policy: json!({}),
            runs,
            args: Vec::new(),
            readings: Vec::new(),
            countdown: 0.3,
            method: EndMethod::Kill,
            launcher: false,
        }
    }
}

/// A reading of both windows: `[used, resets in seconds]`.
#[derive(Clone, Copy)]
struct Reading {
    five: (f64, i64),
    week: (f64, i64),
}

fn write_reading(paths: &Paths, name: &str, reading: Reading, now: Ms) {
    let window = |(used, resets_in): (f64, i64)| json!({ "used": used, "resetsAt": iso(now + resets_in * 1000), "readAt": iso(now) });
    write_json(
        &paths.usage.join(format!("{name}.json")),
        &json!({
            "account": name,
            "at": iso(now),
            "five_hour": window(reading.five),
            "seven_day": window(reading.week),
        }),
    );
}

/// A wrapper running on a thread of its own, with the fake claude.
struct Run {
    done: Option<JoinHandle<(i32, Lead)>>,
    dir: PathBuf,
    home: TestHome,
    keys: Sender<Vec<u8>>,
    output: Output,
    signals: Ignored,
}

impl Run {
    fn paths(&self) -> &Paths {
        &self.home.paths
    }

    /// Waits for the wrapper to end: its exit code, and the wrapper itself.
    fn finish(&mut self) -> (i32, Lead) {
        self.done.take().unwrap().join().unwrap()
    }

    fn invocations(&self) -> Vec<Value> {
        read_json_lines(&self.dir.join("invocations.jsonl"))
    }

    fn steps(&self) -> Vec<Value> {
        read_json_lines(&self.dir.join("steps.jsonl"))
    }

    fn has_step(&self, kind: &str) -> bool {
        self.steps().iter().any(|step| step.text("kind") == Some(kind))
    }

    fn hops(&self) -> Vec<Value> {
        read_json_lines(&history_file(self.paths(), "events", now_ms()))
            .into_iter()
            .filter(|event| event.text("type") == Some("hop"))
            .collect()
    }

    fn profile(&self, account: &str) -> String {
        profile_dir(self.paths(), account).to_string_lossy().into_owned()
    }

    fn type_keys(&self, keys: &str) {
        self.keys.send(keys.as_bytes().to_vec()).unwrap();
    }

    fn wait_for_output(&self, text: &str) {
        until(|| self.output.text().contains(text).then_some(()));
    }
}

/// The options every test wrapper shares: the fake claude, fast timings, nothing on the terminal.
fn lead_options(
    home: &TestHome,
    dir: &Path,
    args: &[&str],
    method: EndMethod,
    countdown: f64,
) -> (LeadOptions, Sender<Vec<u8>>, Output, Ignored) {
    let env = test_env_with(
        &home.home,
        &[
            ("FAKE_CLAUDE_DIR", &dir.to_string_lossy()),
            ("FAKE_BATON", &baton().to_string_lossy()),
        ],
    );
    let (keys, input) = channel_keys();
    let output = Output::default();
    let signals = Ignored::default();
    let mut options = LeadOptions::new(
        args.iter().map(|arg| (*arg).to_owned()).collect(),
        home.paths.clone(),
        env,
    );
    options.poll = false;
    options.cwd = home.home.parent().unwrap().to_path_buf();
    options.claude = Some(launcher(FAKE_CLAUDE));
    options.end = Some(Box::new(move |child| {
        end_claude(
            child,
            &EndOptions {
                method,
                ..EndOptions::default()
            },
        )
    }));
    options.input = Box::new(input);
    options.output = Box::new(output.clone());
    options.output_tty = false;
    options.output_columns = None;
    options.signals = Box::new(signals.clone());
    options.poll_ms = 40;
    options.tick_ms = 40;
    options.countdown_seconds = Some(countdown);
    options.settle_ms = Some(50);
    options.stdio = LeadStdio::Null;
    (options, keys, output, signals)
}

/// Starts a wrapper with the fake claude.
fn lead(setup: Setup) -> Run {
    // Built before any reading is written: the first use builds the programs, and a reading that
    // resets in a few seconds must not grow old meanwhile.
    baton();
    bin(FAKE_CLAUDE);
    let home = temp_home();
    let dir = home.home.parent().unwrap().join("fake");
    write_json(
        &dir.join("scenario.json"),
        &json!({ "session": SID, "runs": setup.runs, "launcher": setup.launcher }),
    );
    let accounts: Vec<Value> = setup
        .accounts
        .iter()
        .map(|name| json!({ "name": name, "mark": "normal" }))
        .collect();
    let mut policy = json!({ "minStayMinutes": 0 });
    for (key, value) in setup.policy.as_object().unwrap() {
        policy[key] = value.clone();
    }
    write_json(
        &home.paths.config,
        &json!({ "version": 1, "accounts": accounts, "policy": policy }),
    );
    let now = now_ms();
    for (name, reading) in &setup.readings {
        write_reading(&home.paths, name, *reading, now);
    }
    let (options, keys, output, signals) =
        lead_options(&home, &dir, &setup.args, setup.method, setup.countdown);
    let done = thread::spawn(move || {
        let mut lead = Lead::new(options);
        let code = lead.run();
        (code, lead)
    });
    Run {
        done: Some(done),
        dir,
        home,
        keys,
        output,
        signals,
    }
}

/// A turn that is refused: the 5-hour window is full and resets in `resets_in` seconds.
///
/// The turn takes a moment, as a turn does: the wrapper learns the session's permission mode from the
/// running turn, one look every `poll_ms`, and this stand-in runs its hooks faster than that.
fn limited(resets_in: i64) -> Vec<Value> {
    vec![
        json!({ "hook": "session-start" }),
        json!({ "statusline": { "five_hour": [100, resets_in], "seven_day": [40, 72 * H] } }),
        json!({ "hook": "prompt", "permission_mode": "default" }),
        json!({ "sleep": 150 }),
        json!({
            "hook": "stop-failure",
            "error": "rate_limit",
            "last_assistant_message": "API Error: Rate limit reached",
        }),
    ]
}

/// Steps, then more steps.
fn steps(parts: &[Vec<Value>]) -> Value {
    json!({ "steps": parts.concat() })
}

fn wait_end() -> Vec<Value> {
    vec![json!({ "wait": "end" })]
}

fn start_then_exit() -> Value {
    json!({ "steps": [{ "hook": "session-start" }, { "exit": 0 }] })
}

fn argv(invocation: &Value) -> Vec<String> {
    invocation["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap().to_owned())
        .collect()
}

fn prompt_of(invocation: &Value) -> String {
    argv(invocation).pop().unwrap()
}

/// The decisions of the turn ends, by a field of theirs.
fn decisions(lead: &Lead, field: &str) -> Vec<String> {
    lead.events()
        .iter()
        .filter(|event| event.text("message") == Some("turn end"))
        .map(|event| event["decision"][field].as_str().unwrap().to_owned())
        .collect()
}

fn has_event(lead: &Lead, message: &str) -> bool {
    lead.events()
        .iter()
        .any(|event| event.text("message") == Some(message))
}

fn hop_fields(run: &Run, fields: &[&str]) -> Vec<Vec<String>> {
    run.hops()
        .iter()
        .map(|hop| {
            fields
                .iter()
                .map(|field| hop[*field].as_str().unwrap().to_owned())
                .collect()
        })
        .collect()
}

#[test]
fn automatic_hop_a_refusal_ends_claude_at_the_turn_end_and_resumes_the_conversation_on_the_next_account() {
    let _serial = serial();
    let first_args = [
        "--model",
        "opus",
        "--add-dir",
        "extra",
        "--permission-mode",
        "acceptEdits",
        "first prompt",
    ];
    let mut run = lead(Setup {
        args: first_args.to_vec(),
        ..Setup::new(json!([steps(&[limited(2 * H), wait_end()]), start_then_exit()]))
    });
    assert_eq!(run.finish().0, 0);
    let invocations = run.invocations();
    let (first, second) = (&invocations[0], &invocations[1]);
    assert_eq!(
        first["configDir"],
        json!(run.profile("acct1")),
        "every account runs under its own profile"
    );
    assert_eq!(argv(first), first_args);
    assert_eq!(second["configDir"], json!(run.profile("acct2")));
    assert_eq!(second["wrapper"], first["wrapper"]);
    // Resume keeps the options, takes the live model and permission mode, drops the first prompt.
    let resumed = argv(second);
    assert_eq!(
        resumed[..resumed.len() - 1],
        [
            "--model",
            "claude-opus-5-5",
            "--add-dir",
            "extra",
            "--permission-mode",
            "manual",
            "--resume",
            SID
        ]
    );
    let prompt = prompt_of(second);
    assert!(
        prompt.contains("moved from account acct1 to account acct2 because acct1 reached its 5-hour limit"),
        "{prompt}"
    );
    assert!(prompt.contains("resumeFromRunId"));
    let output = run.output.text();
    assert!(
        output.contains("moving this conversation to acct2 in 1 s"),
        "{output}"
    );
    assert!(
        output.contains(&format!("baton: resuming {SID} on acct2")),
        "{output}"
    );
    assert_eq!(
        hop_fields(&run, &["from", "to", "reason"]),
        [["acct1", "acct2", "refusal"]]
    );
}

#[test]
fn stay_a_key_during_the_countdown_keeps_the_conversation_on_its_account_and_lets_claude_code_wait() {
    let _serial = serial();
    let again = json!({ "steps": [
        { "hook": "session-start" },
        { "hook": "prompt" },
        { "hook": "stop-failure", "error": "rate_limit" },
        { "sleep": 300 },
        { "exit": 0 },
    ] });
    let mut run = lead(Setup {
        countdown: 8.0,
        ..Setup::new(json!([steps(&[limited(2 * H), wait_end()]), again]))
    });
    run.wait_for_output("moving this conversation");
    thread::sleep(Duration::from_millis(650)); // past the grace period
    run.type_keys("x");
    assert_eq!(run.finish().0, 0);
    let invocations = run.invocations();
    assert_eq!(invocations.len(), 2);
    assert_eq!(
        invocations[1]["configDir"],
        json!(run.profile("acct1")),
        "stayed on acct1"
    );
    assert!(prompt_of(&invocations[1]).contains("chose to stay on it"));
    assert!(run.output.text().contains("staying on acct1"));
    assert!(run.hops().is_empty());
}

#[test]
fn enter_during_the_countdown_hops_at_once_ctrl_c_quits_with_the_resume_command() {
    let _serial = serial();
    let exit = json!({ "steps": [{ "exit": 0 }] });
    let mut go = lead(Setup {
        countdown: 30.0,
        ..Setup::new(json!([steps(&[limited(2 * H), wait_end()]), exit]))
    });
    go.wait_for_output("moving this conversation");
    thread::sleep(Duration::from_millis(650));
    go.type_keys("\r");
    assert_eq!(go.finish().0, 0);
    assert_eq!(go.invocations()[1]["configDir"], json!(go.profile("acct2")));
    let mut quit = lead(Setup {
        countdown: 30.0,
        ..Setup::new(json!([steps(&[limited(2 * H), wait_end()])]))
    });
    quit.wait_for_output("moving this conversation");
    thread::sleep(Duration::from_millis(650));
    quit.type_keys("\x03");
    assert_eq!(quit.finish().0, 130);
    let output = quit.output.text();
    assert!(output.contains(&format!("baton --resume {SID}")), "{output}");
    assert_eq!(quit.invocations().len(), 1);
}

#[test]
fn no_other_account_available_claude_keeps_running_and_claude_codes_own_wait_applies() {
    let _serial = serial();
    let tail = vec![json!({ "sleep": 700 }), json!({ "exit": 0 })];
    let mut run = lead(Setup {
        readings: vec![(
            "acct2",
            Reading {
                five: (100.0, 3 * H),
                week: (10.0, 72 * H),
            },
        )],
        ..Setup::new(json!([steps(&[limited(H), tail])]))
    });
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    assert_eq!(run.invocations().len(), 1);
    assert_eq!(decisions(&lead, "type"), ["wait-in-place"]);
    assert_eq!(decisions(&lead, "reason"), ["all-blocked-current-first"]);
}

#[test]
fn normal_exit_baton_exits_with_claudes_code() {
    let _serial = serial();
    let mut run = lead(Setup::new(json!([{ "steps": [
        { "hook": "session-start" },
        { "hook": "prompt" },
        { "hook": "stop" },
        { "exit": 7 },
    ] }])));
    let (code, lead) = run.finish();
    assert_eq!(code, 7);
    assert_eq!(run.invocations().len(), 1);
    // What the wrapper leaves behind: nothing of its own but the log.
    assert!(!run.paths().sessions.join(SID).join("lock.json").exists());
    assert_eq!(
        fs::read_dir(&run.paths().wrappers)
            .map(|entries| entries.count())
            .unwrap_or(0),
        0
    );
    let log = run.paths().logs.join(format!("lead-{}.log", lead.wrapper()));
    let logged = read_json_lines(&log);
    assert_eq!(logged, lead.events());
    assert_eq!(logged[0]["message"], "launch");
    assert_eq!(logged[0]["account"], "acct1");
    assert_eq!(logged[0]["reason"], "start");
    let last = logged.last().unwrap();
    assert_eq!(
        (last["message"].as_str(), last["exit"]["code"].as_i64()),
        (Some("claude exited"), Some(7))
    );
}

#[test]
fn manual_hop_with_slash_hop_the_request_waits_for_the_end_of_the_turn() {
    let _serial = serial();
    let mut run = lead(Setup::new(json!([
        { "steps": [
            { "hook": "session-start" },
            // /hop only works once the wrapper holds the conversation, which takes it one look at
            // the session's files; this stand-in runs its hooks faster than a claude ever would.
            { "sleep": 200 },
            { "hook": "prompt", "prompt": "/hop" },
            { "hop": "" },
            { "sleep": 300 },
            { "hook": "stop" },
            { "wait": "end" },
        ] },
        start_then_exit(),
    ])));
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    let steps = run.steps();
    let hop = steps
        .iter()
        .find(|step| step.text("kind") == Some("hop"))
        .unwrap();
    assert_eq!(hop["code"], 0);
    let said = hop["stdout"].as_str().unwrap();
    assert!(
        said.contains("moves from acct1 to acct2 when this turn ends"),
        "{said}"
    );
    // The request came in during the turn; the hop was decided on its Stop.
    let ends: Vec<&Value> = lead
        .events()
        .iter()
        .filter(|event| event.text("message") == Some("turn end"))
        .collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(
        (ends[0]["event"].as_str(), ends[0]["decision"]["reason"].as_str()),
        (Some("stop"), Some("manual"))
    );
    assert!(prompt_of(&run.invocations()[1]).contains("the operator asked for it"));
    assert_eq!(hop_fields(&run, &["reason"]), [["manual"]]);
}

/// A turn on an account that fails in a way Baton cannot see (a sign-in that is refused), during
/// which the operator asks for a hop.
fn failing_then_hop() -> Value {
    json!({ "steps": [
        { "hook": "session-start" },
        { "sleep": 200 },
        { "hook": "prompt", "prompt": "/hop" },
        { "hop": "" },
        { "sleep": 300 },
        { "hook": "stop-failure", "error": "oauth_org_not_allowed" },
        { "wait": "end" },
    ] })
}

#[test]
fn hops_in_a_row_go_on_to_the_next_account_not_back_to_the_one_just_left() {
    let _serial = serial();
    // Neither acct1 nor acct2 works, and nothing Baton reads says so: without the run of hops the
    // conversation would go acct1, acct2, acct1 and never reach acct3.
    let mut run = lead(Setup {
        accounts: vec!["acct1", "acct2", "acct3"],
        ..Setup::new(json!([failing_then_hop(), failing_then_hop(), start_then_exit()]))
    });
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    assert_eq!(
        hop_fields(&run, &["from", "to", "reason"]),
        [["acct1", "acct2", "manual"], ["acct2", "acct3", "manual"]]
    );
    assert_eq!(run.invocations()[2]["configDir"], json!(run.profile("acct3")));
    // `baton hop` said where the conversation would go, both times.
    let said: Vec<String> = run
        .steps()
        .iter()
        .filter(|step| step.text("kind") == Some("hop"))
        .map(|step| step["stdout"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(said.len(), 2);
    assert!(said[0].contains("moves from acct1 to acct2"), "{}", said[0]);
    assert!(said[1].contains("moves from acct2 to acct3"), "{}", said[1]);
    let left: Vec<&Value> = lead
        .events()
        .iter()
        .filter(|event| event.text("message") == Some("turn end"))
        .map(|event| &event["left"])
        .collect();
    assert_eq!(left, [&json!([]), &json!(["acct1"])]);
}

#[test]
fn a_conversation_that_settled_may_go_back_to_the_account_it_came_from() {
    let _serial = serial();
    // The turn on acct2 ends well and the minimum stay (none here) is over: the run of hops is
    // over, and acct1 is the best other account again.
    let mut run = lead(Setup {
        accounts: vec!["acct1", "acct2", "acct3"],
        ..Setup::new(json!([
            failing_then_hop(),
            { "steps": [
                { "hook": "session-start" },
                { "hook": "prompt" },
                { "hook": "stop" },
                { "sleep": 300 },
                { "hook": "prompt", "prompt": "/hop" },
                { "hop": "" },
                { "sleep": 300 },
                { "hook": "stop" },
                { "wait": "end" },
            ] },
            start_then_exit(),
        ]))
    });
    assert_eq!(run.finish().0, 0);
    assert_eq!(
        hop_fields(&run, &["from", "to"]),
        [["acct1", "acct2"], ["acct2", "acct1"]]
    );
}

/// Runs the real `baton` in a home, the way another terminal would.
fn baton_in(home: &TestHome, args: &[&str]) -> (Option<i32>, String, String) {
    let mut command = Command::new(baton());
    command.args(args);
    test_env_with(&home.home, &[]).apply(&mut command);
    let output = command.output().unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn manual_hop_from_another_terminal_while_the_conversation_is_between_turns_to_a_named_account() {
    let _serial = serial();
    let mut run = lead(Setup {
        accounts: vec!["acct1", "acct2", "acct3"],
        ..Setup::new(json!([
            { "steps": [
                { "hook": "session-start" },
                { "hook": "prompt" },
                { "hook": "stop" },
                { "wait": "end" },
            ] },
            { "steps": [{ "exit": 0 }] },
        ]))
    });
    until(|| run.has_step("waiting").then_some(()));
    // `baton hop` finds the conversation through the wrapper's record, which names it once the
    // wrapper has taken it up: one look at the session's files, and this stand-in gets to its wait
    // faster than that on a fast machine.
    until(|| {
        list_wrappers(run.paths())
            .iter()
            .any(|wrapper| wrapper.text("session") == Some(SID))
            .then_some(())
    });
    let (code, out, err) = baton_in(&run.home, &["hop", "--to", "acct3"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("moves from acct1 to acct3 now"), "{out}");
    assert_eq!(run.finish().0, 0);
    assert_eq!(run.invocations()[1]["configDir"], json!(run.profile("acct3")));
    let (code, out, _) = baton_in(&run.home, &["hop"]);
    assert_eq!(code, Some(1));
    assert!(
        out.contains("only work in sessions started with baton, and none is running"),
        "{out}"
    );
}

#[test]
fn ctrl_c_belongs_to_claude_baton_ignores_the_interrupts_and_does_not_end_or_signal_claude() {
    let _serial = serial();
    let mut run = lead(Setup::new(json!([{ "steps": [
        { "hook": "session-start" },
        { "waitFile": "release" },
        { "exit": 0 },
    ] }])));
    until(|| run.has_step("hook").then_some(()));
    let ignored = run.signals.names();
    assert!(ignored.contains(&"SIGINT"), "baton handles the interrupt itself");
    assert!(ignored.contains(&if cfg!(windows) { "SIGBREAK" } else { "SIGQUIT" }));
    thread::sleep(Duration::from_millis(200));
    write(&run.dir.join("release"), "");
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    assert!(!has_event(&lead, "ending claude"));
    assert!(run.has_step("exit"), "claude ran to its own end");
    assert!(run.signals.names().is_empty(), "handlers removed after");
}

/// Whether this process has a Windows console the tests can type into.
#[cfg(windows)]
fn console_available() -> bool {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(r"\\.\CONIN$")
        .is_ok()
}

#[cfg(windows)]
#[test]
fn ctrl_c_typed_on_the_windows_console_reaches_claude_as_a_key_not_baton() {
    let _serial = serial();
    use baton_claude::terminate::{KeyTiming, type_console_keys};

    if !console_available() {
        eprintln!("skipped: needs a Windows console");
        return;
    }
    let mut run = lead(Setup::new(json!([{ "steps": [
        { "hook": "session-start" },
        { "keys": true },
        { "waitFile": "release" },
        { "exit": 0 },
    ] }])));
    until(|| (run.has_step("keys-ready") || run.has_step("keys-unavailable")).then_some(()));
    if run.has_step("keys-unavailable") {
        write(&run.dir.join("release"), "");
        run.finish();
        eprintln!("skipped: the console cannot be read here");
        return;
    }
    assert_eq!(
        type_console_keys(std::process::id(), &[3], KeyTiming::default()),
        "ok"
    );
    until(|| run.has_step("ctrl-c").then_some(()));
    write(&run.dir.join("release"), "");
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    assert!(!has_event(&lead, "ending claude"));
}

#[cfg(windows)]
#[test]
fn on_windows_claude_is_ended_by_typing_two_ctrl_c_into_the_console_then_resumed() {
    let _serial = serial();
    if !console_available() {
        eprintln!("skipped: needs a Windows console");
        return;
    }
    let mut first = vec![json!({ "hook": "session-start" }), json!({ "keys": true })];
    first.extend(limited(2 * H).into_iter().skip(1));
    let mut run = lead(Setup {
        method: EndMethod::Keys,
        ..Setup::new(json!([steps(&[first, wait_end()]), { "steps": [{ "exit": 0 }] }]))
    });
    let (code, lead) = run.finish();
    let steps = run.steps();
    let ended = steps
        .iter()
        .find(|step| matches!(step.text("kind"), Some("ended" | "keys-unavailable")))
        .unwrap();
    if ended.text("kind") == Some("keys-unavailable") {
        eprintln!("skipped: the console cannot be read here");
        return;
    }
    assert_eq!(code, 0);
    assert_eq!(ended["how"], "keys");
    let result = &lead
        .events()
        .iter()
        .find(|event| event.text("message") == Some("claude ended"))
        .unwrap()["result"];
    assert_eq!(
        (&result["method"], &result["forced"], &result["rounds"]),
        (&json!("keys"), &json!(false), &json!(1))
    );
    assert_eq!(run.invocations().len(), 2);
}

#[test]
fn planned_hop_when_the_next_turn_would_not_fit_the_conversation_moves_between_turns() {
    let _serial = serial();
    // Over a second per turn: the fake's resets_at (now + 3 h, in whole seconds) moves between
    // readings, as a real one can; the turns still belong to one window period. (This was the flake:
    // with exact comparison a slow run measured no cost and never hopped.)
    let turn = |used: i64| {
        vec![
            json!({ "statusline": { "five_hour": [used, 3 * H], "seven_day": [10, 72 * H] } }),
            json!({ "hook": "prompt" }),
            json!({ "hook": "stop" }),
            json!({ "sleep": 1100 }),
        ]
    };
    let start = vec![json!({ "hook": "session-start" })];
    let mut run = lead(Setup::new(json!([
        steps(&[start, turn(50), turn(62), turn(76), wait_end()]),
        { "steps": [{ "exit": 0 }] },
    ])));
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    // 50: no cost yet; 62: cost 12, headroom 28 >= 18; 76: cost 13, headroom 14 < 19.5
    assert_eq!(decisions(&lead, "reason"), ["stay", "stay", "planned"]);
    let prompt = prompt_of(&run.invocations()[1]);
    assert!(prompt.contains("was about to reach its 5-hour limit"), "{prompt}");
    assert_eq!(hop_fields(&run, &["reason"]), [["planned"]]);
}

#[test]
fn planned_hop_suppressed_while_subagents_run_only_a_refusal_moves_the_session_then() {
    let _serial = serial();
    let turn = |used: i64, tasks: Value| {
        vec![
            json!({ "statusline": { "five_hour": [used, 3 * H], "seven_day": [10, 72 * H] } }),
            json!({ "hook": "prompt" }),
            json!({ "hook": "stop", "background_tasks": tasks }),
            json!({ "sleep": 250 }),
        ]
    };
    let busy = json!([{ "id": "a1", "type": "subagent", "status": "running", "agent_type": "Explore" }]);
    let start = vec![json!({ "hook": "session-start" })];
    let tail = vec![json!({ "sleep": 400 }), json!({ "exit": 0 })];
    let mut run = lead(Setup::new(json!([steps(&[
        start,
        turn(50, json!([])),
        turn(62, json!([])),
        turn(76, busy),
        tail
    ])])));
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    assert_eq!(decisions(&lead, "reason"), ["stay", "stay", "busy"]);
    assert_eq!(run.invocations().len(), 1);
}

#[test]
fn all_blocked_claude_closes_baton_counts_down_to_the_first_account_back_and_resumes_there() {
    let _serial = serial();
    let mut run = lead(Setup {
        readings: vec![(
            "acct2",
            Reading {
                five: (100.0, 6),
                week: (10.0, 72 * H),
            },
        )],
        ..Setup::new(json!([steps(&[limited(2 * H), wait_end()]), start_then_exit()]))
    });
    run.wait_for_output("first back");
    let output = run.output.text();
    assert!(
        output.contains("all 2 accounts at limit; acct2 is first back, its 5-hour window resets at"),
        "{output}"
    );
    assert_eq!(run.finish().0, 0);
    let second = &run.invocations()[1];
    assert_eq!(second["configDir"], json!(run.profile("acct2")));
    let prompt = prompt_of(second);
    assert!(
        prompt.contains("every account was at its limit; Baton waited"),
        "{prompt}"
    );
    assert_eq!(hop_fields(&run, &["reason"]), [["all-blocked-wait"]]);
}

#[test]
fn all_blocked_a_reading_that_changes_during_the_wait_an_earlier_reset_resumes_at_once() {
    let _serial = serial();
    let mut run = lead(Setup {
        accounts: vec!["acct1", "acct2", "acct3"],
        readings: vec![
            (
                "acct2",
                Reading {
                    five: (100.0, 600),
                    week: (10.0, 72 * H),
                },
            ),
            (
                "acct3",
                Reading {
                    five: (100.0, 900),
                    week: (10.0, 72 * H),
                },
            ),
        ],
        ..Setup::new(json!([steps(&[limited(2 * H), wait_end()]), { "steps": [{ "exit": 0 }] }]))
    });
    run.wait_for_output("first back");
    assert!(run.output.text().contains("acct2 is first back"));
    // acct3 reports a fresh window (another session's status line).
    write_reading(
        run.paths(),
        "acct3",
        Reading {
            five: (3.0, 5 * H),
            week: (10.0, 72 * H),
        },
        now_ms(),
    );
    let started = Instant::now();
    assert_eq!(run.finish().0, 0);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(run.invocations()[1]["configDir"], json!(run.profile("acct3")));
}

#[test]
fn re_block_after_resuming_a_new_refusal_re_picks_repeated_ones_back_off_with_a_clear_message() {
    let _serial = serial();
    // acct1 is refused; its 5-hour window resets in 9 s, acct2's in 5 s (seconds of margin: the
    // decision must come before either reset, also on a loaded machine).
    // On acct2: refused again right away (its reset did not happen after all).
    let refused_again = json!({ "steps": [
        { "hook": "session-start" },
        { "statusline": { "five_hour": [100, 2 * H], "seven_day": [10, 72 * H] } },
        { "hook": "prompt" },
        { "hook": "stop-failure", "error": "rate_limit" },
        { "wait": "end" },
    ] });
    let mut run = lead(Setup {
        readings: vec![(
            "acct2",
            Reading {
                five: (100.0, 5),
                week: (10.0, 72 * H),
            },
        )],
        policy: json!({ "maxReblocks": 1, "reblockBackoffMinutes": 0.03, "waitInPlaceMinutes": 0 }),
        ..Setup::new(json!([
            steps(&[limited(9), wait_end()]),
            refused_again,
            start_then_exit()
        ]))
    });
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    let dirs: Vec<Value> = run
        .invocations()
        .iter()
        .map(|invocation| invocation["configDir"].clone())
        .collect();
    assert_eq!(
        dirs,
        [
            json!(run.profile("acct1")),
            json!(run.profile("acct2")),
            json!(run.profile("acct1"))
        ]
    );
    assert_eq!(decisions(&lead, "reason"), ["all-blocked-wait", "reblocked"]);
    let output = run.output.text();
    assert!(output.contains("refused again right after resuming"), "{output}");
    assert_eq!(
        hop_fields(&run, &["to", "reason"]),
        [["acct2", "all-blocked-wait"], ["acct1", "reblocked"]]
    );
}

#[test]
fn a_conversation_already_running_under_another_baton_is_refused() {
    let _serial = serial();
    let mut first = lead(Setup {
        args: vec!["--resume", SID],
        ..Setup::new(json!([{ "steps": [
            { "hook": "session-start" },
            { "waitFile": "release" },
            { "exit": 0 },
        ] }]))
    });
    until(|| first.has_step("hook").then_some(()));
    let (options, _keys, output, _signals) =
        lead_options(&first.home, &first.dir, &["--resume", SID], EndMethod::Kill, 0.3);
    assert_eq!(Lead::new(options).run(), 1);
    let said = output.text();
    assert!(
        said.contains(&format!(
            "baton: conversation {SID} is already running under baton (pid {}); one conversation runs in one process at a time.",
            std::process::id()
        )),
        "{said}"
    );
    write(&first.dir.join("release"), "");
    assert_eq!(first.finish().0, 0);
}

#[test]
fn passthrough_commands_run_under_the_picked_account_without_the_session_machinery() {
    let _serial = serial();
    let mut run = lead(Setup {
        readings: vec![(
            "acct1",
            Reading {
                five: (95.0, H),
                week: (10.0, 72 * H),
            },
        )],
        args: vec!["-p", "hello"],
        ..Setup::new(json!([{ "steps": [{ "exit": 3 }] }]))
    });
    let (code, lead) = run.finish();
    assert_eq!(code, 3);
    assert_eq!(
        run.invocations()[0]["configDir"],
        json!(run.profile("acct2")),
        "acct1 is blocked: acct2"
    );
    assert_eq!(argv(&run.invocations()[0]), ["-p", "hello"]);
    assert!(lead.events().is_empty() && lead.session_id().is_none());
}

/// What `claude -p` run inside the conversation does: the same profile and BATON_WRAPPER, its own pid
/// and session.
fn nested_run() -> Vec<Value> {
    vec![
        json!({ "nested": { "hook": "session-start", "session_id": NESTED } }),
        json!({ "nested": { "hook": "prompt", "session_id": NESTED } }),
        json!({ "nested": { "hook": "stop", "session_id": NESTED } }),
        json!({ "nested": { "hook": "session-end", "session_id": NESTED } }),
        json!({ "sleep": 300 }),
    ]
}

#[test]
fn a_claude_started_inside_the_conversation_does_not_take_the_wrapper_over() {
    let _serial = serial();
    let turn = limited(2 * H);
    let (before, failure) = (turn[..3].to_vec(), turn[3..].to_vec());
    let pause = vec![json!({ "sleep": 300 })];
    let mut run = lead(Setup::new(json!([
        steps(&[before, pause, nested_run(), failure, wait_end()]),
        start_then_exit(),
    ])));
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    let second = &run.invocations()[1];
    assert_eq!(second["configDir"], json!(run.profile("acct2")));
    let resumed = argv(second);
    assert_eq!(resumed[resumed.len() - 3..resumed.len() - 1], ["--resume", SID]);
    assert_eq!(lead.session_id(), Some(SID));
    assert!(!run.paths().sessions.join(NESTED).join("lock.json").exists());
    assert!(
        !lead
            .events()
            .iter()
            .any(|event| event.text("message") == Some("session") && event.text("session") == Some(NESTED))
    );
}

#[test]
fn behind_a_launcher_the_wrapper_follows_the_claude_whose_hooks_report_first_and_only_it() {
    let _serial = serial();
    let turn = limited(2 * H);
    let with_session = |step: &Value| {
        let mut step = step.clone();
        step["session_id"] = json!(CLEARED);
        step
    };
    let mut first = vec![turn[0].clone(), turn[1].clone(), json!({ "sleep": 300 })];
    // /clear: the same claude, a new conversation.
    first.push(json!({ "hook": "session-start", "source": "clear", "session_id": CLEARED }));
    first.push(json!({ "sleep": 300 }));
    first.extend(nested_run());
    first.push(with_session(&turn[2]));
    first.push(turn[3].clone());
    first.push(with_session(&turn[4]));
    let mut run = lead(Setup {
        launcher: true,
        ..Setup::new(json!([steps(&[first, wait_end()]), start_then_exit()]))
    });
    let (code, lead) = run.finish();
    assert_eq!(code, 0);
    let invocations = run.invocations();
    let resumed = argv(&invocations[1]);
    assert_eq!(
        resumed[resumed.len() - 3..resumed.len() - 1],
        ["--resume", CLEARED]
    );
    assert_eq!(lead.session_id(), Some(CLEARED));
    let bound = lead
        .events()
        .iter()
        .find(|event| event.text("message") == Some("claude pid"))
        .unwrap();
    let pid = invocations[0]["pid"].as_i64().unwrap();
    assert_eq!(
        (bound["spawned"].as_i64(), bound["pid"].as_i64()),
        (Some(pid), Some(pid + 1))
    );
}

#[test]
fn with_no_accounts_baton_asks_for_one_and_starts_nothing() {
    let _serial = serial();
    let home = temp_home();
    let dir = home.home.parent().unwrap().to_path_buf();
    let (mut options, _keys, output, _signals) = lead_options(&home, &dir, &[], EndMethod::Kill, 0.3);
    let spawned = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&spawned);
    options.spawn_claude = Some(Box::new(move |_| {
        seen.store(true, Ordering::SeqCst);
        Err(io::Error::other("must not start claude"))
    }));
    assert_eq!(Lead::new(options).run(), 1);
    assert!(!spawned.load(Ordering::SeqCst));
    assert_eq!(
        output.text(),
        "baton: No accounts yet: add one with baton account add <name> (or a in the menu), then sign it in with baton login <name>.\n"
    );
    // With accounts but a claude that cannot be started: said once, and the exit code is 1.
    let home = accounts_home(&["acct1"]);
    let (mut options, _keys, output, _signals) = options_for(&home);
    options.spawn_claude = Some(Box::new(|_| Err(io::Error::other("must not start claude"))));
    let mut lead = Lead::new(options);
    assert_eq!(lead.run(), 1);
    assert_eq!(
        output.text(),
        "baton: claude could not run: must not start claude\n"
    );
    assert!(has_event(&lead, "spawn error"));
    // And a claude that is nowhere to be found.
    let (mut options, _keys, output, _signals) = options_for(&home);
    options.claude = None;
    options.env.set("PATH", "");
    assert_eq!(Lead::new(options).run(), 1);
    assert_eq!(output.text(), "baton: Claude Code (claude) is not on PATH.\n");
}

/// The usual test options for a home that is already set up.
fn options_for(home: &TestHome) -> (LeadOptions, Sender<Vec<u8>>, Output, Ignored) {
    let dir = home.home.parent().unwrap().join("fake");
    lead_options(home, &dir, &[], EndMethod::Kill, 0.3)
}

#[test]
fn a_baton_session_is_an_instance_it_polls_while_it_runs_says_so_and_leaves_nothing_running() {
    let _serial = serial();
    let home = accounts_home(&["acct1"]);
    let paths = home.paths.clone();
    let root = home.home.parent().unwrap().to_path_buf();
    let dir = root.join("fake");
    write_json(
        &dir.join("scenario.json"),
        &json!({ "session": SID, "runs": [{ "steps": [{ "waitFile": "done" }, { "exit": 0 }] }] }),
    );
    let claudes = root.join("claudes.jsonl");
    let (mut options, _keys, output, _signals) =
        lead_options(&home, &dir, &["-p", "hello"], EndMethod::Kill, 0.3);
    // Found the way the command line finds it, and polling as a real session does.
    options.claude = None;
    options
        .env
        .set("BATON_CLAUDE", bin(FAKE_CLAUDE_BOTH).into_os_string());
    options
        .env
        .set("FAKE_USAGE_LOG", claudes.clone().into_os_string());
    options.poll = true;
    let done = thread::spawn(move || Lead::new(options).run());
    // Its start polls (no poll yet), under the account's profile.
    let usage = paths.usage.join("acct1.json");
    until_for(Duration::from_secs(30), || {
        read_json(&usage)
            .filter(|reading| reading.text("source") == Some("poll"))
            .map(|_| ())
    });
    let line = poll_line(&load_snapshot(
        &paths,
        now_ms(),
        Want {
            sessions: false,
            poll: true,
        },
    ))
    .unwrap();
    let by = format!(
        "Usage poll: by baton (pid {}), every 30 min, next ",
        std::process::id()
    );
    assert!(line.starts_with(&by), "{line}");
    write(&dir.join("done"), "");
    assert_eq!(done.join().unwrap(), 0);
    assert_eq!(output.text(), "", "the poll prints nothing in claude's terminal");
    assert_eq!(poll_holder(&paths, now_ms()), None);
    let line = poll_line(&load_snapshot(
        &paths,
        now_ms(),
        Want {
            sessions: false,
            poll: true,
        },
    ))
    .unwrap();
    assert!(line.contains("Usage poll: no baton running"), "{line}");
    let pids = |file: &Path| -> Vec<i64> {
        read_json_lines(file)
            .iter()
            .filter_map(|run| run.int("pid"))
            .collect()
    };
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
fn a_poll_that_hangs_never_holds_up_the_session_the_hop_happens_on_time_and_the_read_ends_with_the_wrapper() {
    let _serial = serial();
    let home = accounts_home(&["acct1", "acct2"]);
    let paths = home.paths.clone();
    let root = home.home.parent().unwrap().to_path_buf();
    let dir = root.join("fake");
    write_json(
        &dir.join("scenario.json"),
        &json!({ "session": SID, "runs": [steps(&[limited(2 * H), wait_end()]), start_then_exit()] }),
    );
    let claudes = root.join("claudes.jsonl");
    let (mut options, _keys, output, _signals) = lead_options(&home, &dir, &[], EndMethod::Kill, 0.0);
    options.claude = Some(launcher(FAKE_CLAUDE_BOTH));
    options.env.set("FAKE_USAGE_MODE", "hang");
    options
        .env
        .set("FAKE_USAGE_LOG", claudes.clone().into_os_string());
    options.poll = true;
    let started = Instant::now();
    assert_eq!(Lead::new(options).run(), 0);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "not held up by the hanging read"
    );
    assert_eq!(
        read_json_lines(&dir.join("invocations.jsonl")).len(),
        2,
        "it hopped"
    );
    let said = output.text().to_lowercase();
    assert!(
        !said.contains("poll") && !said.contains("usage"),
        "the poll said nothing: {said}"
    );
    // The hanging read was started, and ended with the wrapper.
    let reads: Vec<i64> = read_json_lines(&claudes)
        .iter()
        .filter_map(|run| run.int("pid"))
        .collect();
    assert!(!reads.is_empty(), "a read was running");
    until_for(Duration::from_secs(10), || {
        reads.iter().all(|pid| !is_alive(*pid)).then_some(())
    });
    assert_eq!(poll_holder(&paths, now_ms()), None);
}

#[test]
fn every_account_at_its_limit_baton_says_so_and_starts_on_the_first_one_back() {
    let _serial = serial();
    let mut run = lead(Setup {
        readings: vec![
            (
                "acct1",
                Reading {
                    five: (100.0, 2 * H),
                    week: (10.0, 72 * H),
                },
            ),
            (
                "acct2",
                Reading {
                    five: (100.0, H),
                    week: (10.0, 72 * H),
                },
            ),
        ],
        ..Setup::new(json!([{ "steps": [{ "exit": 0 }] }]))
    });
    assert_eq!(run.finish().0, 0);
    assert_eq!(run.invocations()[0]["configDir"], json!(run.profile("acct2")));
    let output = run.output.text();
    let said = regex::Regex::new(
        r"^baton: every account is at its limit; starting on acct2, the first back \(\d\d:\d\d:\d\d\)\. Claude Code's own wait applies\.\n$",
    )
    .unwrap();
    assert!(said.is_match(&output), "{output}");
}

#[test]
fn claude_gets_the_wrappers_own_terminal_unless_told_otherwise() {
    let _serial = serial();
    let home = accounts_home(&["acct1"]);
    let dir = home.home.parent().unwrap().join("fake");
    write_json(
        &dir.join("scenario.json"),
        &json!({ "runs": [{ "steps": [{ "exit": 5 }] }] }),
    );
    let (mut options, _keys, output, _signals) =
        lead_options(&home, &dir, &["--version"], EndMethod::Kill, 0.3);
    options.stdio = LeadStdio::Inherit;
    assert_eq!(Lead::new(options).run(), 5);
    assert_eq!(output.text(), "");
    assert_eq!(read_json_lines(&dir.join("invocations.jsonl")).len(), 1);
}

#[test]
fn full_sync_what_is_set_up_during_the_conversation_is_there_on_the_account_it_moves_to() {
    let _serial = serial();
    let mut run = lead(Setup::new(json!([
        { "steps": [
            { "hook": "session-start" },
            { "sleep": 200 },
            { "hook": "prompt", "prompt": "/hop" },
            // While this turn runs, a server is added and the folder trusted, on acct1.
            { "sleep": 900 },
            { "hop": "" },
            { "sleep": 300 },
            { "hook": "stop" },
            { "wait": "end" },
        ] },
        start_then_exit(),
    ])));
    until(|| run.has_step("hook").then_some(()));
    let paths = run.paths().clone();
    let settings_of = |account: &str| profile_dir(&paths, account).join(".claude.json");
    write_json(
        &settings_of("acct1"),
        &json!({
            "userID": "user-of-acct1",
            "theme": "light",
            "mcpServers": { "github": { "command": "gh" } },
            "projects": { "/work": { "hasTrustDialogAccepted": true, "allowedTools": ["Read"] } }
        }),
    );
    let (code, _lead) = run.finish();
    assert_eq!(code, 0);
    assert_eq!(hop_fields(&run, &["from", "to"]), [["acct1", "acct2"]]);
    let arrived = read_json(&settings_of("acct2")).unwrap();
    assert_eq!(arrived["theme"], "light");
    assert_eq!(arrived["mcpServers"], json!({ "github": { "command": "gh" } }));
    assert_eq!(arrived["projects"]["/work"]["hasTrustDialogAccepted"], true);
    assert_eq!(arrived["projects"]["/work"]["allowedTools"], json!(["Read"]));
    assert!(arrived.get("userID").is_none(), "never who the other account is");
}

#[test]
fn full_sync_before_a_session_starts_baton_asks_whose_setting_stays_when_two_accounts_differ() {
    let _serial = serial();
    let home = accounts_home(&["acct1", "acct2"]);
    let paths = home.paths.clone();
    let settings_of = |account: &str| profile_dir(&paths, account).join(".claude.json");
    write_json(&paths.claude_json, &json!({ "theme": "dark" }));
    write_json(
        &settings_of("acct1"),
        &json!({ "theme": "light", "userID": "one" }),
    );
    write_json(
        &settings_of("acct2"),
        &json!({ "theme": "dark-daltonized", "userID": "two" }),
    );
    let theme_of = |account: &str| read_json(&settings_of(account)).unwrap()["theme"].clone();
    // Nobody at a terminal (a pipe): nothing is asked, and nobody's theme is written over.
    let (mut options, _keys, output, _signals) = options_for(&home);
    options.spawn_claude = Some(Box::new(|_| Err(io::Error::other("stop here"))));
    assert_eq!(Lead::new(options).run(), 1);
    assert!(!output.text().contains("differ"), "{}", output.text());
    assert_eq!(
        (theme_of("acct1"), theme_of("acct2")),
        (json!("light"), json!("dark-daltonized"))
    );
    // At a terminal: one line, and the digit typed says whose value every account gets.
    let (mut options, keys, output, _signals) = options_for(&home);
    options.output_tty = true;
    options.env.set("NO_COLOR", "1");
    options.spawn_claude = Some(Box::new(|_| Err(io::Error::other("stop here"))));
    let typist = thread::spawn(move || {
        thread::sleep(Duration::from_millis(700));
        let _ = keys.send(b"2".to_vec());
    });
    assert_eq!(Lead::new(options).run(), 1);
    typist.join().unwrap();
    let said = output.text();
    assert!(
        said.contains(
            "baton: the accounts differ on theme · 1: \"light\" (acct1) · 2: \"dark-daltonized\" (acct2) · 3: \"dark\" (~/.claude.json) · any other key: later · Ctrl+C: quit"
        ),
        "{said}"
    );
    assert_eq!(
        (theme_of("acct1"), theme_of("acct2")),
        (json!("dark-daltonized"), json!("dark-daltonized"))
    );
    assert_eq!(read_json(&settings_of("acct1")).unwrap()["userID"], "one");
    // Answered: the next start asks nothing.
    let (mut options, _keys, output, _signals) = options_for(&home);
    options.output_tty = true;
    options.env.set("NO_COLOR", "1");
    options.spawn_claude = Some(Box::new(|_| Err(io::Error::other("stop here"))));
    assert_eq!(Lead::new(options).run(), 1);
    assert!(!output.text().contains("differ"), "{}", output.text());
}
