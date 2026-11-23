//! The session wrapper (`baton [claude args]`): starts `claude` under the picked account with the
//! terminal attached and stays its parent.
//!
//! It learns the conversation from the SessionStart hook (`BATON_WRAPPER` ties the hook to this
//! wrapper), holds the session's lock, follows the turns through the files the hooks write, and at a
//! turn end asks the policy (`decide_turn_end`) whether to stay, wait in place, hop or wait for an
//! account. To hop it ends claude (`terminate`), shows one line with a countdown, and resumes the same
//! conversation with `claude --resume <id> "<continue prompt>"` under the next account. Ctrl+C belongs
//! to claude: Baton ignores the console's interrupts while claude runs (claude, in the same console and
//! process group, gets them itself).

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use baton_core::claude_path::{ClaudeNotFound, Launcher, resolve_claude};
use baton_core::clock::{HOUR, MINUTE, Ms, iso, local, ms_of, now_ms};
use baton_core::config::{Policy, config_problem, effective_policy, load_config, name_of};
use baton_core::env::Env;
use baton_core::format::NO_ACCOUNTS;
use baton_core::fsutil::{append_json_line, random_hex, read_json, write_json_atomic};
use baton_core::json::{Get, Object, is_truthy};
use baton_core::oslock::OsLock;
use baton_core::paths::{Paths, profile_env};
use baton_core::policy::{
    AccountState, Decision, SessionFacts, TurnEnd, WaitDecision, burn_rate, decide_turn_end, decide_wait,
    hop_run, pick_start, states_for, turn_cost,
};
use baton_core::prompts::{continue_prompt, hop_line, stay_prompt, wait_line};
use baton_core::retention::clean_up;
use baton_core::sessions::{
    Lock, SessionLock, acquire_lock, hold_wrapper, read_session_file, read_turn_samples, remove_wrapper,
    valid_id, wrapper_file, wrapper_session_file,
};
use baton_core::state::{Want, load_snapshot};
use baton_core::sys::{ProcessTree, end_requested, flush_input, save_terminal};
use baton_core::term::{Terminal, notice, terminal_for};
use baton_core::usage::{LIMIT_ERRORS, Others, TurnSample, read_rises, record_event};
use baton_core::window::Limit;
use baton_profile::profile::{LinkOptions, carry_trust, link_profile};
use baton_profile::shared::{sync_everything, take_everything};
use baton_profile::sync::{Apply, HOME};
use serde_json::{Value, json};

use crate::args::{Live, ParsedArgs, is_session_id, parse_claude_args, resume_args};
use crate::ask::{Asking, ask};
use crate::countdown::{Answer, ConsoleKeys, KeyInput, PromptOptions, Step, key_prompt};
use crate::poll_host::{PollHost, PollHostOptions};
use crate::poller::Clock;
use crate::proc::exited;
use crate::terminate::{EndOptions, Ended, end_claude, wait_exit};

/// Starts `claude`: given the command with its arguments, environment, folder and stdio set.
pub type SpawnFn = Box<dyn Fn(&mut Command) -> io::Result<Child> + Send>;

/// Ends a running `claude` and says how it went.
pub type EndFn = Box<dyn Fn(&mut Child) -> Ended + Send>;

/// The console's interrupts (Ctrl+C, Ctrl+Break, on unix SIGINT and SIGQUIT), which belong to claude
/// while the wrapper runs.
pub trait Signals: Send {
    /// Starts ignoring the interrupts named (`SIGINT` and `SIGBREAK` on Windows, `SIGINT` and
    /// `SIGQUIT` elsewhere), without changing what a child process does with them.
    fn ignore(&mut self, names: &[&'static str]);

    /// Stops ignoring them.
    fn restore(&mut self, names: &[&'static str]);
}

/// The interrupts of this process: while they are ignored, Ctrl+C and Ctrl+Break (SIGINT and SIGQUIT
/// on unix) reach claude, which shares the console, and pass the wrapper by.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessSignals;

impl Signals for ProcessSignals {
    fn ignore(&mut self, _names: &[&'static str]) {
        baton_core::sys::ignore_interrupts(true);
    }

    fn restore(&mut self, _names: &[&'static str]) {
        baton_core::sys::ignore_interrupts(false);
    }
}

/// What `claude` is given for its input and output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LeadStdio {
    /// The wrapper's own terminal.
    #[default]
    Inherit,
    /// Nothing (tests).
    Null,
}

/// What a session wrapper is started with. Everything but the arguments, the paths and the
/// environment has a working default.
pub struct LeadOptions {
    /// Claude Code's own arguments.
    pub args: Vec<String>,
    /// Baton's paths.
    pub paths: Paths,
    /// The environment claude is started from.
    pub env: Env,
    /// The folder claude starts in.
    pub cwd: PathBuf,
    /// How Claude Code is started; None finds it (the configured path, `BATON_CLAUDE`, PATH).
    pub claude: Option<Launcher>,
    /// Starts claude; None spawns the command.
    pub spawn_claude: Option<SpawnFn>,
    /// Ends claude at a turn end; None asks the way the config says (`endMethod`).
    pub end: Option<EndFn>,
    /// Where the keys of the countdown and the wait come from.
    pub input: Box<dyn KeyInput + Send>,
    /// Where Baton's own lines go.
    pub output: Box<dyn Write + Send>,
    /// Whether that output is a terminal.
    pub output_tty: bool,
    /// How wide it is, when known.
    pub output_columns: Option<usize>,
    /// The console's interrupts.
    pub signals: Box<dyn Signals>,
    /// The clock.
    pub now: Clock,
    /// The time between two looks at the session's files, in milliseconds.
    pub poll_ms: u64,
    /// The time between two redraws of the wait line, in milliseconds.
    pub tick_ms: u64,
    /// The countdown before an automatic hop, in seconds; None takes the policy's.
    pub countdown_seconds: Option<f64>,
    /// The wait after a turn ends for its last status line reading, in milliseconds; None takes the
    /// policy's.
    pub settle_ms: Option<u64>,
    /// What claude is given for its input and output.
    pub stdio: LeadStdio,
    /// The wrapper is a baton instance: while it runs it takes its turn at the usage poll, silently,
    /// at a low priority and never in the way of the session; false (tests) leaves it out.
    pub poll: bool,
}

impl LeadOptions {
    /// A wrapper for these arguments on the operator's own terminal.
    pub fn new(args: Vec<String>, paths: Paths, env: Env) -> LeadOptions {
        let stderr = io::stderr();
        let output_tty = stderr.is_terminal();
        LeadOptions {
            args,
            paths,
            env,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            claude: None,
            spawn_claude: None,
            end: None,
            input: Box::new(ConsoleKeys::new()),
            output: Box::new(stderr),
            output_tty,
            output_columns: crossterm::terminal::size()
                .ok()
                .map(|(columns, _)| usize::from(columns)),
            signals: Box::new(ProcessSignals),
            now: Arc::new(now_ms),
            poll_ms: 250,
            tick_ms: 1000,
            countdown_seconds: None,
            settle_ms: None,
            stdio: LeadStdio::Inherit,
            poll: true,
        }
    }
}

impl std::fmt::Debug for LeadOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeadOptions")
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("claude", &self.claude)
            .field("poll_ms", &self.poll_ms)
            .field("tick_ms", &self.tick_ms)
            .field("countdown_seconds", &self.countdown_seconds)
            .field("settle_ms", &self.settle_ms)
            .field("stdio", &self.stdio)
            .field("poll", &self.poll)
            .finish_non_exhaustive()
    }
}

/// Runs a session wrapper to its end and returns the exit code: claude's own, 1 when nothing could be
/// started, 130 when the operator quit.
pub fn run_lead(options: LeadOptions) -> i32 {
    // The terminal being closed, or a signal to end: claude is ended and what the wrapper holds is
    // let go of, where the process used to be gone at once with its lock and records left behind.
    baton_core::sys::watch_end_requests();
    let code = Lead::new(options).run();
    baton_core::sys::end_handled();
    code
}

/// One start of claude.
#[derive(Clone, Debug)]
struct Launch {
    /// The account it runs under.
    account: String,
    /// The account the conversation comes from, for a resume.
    from: Option<String>,
    /// Claude Code's arguments.
    args: Vec<String>,
    /// The folder it starts in.
    cwd: PathBuf,
    /// Why: `start`, or the reason of the move.
    reason: &'static str,
}

/// A decision to end claude, with what it was decided from.
#[derive(Clone, Debug)]
struct Pending {
    /// The decision: a hop or a wait.
    decision: Decision,
    /// The failed turn: `{ error, window, until }`.
    failure: Option<Value>,
    /// The account the conversation runs on.
    from: String,
    /// Every account's state at the decision.
    states: Vec<AccountState>,
    /// The policy at the decision.
    policy: Policy,
}

/// How a run of claude ended.
enum Outcome {
    /// It exited by itself, with this code.
    Exit(i32),
    /// Baton ended it, for this decision.
    Ended(Box<Pending>),
}

/// Where the conversation resumes.
#[derive(Clone, Debug)]
struct Resume {
    /// The account to resume on.
    to: String,
    /// The account it ran on.
    from: String,
    /// Why: the reason of the move, or `stay`.
    reason: &'static str,
    /// The first prompt: None for the usual one, `Some(None)` for none at all.
    prompt: Option<Option<String>>,
    /// The limit that was reached, when one was.
    window: Option<Limit>,
    /// When it lifts, when known.
    until: Option<Ms>,
}

/// What follows the end of claude.
enum After {
    /// The operator quit; the exit code.
    Quit(i32),
    /// The conversation resumes.
    Resume(Resume),
}

/// An account's current name (the operator may rename it while the wrapper runs).
fn label_of(paths: &Paths, id: &str) -> String {
    name_of(&load_config(paths), id)
}

/// The process of a lock's holder, for telling the operator; `?` when its record does not say.
fn pid_of(holder: &Value) -> String {
    holder
        .int("pid")
        .map_or_else(|| "?".to_owned(), |pid| pid.to_string())
}

/// A local time of day, `15:20:00`.
fn time_of(at: Ms) -> String {
    let at = local(at);
    format!("{:02}:{:02}:{:02}", at.hour, at.minute, at.second)
}

/// The exit code of a claude that ended by itself: its own, or, when a signal ended it, 128 and the
/// signal's number as a shell would say it (143 for SIGTERM).
fn exit_code(status: &ExitStatus) -> i32 {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(status);
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    status
        .code()
        .unwrap_or_else(|| signal.map_or(1, |signal| 128 + signal))
}

/// How claude exited, for the log.
fn exit_json(status: Option<&ExitStatus>) -> Value {
    let Some(status) = status else {
        return Value::Null;
    };
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(status);
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    json!({ "code": status.code(), "signal": signal })
}

/// A decision, for the log and the wrapper's record.
fn decision_json(decision: &Decision) -> Value {
    let mut out = match decision {
        Decision::Stay { reason, .. } => json!({ "type": "none", "reason": reason }),
        Decision::WaitInPlace { reason, at } => {
            let mut out = json!({ "type": "wait-in-place", "reason": reason });
            if let Some(at) = at {
                out["at"] = json!(at);
            }
            out
        }
        Decision::Hop {
            to,
            reason,
            countdown,
            ..
        } => {
            json!({ "type": "hop", "to": to, "reason": reason, "countdown": countdown })
        }
        Decision::Wait { to, at, reason } => json!({ "type": "wait", "to": to, "at": at, "reason": reason }),
    };
    if let Decision::Stay { windows, .. } | Decision::Hop { windows, .. } = decision
        && !windows.is_empty()
    {
        out["windows"] = json!(windows);
    }
    out
}

/// How claude was ended, for the log.
fn ended_json(ended: &Ended) -> Value {
    let mut out = json!({ "method": ended.method, "forced": ended.forced });
    if let Some(rounds) = ended.rounds {
        out["rounds"] = json!(rounds);
    }
    if ended.method == "keys" {
        out["detail"] = json!(ended.detail);
    }
    out
}

/// Writes a wrapper's record: its base fields and what the moment adds. The registry is
/// informational: a write that fails is left at that.
fn write_record(paths: &Paths, wrapper: &str, base: &Object, extra: Value) {
    let mut record = base.clone();
    if let Value::Object(extra) = extra {
        record.extend(extra);
    }
    let _ = write_json_atomic(&wrapper_file(paths, wrapper), &record);
}

/// A session wrapper.
pub struct Lead {
    /// Whether it takes its turn at the usage poll.
    poll_enabled: bool,
    /// Its part in the usage poll, while it runs.
    poll_host: Option<PollHost>,
    /// Claude Code's own arguments.
    args: Vec<String>,
    /// Baton's paths.
    paths: Paths,
    /// The environment claude is started from.
    env: Env,
    /// The folder claude starts in.
    cwd: PathBuf,
    /// How Claude Code is started, once known.
    claude: Option<Launcher>,
    /// Starts claude.
    spawn_claude: Option<SpawnFn>,
    /// Ends claude at a turn end.
    end_override: Option<EndFn>,
    /// Where the keys of the countdown and the wait come from.
    input: Box<dyn KeyInput + Send>,
    /// Where Baton's own lines go.
    output: Box<dyn Write + Send>,
    /// How those lines are drawn.
    term: Terminal,
    /// The console's interrupts.
    signals: Box<dyn Signals>,
    /// The clock.
    now: Clock,
    /// The time between two looks at the session's files.
    poll_ms: u64,
    /// The time between two redraws of the wait line.
    tick_ms: u64,
    /// The countdown before an automatic hop, when the caller set it.
    countdown_override: Option<f64>,
    /// The wait after a turn end, when the caller set it.
    settle_override: Option<u64>,
    /// What claude is given for its input and output.
    stdio: LeadStdio,
    /// The arguments, read.
    parsed: ParsedArgs,
    /// This wrapper's id: what `BATON_WRAPPER` carries.
    wrapper: String,
    /// The conversation, once known.
    session_id: Option<String>,
    /// The conversation's lock, while this wrapper drives it.
    session_lock: Option<SessionLock>,
    /// The lock that says this wrapper runs.
    running: Option<OsLock>,
    /// Whether another wrapper drives the conversation: this one then decides nothing about it.
    held_elsewhere: bool,
    /// What to tell the operator, once claude is gone, about a conversation another wrapper drove.
    held_note: Option<String>,
    /// The account the conversation runs on.
    account: Option<String>,
    /// The claude this wrapper started: the pid its hooks report.
    claude_pid: Option<i64>,
    /// Whether a mapping from that claude has been seen since the launch.
    claude_seen: bool,
    /// When the conversation arrived on its account.
    arrived_at: Option<Ms>,
    /// The accounts the conversation left since it last settled on one, oldest first: a run of
    /// hops does not go back to them while another account is open.
    left: Vec<String>,
    /// When the wrapper started.
    started_at: Option<Ms>,
    /// When claude was last launched.
    launched_at: Ms,
    /// The last turn end that was looked at.
    last_turn_at: Ms,
    /// The last turn that ended well.
    last_stop_at: Ms,
    /// The last hop request that was looked at.
    last_hop_at: Ms,
    /// The session's permission mode, as last seen.
    permission_mode: Option<String>,
    /// The operator chose to stay on a limited account until then.
    stay_until: Ms,
    /// The operator declined a planned move until then.
    veto_until: Ms,
    /// Refusals right after a resume, in a row.
    reblocks: u32,
    /// Whether Baton resumed the conversation itself.
    resumed_by_baton: bool,
    /// Whether a turn ended well since that resume.
    ok_since_resume: bool,
    /// What the wrapper is doing: starting, running, ending, countdown, waiting.
    state: &'static str,
    /// What happened, for tests and the log.
    events: Vec<Value>,
}

impl std::fmt::Debug for Lead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lead")
            .field("wrapper", &self.wrapper)
            .field("session_id", &self.session_id)
            .field("account", &self.account)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Lead {
    /// A wrapper. Nothing runs until [`Lead::run`].
    pub fn new(options: LeadOptions) -> Lead {
        let parsed = parse_claude_args(&options.args);
        let session_id = parsed.session.id.clone().filter(|id| is_session_id(id));
        Lead {
            poll_enabled: options.poll,
            poll_host: None,
            term: terminal_for(options.output_tty, options.output_columns, &options.env),
            args: options.args,
            paths: options.paths,
            env: options.env,
            cwd: options.cwd,
            claude: options.claude,
            spawn_claude: options.spawn_claude,
            end_override: options.end,
            input: options.input,
            output: options.output,
            signals: options.signals,
            now: options.now,
            poll_ms: options.poll_ms,
            tick_ms: options.tick_ms,
            countdown_override: options.countdown_seconds,
            settle_override: options.settle_ms,
            stdio: options.stdio,
            parsed,
            wrapper: format!("{}-{}", std::process::id(), random_hex(4)),
            session_id,
            session_lock: None,
            running: None,
            held_elsewhere: false,
            held_note: None,
            account: None,
            claude_pid: None,
            claude_seen: false,
            arrived_at: None,
            left: Vec::new(),
            started_at: None,
            launched_at: 0,
            last_turn_at: 0,
            last_stop_at: 0,
            last_hop_at: 0,
            permission_mode: None,
            stay_until: 0,
            veto_until: 0,
            reblocks: 0,
            resumed_by_baton: false,
            ok_since_resume: false,
            state: "starting",
            events: Vec::new(),
        }
    }

    /// What happened so far: one entry per step, `{ at, message, ... }`, as the log file has them.
    pub fn events(&self) -> &[Value] {
        &self.events
    }

    /// The conversation the wrapper follows, once it knows it.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// This wrapper's id: what `BATON_WRAPPER` carries to everything claude runs.
    pub fn wrapper(&self) -> &str {
        &self.wrapper
    }

    /// The time now.
    fn time(&self) -> Ms {
        (self.now)()
    }

    /// An account's current name.
    fn label(&self, id: &str) -> String {
        label_of(&self.paths, id)
    }

    /// Writes one of Baton's own lines.
    fn say(&mut self, line: &str) {
        let _ = writeln!(self.output, "{}", notice(line, &self.term));
        let _ = self.output.flush();
    }

    /// Keeps what happened, in memory and in the wrapper's log. Logging never stops the session.
    fn log(&mut self, message: &str, data: Value) {
        let mut entry = Object::new();
        entry.insert("at".to_owned(), json!(iso(self.time())));
        entry.insert("message".to_owned(), json!(message));
        if let Value::Object(data) = data {
            entry.extend(data);
        }
        let entry = Value::Object(entry);
        let _ = append_json_line(
            &self.paths.logs.join(format!("lead-{}.log", self.wrapper)),
            &entry,
        );
        self.events.push(entry);
    }

    /// The wrapper's record as it stands.
    fn record(&self) -> Object {
        let mut record = Object::new();
        record.insert("pid".to_owned(), json!(std::process::id()));
        record.insert("wrapper".to_owned(), json!(self.wrapper));
        record.insert("account".to_owned(), json!(self.account));
        record.insert("session".to_owned(), json!(self.session_id));
        record.insert(
            "startedAt".to_owned(),
            json!(iso(self.started_at.unwrap_or_else(|| self.time()))),
        );
        record.insert("cwd".to_owned(), json!(self.cwd.to_string_lossy()));
        record.insert("state".to_owned(), json!(self.state));
        // The run of hops, for `baton hop` to say where the conversation goes next.
        record.insert("left".to_owned(), json!(self.left));
        record.insert("arrivedAt".to_owned(), json!(self.arrived_at.map(iso)));
        record.insert("endedWell".to_owned(), json!(self.ok_since_resume));
        record
    }

    /// Writes the wrapper's record, with what the moment adds.
    fn register(&self, extra: Value) {
        write_record(&self.paths, &self.wrapper, &self.record(), extra);
    }

    /// How Claude Code is started.
    fn launcher(&mut self) -> Result<Launcher, ClaudeNotFound> {
        if let Some(claude) = &self.claude {
            return Ok(claude.clone());
        }
        let configured = load_config(&self.paths).claude_path;
        let claude = resolve_claude(configured.as_deref(), &self.env, Some(&self.paths.home))?;
        self.claude = Some(claude.clone());
        Ok(claude)
    }

    /// Brings the account's profile in line before claude runs under it.
    fn prepare(&mut self, account: &str) {
        match link_profile(&self.paths, account, &LinkOptions::default()) {
            Ok(report) => {
                let problems: Vec<_> = report
                    .iter()
                    .filter(|entry| matches!(entry.action, "conflict" | "removed-unsafe-link"))
                    .collect();
                if !problems.is_empty() {
                    self.log(
                        "profile problems",
                        json!({ "account": account, "problems": problems }),
                    );
                }
            }
            Err(error) => {
                let name = self.label(account);
                self.say(&format!("baton: could not refresh the {name} profile: {error}"));
            }
        }
    }

    /// Runs the wrapper to its end and returns the exit code.
    pub fn run(&mut self) -> i32 {
        let names: &[&'static str] = if cfg!(windows) {
            &["SIGINT", "SIGBREAK"]
        } else {
            &["SIGINT", "SIGQUIT"]
        };
        self.signals.ignore(names);
        self.started_at = Some(self.time());
        self.running = hold_wrapper(&self.paths, &self.wrapper);
        let code = self.main();
        self.signals.restore(names);
        self.session_lock = None;
        if let Some(note) = self.held_note.take() {
            self.say(&note);
        }
        self.running = None;
        remove_wrapper(&self.paths, &self.wrapper);
        // The poll goes with the wrapper: its lease, and a read in progress.
        if let Some(host) = self.poll_host.take() {
            host.stop();
        }
        code
    }

    /// Picks the account, starts claude, and follows the conversation from account to account.
    fn main(&mut self) -> i32 {
        // A config that cannot be read is not a config with no accounts.
        if let Some(problem) = config_problem(&self.paths) {
            self.say(&format!("baton: {problem}"));
            return 1;
        }
        if load_config(&self.paths).accounts.is_empty() {
            self.say(&format!("baton: {NO_ACCOUNTS}"));
            return 1;
        }
        let launcher = match self.launcher() {
            Ok(launcher) => launcher,
            Err(error) => {
                self.say(&format!("baton: {error}"));
                return 1;
            }
        };
        if self.poll_enabled {
            let mut options = PollHostOptions::new(self.paths.clone(), "start", self.env.clone());
            options.launcher = Some(launcher.clone());
            options.low = true;
            let host = PollHost::new(options);
            host.start();
            self.poll_host = Some(host);
        }
        let snapshot = load_snapshot(&self.paths, self.time(), Want::default());
        // Housekeeping only: what the policy says is old goes.
        let cleaned = clean_up(&self.paths, &snapshot.policy, self.time());
        if cleaned.total() > 0 {
            self.log("cleaned up", json!(cleaned));
        }
        // A chat whose model is known meets that model's limits; any other, the strictest.
        let states = states_for(&snapshot.states, self.model().as_deref(), snapshot.now);
        let pick = pick_start(&states, &snapshot.policy, &[]);
        let account = match pick.account.clone() {
            Some(account) => account,
            None => {
                let Some(first) = pick
                    .earliest
                    .clone()
                    .or_else(|| snapshot.states.first().map(|s| s.id.clone()))
                else {
                    self.say(&format!("baton: {NO_ACCOUNTS}"));
                    return 1;
                };
                let back = match pick.available_at.filter(|_| pick.available_known) {
                    Some(at) => format!(" ({})", time_of(at)),
                    None => String::new(),
                };
                let name = self.label(&first);
                self.say(&format!(
                    "baton: every account is at its limit; starting on {name}, the first back{back}. Claude Code's own wait applies."
                ));
                first
            }
        };
        if !self.parsed.interactive {
            return self.passthrough(&launcher, &account);
        }
        if !self.ask_settings() {
            return 130;
        }
        if let Some(id) = self.session_id.clone() {
            match acquire_lock(&self.paths, &id, &self.lock_owner(&account)) {
                Lock::Taken(lock) => self.session_lock = Some(lock),
                Lock::Held(holder) => {
                    self.say(&format!(
                        "baton: conversation {id} is already running under baton (pid {}); one conversation runs in one process at a time.",
                        pid_of(&holder)
                    ));
                    return 1;
                }
                // No lock could be made at all (the folder, the file system): Claude Code still runs.
                Lock::Failed(error) => {
                    self.log("session lock failed", json!({ "session": id, "error": error }))
                }
            }
        }
        let mut launch = Launch {
            account,
            from: None,
            args: self.args.clone(),
            cwd: self.cwd.clone(),
            reason: "start",
        };
        loop {
            let pending = match self.run_claude(&launcher, &launch) {
                Outcome::Exit(code) => return code,
                Outcome::Ended(pending) => pending,
            };
            launch = match self.after_end(&pending) {
                After::Quit(code) => return code,
                After::Resume(next) => self.resume_launch(&next),
            };
        }
    }

    /// Who holds a session's lock: this wrapper.
    fn lock_owner(&self, account: &str) -> Value {
        json!({
            "pid": std::process::id(),
            "wrapper": self.wrapper,
            "at": iso(self.time()),
            "account": account,
        })
    }

    /// A launch that is not an interactive session (`-p`, `--version`, a subcommand): claude under
    /// the picked account, and nothing else.
    fn passthrough(&mut self, launcher: &Launcher, account: &str) -> i32 {
        self.prepare(account);
        let (args, cwd) = (self.args.clone(), self.cwd.clone());
        let Some(mut child) = self.spawn(launcher, account, &args, &cwd) else {
            return 1;
        };
        child.wait().map_or(1, |status| exit_code(&status))
    }

    /// Starts claude under an account's profile. Always a profile of Baton's: never the default
    /// Claude Code account.
    fn spawn(&mut self, launcher: &Launcher, account: &str, args: &[String], cwd: &Path) -> Option<Child> {
        let mut command = launcher.command(args);
        let mut env = profile_env(&self.paths, account, &self.env);
        env.set("BATON_WRAPPER", self.wrapper.as_str());
        env.apply(&mut command);
        command.current_dir(cwd);
        if self.stdio == LeadStdio::Null {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
        }
        let spawned = match &self.spawn_claude {
            Some(spawn) => spawn(&mut command),
            None => command.spawn(),
        };
        match spawned {
            Ok(child) => Some(child),
            Err(error) => {
                self.log("spawn error", json!({ "error": error.to_string() }));
                self.say(&format!("baton: claude could not run: {error}"));
                None
            }
        }
    }

    /// Runs claude once: until it exits by itself, or until a turn end decides to move the
    /// conversation and Baton ends it.
    fn run_claude(&mut self, launcher: &Launcher, launch: &Launch) -> Outcome {
        if let Some(from) = launch.from.as_deref().filter(|from| *from != launch.account)
            && let Err(error) = carry_trust(&self.paths, from, &launch.account, &launch.cwd.to_string_lossy())
        {
            self.log("trust carry failed", json!({ "error": error.to_string() }));
        }
        self.prepare(&launch.account);
        if self.account.as_deref() != Some(launch.account.as_str()) {
            self.arrived_at = Some(self.time());
            // The account it leaves joins the run of hops, and the one it goes to leaves it.
            let previous = self.account.take();
            self.left
                .retain(|id| Some(id) != previous.as_ref() && *id != launch.account);
            self.left.extend(previous);
        }
        self.account = Some(launch.account.clone());
        self.launched_at = self.time();
        self.state = "running";
        self.register(json!({}));
        self.log(
            "launch",
            json!({ "account": launch.account, "args": launch.args, "reason": launch.reason }),
        );
        // How the terminal is set now is how it is set again once claude is gone, however it goes.
        // What was typed for a claude that is gone is not for the next one.
        let own_terminal = self.stdio == LeadStdio::Inherit;
        let terminal = if own_terminal { save_terminal() } else { None };
        if own_terminal && launch.reason != "start" {
            flush_input();
        }
        let Some(mut child) = self.spawn(launcher, &launch.account, &launch.args, &launch.cwd) else {
            return Outcome::Exit(1);
        };
        // Claude with what it starts (MCP servers, shells), should it have to be ended by force.
        let tree = ProcessTree::foreground(child.id());
        self.claude_pid = Some(i64::from(child.id()));
        self.claude_seen = false;
        let outcome = self.follow(&mut child, &tree);
        if let Some(terminal) = terminal {
            terminal.restore();
        }
        outcome
    }

    /// Follows a running claude: until it exits by itself, or until a turn end decides to move the
    /// conversation and Baton ends it.
    fn follow(&mut self, child: &mut Child, tree: &ProcessTree) -> Outcome {
        loop {
            if wait_exit(child, self.poll_ms) {
                break;
            }
            if let Some(code) = end_requested() {
                return self.end_on_request(child, tree, code);
            }
            let Some(pending) = self.poll(child) else {
                continue;
            };
            if exited(child) {
                continue;
            }
            self.state = "ending";
            let decision = decision_json(&pending.decision);
            self.register(json!({ "decision": decision["type"], "target": decision["to"] }));
            let mut logged = decision.clone();
            logged["failure"] = json!(pending.failure);
            logged["from"] = json!(pending.from);
            self.log("ending claude", json!({ "decision": logged }));
            let result = match &self.end_override {
                Some(end) => end(child),
                None => {
                    let method = load_config(&self.paths).end_method;
                    end_claude(
                        child,
                        &EndOptions {
                            method,
                            tree: Some(tree),
                            ..EndOptions::default()
                        },
                    )
                }
            };
            let status = child.wait().ok();
            self.log(
                "claude ended",
                json!({ "result": ended_json(&result), "exit": exit_json(status.as_ref()) }),
            );
            return Outcome::Ended(Box::new(pending));
        }
        let status = child.wait().ok();
        self.log("claude exited", json!({ "exit": exit_json(status.as_ref()) }));
        Outcome::Exit(status.as_ref().map_or(1, exit_code))
    }

    /// The wrapper was asked to end (the terminal closed, SIGTERM, SIGHUP): claude is asked to end as
    /// it would be for a hop, given a few seconds, and ended with what it started if it is still
    /// there. The wrapper then leaves with the request's exit code.
    fn end_on_request(&mut self, child: &mut Child, tree: &ProcessTree, code: i32) -> Outcome {
        self.log("end requested", json!({ "code": code }));
        // A closed console has told claude itself; a signal reached the wrapper alone.
        if !(cfg!(unix) && baton_core::sys::terminate(child.id())) && cfg!(unix) {
            let _ = child.kill();
        }
        if !wait_exit(child, 3000) {
            crate::terminate::force_kill(child, Some(tree));
        }
        let status = child.wait().ok();
        self.log("claude ended", json!({ "exit": exit_json(status.as_ref()) }));
        Outcome::Exit(code)
    }

    /// Follows another conversation: the one the wrapper's own claude reported. One that another
    /// wrapper already drives is followed for what it is, and nothing is decided about it here.
    fn switch_session(&mut self, id: &str) {
        if self.session_id.as_deref() == Some(id) {
            return;
        }
        // The previous conversation is free for another wrapper.
        self.session_lock = None;
        self.held_elsewhere = false;
        let account = self.account.clone().unwrap_or_default();
        match acquire_lock(&self.paths, id, &self.lock_owner(&account)) {
            Lock::Taken(lock) => self.session_lock = Some(lock),
            Lock::Held(holder) => {
                self.held_elsewhere = true;
                self.held_note = Some(format!(
                    "baton: conversation {id} was already running under another baton (pid {}); that one moves it between accounts, this one left it alone.",
                    pid_of(&holder)
                ));
                self.log(
                    "session lock held elsewhere",
                    json!({ "session": id, "holder": holder }),
                );
            }
            Lock::Failed(error) => {
                self.log("session lock failed", json!({ "session": id, "error": error }));
            }
        }
        let previous = self.session_id.replace(id.to_owned());
        self.register(json!({}));
        self.log("session", json!({ "session": id, "previous": previous }));
    }

    /// Asks again for the lock of a conversation another wrapper drove: that wrapper may be gone.
    /// What happened in the conversation meanwhile was not this wrapper's to act on.
    fn claim(&mut self, id: &str) -> bool {
        let account = self.account.clone().unwrap_or_default();
        let Lock::Taken(lock) = acquire_lock(&self.paths, id, &self.lock_owner(&account)) else {
            return false;
        };
        self.session_lock = Some(lock);
        self.held_elsewhere = false;
        self.held_note = None;
        let now = self.time();
        self.last_turn_at = self.last_turn_at.max(now);
        self.last_hop_at = self.last_hop_at.max(now);
        self.log("session lock taken", json!({ "session": id }));
        true
    }

    /// Whether the session mapping was written by the claude this wrapper started.
    ///
    /// A claude started inside the session (a `claude -p` it runs, a hook's) inherits `BATON_WRAPPER`
    /// and its hooks write the same mapping; following it would hand this wrapper another
    /// conversation. The mapping carries the writer's pid (none from a Claude Code that does not give
    /// it: taken, as before). The first one written after a launch that is not the spawned process's
    /// binds its pid (a launcher in between): nothing inside claude runs before claude's own
    /// SessionStart.
    fn is_own_claude(&mut self, mapped: &Value) -> bool {
        let Some(pid) = mapped.int("pid") else {
            return true;
        };
        if Some(pid) == self.claude_pid {
            self.claude_seen = true;
            return true;
        }
        if self.claude_seen || ms_of(mapped.at("at")).unwrap_or(0) < self.launched_at {
            return false;
        }
        self.log("claude pid", json!({ "spawned": self.claude_pid, "pid": pid }));
        self.claude_pid = Some(pid);
        self.claude_seen = true;
        true
    }

    /// One look at the session's files. Returns a decision to end claude (a hop or a wait) or None.
    fn poll(&mut self, child: &mut Child) -> Option<Pending> {
        let mapped = read_json(&wrapper_session_file(&self.paths, &self.wrapper)).filter(Value::is_object);
        if let Some(mapped) = mapped
            && let Some(session) = mapped.text("session").filter(|id| valid_id(id))
            && self.is_own_claude(&mapped)
            && self.session_id.as_deref() != Some(session)
        {
            self.switch_session(session);
        }
        let id = self.session_id.clone()?;
        if self.held_elsewhere && !self.claim(&id) {
            return None;
        }
        let read = |name: &str| read_session_file(&self.paths, &id, name).unwrap_or(Value::Null);
        let mut turn = read("turn.json");
        let hop = read("hop.json");
        let turn_at = ms_of(turn.at("at")).unwrap_or(0);
        let hop_at = ms_of(hop.at("at")).unwrap_or(0);
        if turn_at >= self.launched_at
            && let Some(mode) = turn.word("permissionMode")
        {
            self.permission_mode = Some(mode.to_owned());
        }
        // A request made while claude was being started again counts too: only what was asked
        // before this wrapper began is somebody else's.
        let asked_here = hop_at >= self.started_at.unwrap_or(self.launched_at);
        let pending_hop = !hop.is_null() && hop_at > self.last_hop_at && asked_here;
        let state = turn.text("state");
        let fresh_turn_end =
            state == Some("idle") && turn_at > self.last_turn_at && turn_at >= self.launched_at;
        let running = state == Some("running") && turn_at >= self.launched_at;
        if !fresh_turn_end && !(pending_hop && !running) {
            return None;
        }

        let mut event: Option<String> = None;
        let mut failure: Option<Value> = None;
        // The turn that carries a hop request settles nothing: `baton hop` said where the
        // conversation goes before that turn ended.
        let ended_well = self.ok_since_resume;
        if fresh_turn_end {
            self.last_turn_at = turn_at;
            event = turn.text("event").map(str::to_owned);
            match event.as_deref() {
                Some("stop") => {
                    if !self.ok_since_resume {
                        self.ok_since_resume = true;
                        self.register(json!({}));
                    }
                    self.reblocks = 0;
                }
                Some("stop-failure") => {
                    failure = Some(json!({
                        "error": turn.at("error"),
                        "window": turn.at("window"),
                        "until": turn.at("until"),
                    }));
                    let limit = turn
                        .text("error")
                        .is_some_and(|error| LIMIT_ERRORS.contains(&error));
                    if limit && self.resumed_by_baton && !self.ok_since_resume {
                        self.reblocks += 1;
                    }
                }
                _ => {}
            }
        }
        if pending_hop {
            self.last_hop_at = hop_at;
        }

        if event.as_deref() == Some("stop") && !pending_hop {
            // Let the last status line reading of the turn land, and give up if a new turn starts
            // meanwhile.
            let settle = match self.settle_override {
                Some(settle) => settle,
                None => effective_policy(&load_config(&self.paths)).settle_ms.max(0.0) as u64,
            };
            if settle > 0 {
                thread::sleep(Duration::from_millis(settle));
            }
            if exited(child) {
                return None;
            }
            turn = read_session_file(&self.paths, &id, "turn.json").unwrap_or(Value::Null);
            if ms_of(turn.at("at")).unwrap_or(0) > self.last_turn_at {
                return None;
            }
        }

        let now = self.time();
        let snapshot = load_snapshot(&self.paths, now, Want::default());
        let mut samples = read_turn_samples(&self.paths, &id);
        let live = read_session_file(&self.paths, &id, "live.json").unwrap_or(Value::Null);
        let reading = live.at("reading");
        if is_truthy(reading)
            && live.text("account") == self.account.as_deref()
            && ms_of(live.at("at")).unwrap_or(0) > samples.last().and_then(|sample| sample.at).unwrap_or(0)
        {
            samples.push(TurnSample::from_value(&json!({
                "at": live.at("at"),
                "account": live.at("account"),
                "five_hour": reading.at("five_hour"),
                "seven_day": reading.at("seven_day"),
            })));
        }
        // What other sessions used of the same accounts while this one's turns ran is not its cost.
        let since = samples.first().and_then(|sample| sample.at).unwrap_or(now);
        let rises = read_rises(&self.paths, since, now);
        let others = Others {
            rises: &rises,
            session: &id,
        };
        let compact_at = read_session_file(&self.paths, &id, "compact.json")
            .and_then(|compact| ms_of(compact.at("at")))
            .unwrap_or(0);
        let compacted = compact_at > self.last_stop_at && compact_at >= self.launched_at;
        let account = self.account.clone().unwrap_or_default();
        let tasks = turn.at("backgroundTasks");
        // The accounts as this chat's model meets them: another model's own limit holds nothing
        // back here, and this model's own limit blocks an account whose week still has room.
        let model = live.word("model").map(str::to_owned).or_else(|| self.model());
        let states = states_for(&snapshot.states, model.as_deref(), now);
        // The run of hops is over once the conversation has settled here.
        let settled = hop_run(
            &self.left,
            self.arrived_at,
            if pending_hop {
                ended_well
            } else {
                self.ok_since_resume
            },
            &snapshot.policy,
            now,
        )
        .is_empty();
        if settled && !self.left.is_empty() {
            self.left.clear();
            self.register(json!({}));
        }
        let decision = decide_turn_end(&TurnEnd {
            now,
            policy: &snapshot.policy,
            states: &states,
            current: &account,
            event: event.as_deref().unwrap_or("idle"),
            failure_error: failure.as_ref().and_then(|failure| failure.text("error")),
            hop_request: pending_hop.then(|| hop.word("to")),
            session: SessionFacts {
                arrived_at: self.arrived_at,
                stay_until: Some(self.stay_until),
                veto_until: Some(self.veto_until),
                reblocks: f64::from(self.reblocks),
                left: self.left.clone(),
                compacted,
                background_tasks: if tasks.is_null() { json!([]) } else { tasks.clone() },
                cost: turn_cost(&samples, &others, &snapshot.policy),
                burn: Some(burn_rate(&samples, &others, &snapshot.policy, now)),
            },
        });
        if event.as_deref() == Some("stop") {
            self.last_stop_at = turn_at;
        }
        self.log(
            "turn end",
            json!({
                "event": event,
                "failure": failure,
                "hopRequest": if pending_hop { hop.clone() } else { Value::Null },
                "left": self.left,
                "decision": decision_json(&decision),
            }),
        );
        match decision {
            Decision::Hop { .. } | Decision::Wait { .. } => Some(Pending {
                decision,
                failure,
                from: account,
                states,
                policy: snapshot.policy,
            }),
            _ => None,
        }
    }

    /// After claude was ended: the countdown or the wait, then where to resume.
    fn after_end(&mut self, pending: &Pending) -> After {
        // Keys left over from ending claude (a Ctrl+C more than it took), or typed for it, would
        // answer the countdown before the operator read it.
        if self.stdio == LeadStdio::Inherit {
            flush_input();
        }
        let from = self.account.clone().unwrap_or_else(|| pending.from.clone());
        let failure = pending.failure.as_ref();
        let named = failure
            .and_then(|failure| failure.text("window"))
            .map(Limit::parse);
        let short = match &pending.decision {
            Decision::Hop { windows, .. } => windows.first().map(|window| Limit::from(*window)),
            _ => None,
        };
        let window = named.or(short);
        let until = failure.and_then(|failure| ms_of(failure.at("until")));
        let resume = |to: &str, reason: &'static str| {
            After::Resume(Resume {
                to: to.to_owned(),
                from: from.clone(),
                reason,
                prompt: None,
                window,
                until,
            })
        };
        match &pending.decision {
            Decision::Hop {
                to,
                reason,
                countdown,
                ..
            } => {
                let reason = *reason;
                let seconds = self
                    .countdown_override
                    .unwrap_or(pending.policy.countdown_seconds);
                if !*countdown || seconds <= 0.0 {
                    let line = hop_line(
                        &self.label(&from),
                        &self.label(to),
                        reason,
                        window,
                        until,
                        self.time(),
                        None,
                    );
                    self.say(&line);
                    return resume(to, reason);
                }
                self.state = "countdown";
                self.register(json!({ "target": to }));
                let deadline = self.time() + (seconds * 1000.0) as Ms;
                let tick_ms = self.tick_ms.min(1000);
                let answer = {
                    let Lead {
                        input,
                        output,
                        paths,
                        term,
                        now,
                        ..
                    } = self;
                    let options = PromptOptions {
                        tick_ms,
                        now: now.as_ref(),
                        ..PromptOptions::default()
                    };
                    key_prompt(input.as_mut(), output.as_mut(), &options, |t| {
                        if t >= deadline {
                            return Ok(Step::done(()));
                        }
                        let left = ((deadline - t) as f64 / 1000.0).ceil() as i64;
                        let line = hop_line(
                            &label_of(paths, &from),
                            &label_of(paths, to),
                            reason,
                            window,
                            until,
                            t,
                            Some(left),
                        );
                        Ok(Step::line(notice(&line, term)))
                    })
                };
                match answer {
                    Answer::Quit => self.quit(),
                    Answer::Stay => self.stay(&from, pending, reason, window, until),
                    Answer::Go | Answer::Tick(()) => resume(to, reason),
                }
            }
            Decision::Wait { to, at, reason } => {
                // Every account is blocked (or the readings proved wrong).
                let reason = *reason;
                self.state = "waiting";
                let mut target = to.clone();
                let mut at = *at;
                self.register(json!({ "target": target, "until": iso(at) }));
                let base = self.record();
                let tick_ms = self.tick_ms;
                let model = self.model();
                let answer = {
                    let Lead {
                        input,
                        output,
                        paths,
                        term,
                        now,
                        wrapper,
                        ..
                    } = self;
                    let options = PromptOptions {
                        tick_ms,
                        now: now.as_ref(),
                        ..PromptOptions::default()
                    };
                    key_prompt(input.as_mut(), output.as_mut(), &options, |t| {
                        // Fresh readings every tick: another session may report an earlier reset.
                        let snapshot = load_snapshot(paths, t, Want::default());
                        let states = states_for(&snapshot.states, model.as_deref(), t);
                        if reason != "reblocked" || t >= at {
                            match decide_wait(&states, &snapshot.policy, Some(target.as_str()), Some(at)) {
                                WaitDecision::Resume { to, .. } => return Ok(Step::done(to)),
                                WaitDecision::Wait {
                                    to: next,
                                    at: next_at,
                                } => {
                                    let next = next.unwrap_or_else(|| target.clone());
                                    let next_at = next_at.unwrap_or(at);
                                    if next != target || next_at != at {
                                        target = next;
                                        at = next_at;
                                        write_record(
                                            paths,
                                            wrapper,
                                            &base,
                                            json!({ "target": target, "until": iso(at) }),
                                        );
                                    }
                                }
                            }
                            if at <= t {
                                // Not open yet although due: look again in a minute.
                                at = t + MINUTE;
                            }
                        }
                        let blocking = states.iter().find(|state| state.id == target).and_then(|state| {
                            match state.blocked_by.as_slice() {
                                [only] if *only != Limit::Unknown => Some(*only),
                                _ => None,
                            }
                        });
                        let line = wait_line(
                            snapshot.states.len(),
                            &label_of(paths, &target),
                            blocking,
                            Some(at),
                            t,
                            reason,
                        );
                        Ok(Step::line(notice(&line, term)))
                    })
                };
                match answer {
                    Answer::Quit => self.quit(),
                    Answer::Stay | Answer::Go => self.stay(&from, pending, reason, window, until),
                    Answer::Tick(to) => resume(
                        &to,
                        if reason == "reblocked" {
                            "reblocked"
                        } else {
                            "all-blocked-wait"
                        },
                    ),
                }
            }
            // Only a hop or a wait ends claude.
            _ => self.quit(),
        }
    }

    /// With full sync on and somebody at the terminal: asks about the settings two accounts changed in
    /// different ways, and gives every account the value the operator picks. What is not answered stays
    /// as it is, on every account, and is asked again at the next start (`baton sync` shows it too).
    /// Returns false when the operator quit.
    fn ask_settings(&mut self) -> bool {
        if !self.term.tty {
            return true;
        }
        let config = load_config(&self.paths);
        let now = self.time();
        // What changed since Baton last looked may be a question by now.
        let Ok(report) = sync_everything(&self.paths, &config, Apply::Nothing, now) else {
            return true;
        };
        if report.questions.is_empty() {
            return true;
        }
        let label = |who: &str| {
            if who == HOME {
                "home".to_owned()
            } else {
                name_of(&config, who)
            }
        };
        let answers = ask(
            &report.questions,
            Asking {
                input: self.input.as_mut(),
                output: self.output.as_mut(),
                term: &self.term,
                label: &label,
                wait: Duration::from_secs(60),
            },
        );
        for (leaf, who) in &answers.taken {
            if let Err(error) = take_everything(&self.paths, &config, std::slice::from_ref(leaf), who, now) {
                self.say(&format!("baton: could not keep that answer: {error}"));
            }
        }
        !answers.quit
    }

    /// The operator quit: says how to pick the conversation up again.
    fn quit(&mut self) -> After {
        if let Some(id) = self.session_id.clone() {
            self.say(&format!(
                "baton: stopped. Resume the conversation with: baton --resume {id}"
            ));
        }
        After::Quit(end_requested().unwrap_or(130))
    }

    /// The operator chose to stay: no move for a while, and the conversation resumes where it was.
    fn stay(
        &mut self,
        from: &str,
        pending: &Pending,
        reason: &str,
        window: Option<Limit>,
        until: Option<Ms>,
    ) -> After {
        let limited = matches!(reason, "refusal" | "all-blocked-wait" | "reblocked");
        let now = self.time();
        if limited {
            let back = pending
                .states
                .iter()
                .find(|state| state.id == from)
                .filter(|state| state.available_known)
                .and_then(|state| state.available_at)
                .filter(|at| *at > now);
            self.stay_until =
                back.unwrap_or_else(|| now + (pending.policy.unknown_rejection_hours * HOUR as f64) as Ms);
        } else {
            self.veto_until = now + (pending.policy.min_stay_minutes * MINUTE as f64) as Ms;
        }
        let name = self.label(from);
        self.say(&format!("baton: staying on {name}."));
        After::Resume(Resume {
            to: from.to_owned(),
            from: from.to_owned(),
            reason: "stay",
            prompt: Some(limited.then(|| stay_prompt(window, until, now))),
            window,
            until,
        })
    }

    /// The model the conversation runs, as far as it is known: what its status line last said,
    /// else what it started with, else what the command line asks for. None when nothing says.
    fn model(&self) -> Option<String> {
        let seen = self.session_id.as_deref().and_then(|id| {
            ["live.json", "start.json"].iter().find_map(|name| {
                read_session_file(&self.paths, id, name)?
                    .word("model")
                    .map(str::to_owned)
            })
        });
        seen.or_else(|| self.parsed.value("--model").map(str::to_owned))
    }

    /// The launch that resumes the conversation: the original options, the session's live model,
    /// effort and permission mode, and the prompt that tells claude what happened.
    fn resume_launch(&mut self, next: &Resume) -> Launch {
        let id = self.session_id.clone().unwrap_or_default();
        let read = |name: &str| read_session_file(&self.paths, &id, name).unwrap_or(Value::Null);
        let live = read("live.json");
        let start = read("start.json");
        // Where the conversation is resumed: in the folder it belongs to, which is where Claude Code
        // looks for it, and not in the folder its work has moved to since.
        let cwd = [live.word("project"), start.word("cwd"), live.word("cwd")]
            .into_iter()
            .flatten()
            .map(PathBuf::from)
            .find(|cwd| cwd.exists())
            .unwrap_or_else(|| self.cwd.clone());
        let prompt = match &next.prompt {
            Some(prompt) => prompt.clone(),
            None => Some(continue_prompt(next.reason, next.window, next.until, self.time())),
        };
        let seen = Live {
            model: live.word("model").map(str::to_owned),
            effort: live.word("effort").map(str::to_owned),
            permission_mode: self.permission_mode.clone(),
        };
        let args = resume_args(&self.parsed, &id, prompt.as_deref(), &seen);
        if !self.parsed.guessed.is_empty() {
            self.log(
                "unknown options kept with the token after them",
                json!({ "options": self.parsed.guessed }),
            );
        }
        if next.reason == "stay" {
            let event = json!({ "type": "stay", "session": id, "account": next.to, "wrapper": self.wrapper });
            record_event(&self.paths, event, self.time());
        } else {
            // A wait that ends on the account it began on moved nothing.
            if next.from != next.to {
                let event = json!({
                    "type": "hop",
                    "session": id,
                    "from": next.from,
                    "to": next.to,
                    "reason": next.reason,
                    "wrapper": self.wrapper,
                });
                record_event(&self.paths, event, self.time());
            }
            self.resumed_by_baton = true;
            self.ok_since_resume = false;
        }
        let name = self.label(&next.to);
        self.say(&format!("baton: resuming {id} on {name}"));
        Launch {
            account: next.to.clone(),
            from: Some(next.from.clone()),
            args,
            cwd,
            reason: next.reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::window::Window;

    #[test]
    fn decisions_are_logged_in_the_shape_they_always_had() {
        let stay = Decision::Stay {
            reason: "stay",
            windows: Vec::new(),
        };
        assert_eq!(
            decision_json(&stay).to_string(),
            r#"{"type":"none","reason":"stay"}"#
        );
        let short = Decision::Stay {
            reason: "planned-no-target",
            windows: vec![Window::FiveHour],
        };
        assert_eq!(
            decision_json(&short).to_string(),
            r#"{"type":"none","reason":"planned-no-target","windows":["five_hour"]}"#
        );
        let in_place = Decision::WaitInPlace {
            reason: "reset-soon",
            at: Some(5),
        };
        assert_eq!(
            decision_json(&in_place).to_string(),
            r#"{"type":"wait-in-place","reason":"reset-soon","at":5}"#
        );
        let unknown = Decision::WaitInPlace {
            reason: "all-blocked-unknown",
            at: None,
        };
        assert_eq!(
            decision_json(&unknown).to_string(),
            r#"{"type":"wait-in-place","reason":"all-blocked-unknown"}"#
        );
        let hop = Decision::Hop {
            to: "b".into(),
            reason: "refusal",
            countdown: true,
            windows: Vec::new(),
        };
        assert_eq!(
            decision_json(&hop).to_string(),
            r#"{"type":"hop","to":"b","reason":"refusal","countdown":true}"#
        );
        let planned = Decision::Hop {
            to: "b".into(),
            reason: "planned",
            countdown: true,
            windows: vec![Window::SevenDay],
        };
        assert_eq!(
            decision_json(&planned).to_string(),
            r#"{"type":"hop","to":"b","reason":"planned","countdown":true,"windows":["seven_day"]}"#
        );
        let wait = Decision::Wait {
            to: "b".into(),
            at: 9,
            reason: "all-blocked-wait",
        };
        assert_eq!(
            decision_json(&wait).to_string(),
            r#"{"type":"wait","to":"b","at":9,"reason":"all-blocked-wait"}"#
        );
    }

    #[test]
    fn how_claude_ended_is_logged() {
        let keys = Ended {
            method: "keys",
            forced: false,
            rounds: Some(1),
            detail: vec!["ok".into()],
        };
        assert_eq!(
            ended_json(&keys).to_string(),
            r#"{"method":"keys","forced":false,"rounds":1,"detail":["ok"]}"#
        );
        let kill = Ended {
            method: "kill",
            forced: true,
            rounds: None,
            detail: Vec::new(),
        };
        assert_eq!(
            ended_json(&kill).to_string(),
            r#"{"method":"kill","forced":true}"#
        );
        assert_eq!(exit_json(None), Value::Null);
    }

    #[test]
    fn a_wrapper_starts_with_what_its_arguments_say() {
        let paths = baton_core::paths::baton_paths(baton_core::paths::resolve("home"));
        let sid = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";
        let named = Lead::new(LeadOptions::new(
            vec!["--resume".into(), sid.into()],
            paths.clone(),
            Env::new(),
        ));
        assert_eq!(named.session_id(), Some(sid));
        assert!(named.wrapper().starts_with(&format!("{}-", std::process::id())));
        assert_eq!(named.wrapper().len(), std::process::id().to_string().len() + 9);
        assert!(named.events().is_empty());
        // A name that is not a conversation id is Claude Code's to resolve.
        let by_name = Lead::new(LeadOptions::new(
            vec!["--resume".into(), "my work".into()],
            paths.clone(),
            Env::new(),
        ));
        assert_eq!(by_name.session_id(), None);
        let options = LeadOptions::new(Vec::new(), paths, Env::new());
        assert_eq!(
            (options.poll_ms, options.tick_ms, options.stdio, options.poll),
            (250, 1000, LeadStdio::Inherit, true)
        );
        assert_eq!((options.countdown_seconds, options.settle_ms), (None, None));
        assert_eq!(
            time_of(baton_core::clock::local_ms(2026, 3, 10, 15, 20).unwrap_or(0) + 7000),
            "15:20:07"
        );
    }
}
