//! One account's plan usage, read the documented way without credentials: a Claude Code session under
//! the account's profile, driven over stream-json the way the Agent SDK drives it, whose prompt is
//! "/usage".
//!
//! Claude Code answers a /usage command locally with a synthetic assistant message; from Claude Code
//! 2.1.273 that message carries usage_report, the structured twin of the text: rate_limits.limits[]
//! (the usage endpoint's meters: kind, group, percent, resets_at, scope, severity, is_active) and
//! extra_usage. rate_limits is null when the plan usage could not be fetched (the endpoint failing or
//! rate limited, a token without the profile scope), and usage_report is absent outside claude.ai
//! plans.
//!
//! The session is made to touch nothing: the installed claude, no settings sources (so no hooks, no
//! status line and no plugins: Baton's own hooks do not run), no tools, no MCP servers, one turn at
//! most, no session persistence, in a folder of its own (`~/.baton/probe`) whose transcripts, should
//! any appear, are deleted. It is ended after the timeout, its whole process tree with it.
//!
//! The wire, as Agent SDK 0.3.296 speaks it for these options: `claude --output-format stream-json
//! --verbose --input-format stream-json --max-turns=1 --tools= --setting-sources= --strict-mcp-config
//! --no-session-persistence`, with `CLAUDE_CODE_ENTRYPOINT=sdk-ts` and without `NODE_OPTIONS` and `DEBUG`; on
//! stdin an `initialize` control request, then, once it is answered, one user message; on stdout JSON
//! lines, of which the assistant message and the result matter. Once the result is in, stdin is closed
//! and claude leaves by itself.

use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use baton_core::claude_path::Launcher;
use baton_core::clock::{Ms, iso, ms_of, now_ms};
use baton_core::config::loose_f64;
use baton_core::env::Env;
use baton_core::fsutil::{lower_priority, random_hex, remove_tree};
use baton_core::json::Get;
use baton_core::paths::{Paths, profile_dir, profile_env};
use baton_core::sys::{ProcessTree, own_group};
use baton_core::usage::{ExtraUsage, Meter, Sample, is_weekly_meter, parse_time_after};
use baton_core::window::PerWindow;
use regex::Regex;
use serde_json::{Value, json};

use crate::proc::hide_window;
use crate::terminate::{force_kill, wait_exit};

/// A read is ended after this long, in milliseconds.
pub const PROBE_TIMEOUT_MS: u64 = 60_000;

/// What a Claude Code that started Baton leaves in the environment, and credentials or endpoints from
/// the environment that would answer for another account than the profile's sign-in (only the names
/// are looked at).
const DROPPED: [&str; 9] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_SSE_PORT",
    "BATON_WRAPPER",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
];

/// The command line of a read.
const PROBE_ARGS: [&str; 10] = [
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
    "--max-turns=1",
    "--tools=",
    "--setting-sources=",
    "--strict-mcp-config",
    "--no-session-persistence",
];

/// The environment a read runs with: the account's profile, and nothing that speaks for another
/// account or another session.
pub fn probe_env(paths: &Paths, id: &str, env: &Env) -> Env {
    let mut out = profile_env(paths, id, env);
    out.retain(|name| !DROPPED.iter().any(|dropped| name.eq_ignore_ascii_case(dropped)));
    out
}

/// The folder reads run in.
pub fn probe_dir(paths: &Paths) -> PathBuf {
    paths.root.join("probe")
}

/// Claude Code keeps a folder's transcripts in `<config>/projects/<the folder with every character
/// but letters and digits as "-">`; a long one is cut at 200 characters and given a hash.
pub fn project_dir_name(cwd: &str) -> String {
    let mut out = String::new();
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else {
            // One "-" per UTF-16 unit, as Claude Code counts them.
            out.push_str(&"-".repeat(c.len_utf16()));
        }
    }
    out
}

/// Deletes the transcripts a read left in the account's profile: only ever the probe's own folder.
/// Returns what was removed.
pub fn remove_probe_transcripts(paths: &Paths, id: &str, cwd: &Path) -> Vec<PathBuf> {
    let name = project_dir_name(&cwd.to_string_lossy()).to_lowercase();
    if !name.ends_with("-baton-probe") {
        return Vec::new();
    }
    let mut removed = Vec::new();
    // The profile's projects/ (a link into the shared base): only the probe's own folder in it.
    let base = profile_dir(paths, id).join("projects");
    let Ok(entries) = fs::read_dir(&base) else {
        return removed;
    };
    let cut: String = name.chars().take(200).collect();
    for entry in entries.flatten() {
        let lower = entry.file_name().to_string_lossy().to_lowercase();
        let ours = lower == name || (name.len() > 200 && lower.starts_with(&cut));
        let dir = base.join(entry.file_name());
        if !ours || !dir.exists() {
            continue;
        }
        if remove_tree(&dir).is_ok() {
            removed.push(dir);
        }
    }
    removed
}

/// Text on one line, at most `max` characters.
fn one_line(text: &str, max: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// A text field worth keeping: trimmed, not empty, at most `max` characters.
fn word(value: &Value, max: usize) -> Option<String> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then(|| text.chars().take(max).collect())
}

/// A number field: a number, or anything that reads as one; null is none.
fn number(value: &Value) -> Option<f64> {
    if value.is_null() {
        return None;
    }
    Some(loose_f64(value)).filter(|n| n.is_finite())
}

/// A meter of the usage endpoint as Baton keeps it.
fn meter(row: &Value) -> Option<Meter> {
    if !row.is_object() {
        return None;
    }
    let used = number(row.get("percent")?)?;
    let scope = row.at("scope");
    Some(Meter {
        kind: word(row.at("kind"), 64).unwrap_or_else(|| "unknown".to_owned()),
        group: word(row.at("group"), 64),
        label: word(scope.at("model").at("display_name"), 64)
            .or_else(|| word(scope.at("surface").at("display_name"), 64)),
        used,
        resets_at: ms_of(row.at("resets_at")).map(iso),
        severity: word(row.at("severity"), 32),
        active: row.flag("is_active"),
    })
}

/// The fullest of some meters: the first one when several are as full.
fn fullest<'a>(rows: impl Iterator<Item = &'a Meter>) -> Option<&'a Meter> {
    rows.fold(None, |best: Option<&Meter>, row| match best {
        Some(best) if row.used <= best.used => Some(best),
        _ => Some(row),
    })
}

/// What a read of one account's usage came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// The plan usage.
    Ok {
        /// The 5-hour window (the "session" meter) and the weekly one (all models).
        reading: PerWindow<Option<Sample>>,
        /// Every meter, a model's own weekly limit among them.
        limits: Vec<Meter>,
        /// The extra-usage spend.
        extra_usage: Option<ExtraUsage>,
    },
    /// The usage endpoint refused.
    RateLimited {
        /// The time it named, when it named one that is still ahead.
        until: Option<Ms>,
        /// What it said.
        reason: String,
    },
    /// The plan usage could not be fetched.
    Unavailable {
        /// Why.
        reason: String,
    },
    /// The profile is not signed in.
    SignedOut {
        /// What Claude Code said.
        reason: String,
    },
    /// No answer in time; claude was ended.
    Timeout {
        /// What happened.
        reason: String,
    },
    /// claude could not be run, or failed.
    Failed {
        /// Why.
        reason: String,
    },
    /// The read was called off.
    Stopped,
}

impl Outcome {
    /// The outcome in a word: ok, rate-limited, unavailable, signed-out, timeout, failed, stopped.
    pub fn status(&self) -> &'static str {
        match self {
            Outcome::Ok { .. } => "ok",
            Outcome::RateLimited { .. } => "rate-limited",
            Outcome::Unavailable { .. } => "unavailable",
            Outcome::SignedOut { .. } => "signed-out",
            Outcome::Timeout { .. } => "timeout",
            Outcome::Failed { .. } => "failed",
            Outcome::Stopped => "stopped",
        }
    }

    /// Why, for anything but ok.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Outcome::Ok { .. } => None,
            Outcome::RateLimited { reason, .. }
            | Outcome::Unavailable { reason }
            | Outcome::SignedOut { reason }
            | Outcome::Timeout { reason }
            | Outcome::Failed { reason } => Some(reason),
            Outcome::Stopped => Some("stopped"),
        }
    }
}

/// Whether the /usage text says the usage endpoint is rate limited.
fn says_rate_limited(text: &str) -> bool {
    Regex::new(r"(?i)rate.?limited").is_ok_and(|pattern| pattern.is_match(text))
}

/// Whether the /usage text says the profile is not signed in.
fn says_signed_out(text: &str) -> bool {
    Regex::new(r"(?i)not logged in|please run /login|/login\b").is_ok_and(|pattern| pattern.is_match(text))
}

/// usage_report (and the /usage text beside it) as an outcome. `report` is None when Claude Code sent
/// no usage_report at all. five_hour is the "session" meter, seven_day the all-models weekly one
/// ("weekly_all"); every meter is kept in limits (a model's own weekly limit among them), where the
/// policy finds the strictest weekly limit.
pub fn usage_from_report(report: Option<&Value>, text: &str, at: Ms) -> Outcome {
    let words = one_line(text, 200);
    let rate_limits = report.map(|report| report.at("rate_limits"));
    let Some(limits) = rate_limits.and_then(|limits| limits.at("limits").as_array()) else {
        if says_rate_limited(text) {
            return Outcome::RateLimited {
                until: parse_time_after(text, "until", at).filter(|until| *until > at),
                reason: if words.is_empty() {
                    "the usage endpoint is rate limited".to_owned()
                } else {
                    words
                },
            };
        }
        if says_signed_out(text) {
            return Outcome::SignedOut {
                reason: if words.is_empty() {
                    "not signed in".to_owned()
                } else {
                    words
                },
            };
        }
        let reason = match (report, words.is_empty()) {
            (None, true) => "Claude Code sent no usage report (not a claude.ai plan, or too old)".to_owned(),
            (None, false) => format!("no usage report ({words})"),
            (Some(_), true) => "the plan usage could not be fetched".to_owned(),
            (Some(_), false) => words,
        };
        return Outcome::Unavailable { reason };
    };
    let rows: Vec<Meter> = limits.iter().filter_map(meter).collect();
    let unscoped = |row: &&Meter| row.label.is_none();
    let five = fullest(rows.iter().filter(|r| r.kind == "session")).or_else(|| {
        fullest(
            rows.iter()
                .filter(|r| r.group.as_deref() == Some("session"))
                .filter(unscoped),
        )
    });
    let week = fullest(rows.iter().filter(|r| r.kind == "weekly_all"))
        .or_else(|| fullest(rows.iter().filter(|r| is_weekly_meter(r)).filter(unscoped)));
    let sample = |row: &Meter| Sample {
        used: row.used,
        resets_at: row.resets_at.clone(),
    };
    let reading = PerWindow {
        five_hour: five.map(sample),
        seven_day: week.map(sample),
    };
    let extra = rate_limits
        .map(|limits| limits.at("extra_usage"))
        .filter(|extra| extra.is_object());
    let extra_usage = extra.map(|extra| ExtraUsage {
        enabled: extra.flag("is_enabled"),
        monthly_limit: number(extra.at("monthly_limit")),
        used_credits: number(extra.at("used_credits")),
        utilization: number(extra.at("utilization")),
        currency: word(extra.at("currency"), 8),
    });
    Outcome::Ok {
        reading,
        limits: rows,
        extra_usage,
    }
}

/// The text of an assistant message.
fn text_of(message: &Value) -> String {
    let content = message.at("message").at("content");
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    blocks
        .iter()
        .filter(|block| block.text("type") == Some("text"))
        .filter_map(|block| block.text("text"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a session has for the reader.
#[derive(Clone, Debug, PartialEq)]
pub enum Next {
    /// A message.
    Message(Value),
    /// Nothing yet.
    Waiting,
    /// The session is over.
    Ended,
    /// The session failed.
    Failed(String),
}

/// A conversation with a Claude Code process: messages come out of it until it ends.
pub trait Session {
    /// The next message, waiting at most `timeout`.
    fn next(&mut self, timeout: Duration) -> Next;

    /// Ends the session at once, its process tree with it.
    fn abort(&mut self);

    /// Lets the session finish: its input is closed, and what does not leave by itself is ended.
    fn close(&mut self);

    /// What the process wrote to stderr, for a failure with no message of its own.
    fn stderr(&self) -> String {
        String::new()
    }
}

/// What a session is started with.
#[derive(Clone, Copy, Debug)]
pub struct QueryOptions<'a> {
    /// The folder it runs in.
    pub cwd: &'a Path,
    /// Its environment.
    pub env: &'a Env,
    /// How Claude Code is started.
    pub launcher: &'a Launcher,
    /// Whether it runs at a low priority.
    pub low: bool,
}

/// Starts a session whose prompt is "/usage"; an error when it cannot be started.
pub type Query<'a> = &'a dyn Fn(&QueryOptions<'_>) -> Result<Box<dyn Session>, String>;

/// A line of the process's stdout, or its end.
enum Line {
    /// One line.
    Text(String),
    /// stdout closed.
    End,
}

/// The real thing: a `claude` process spoken to over stream-json.
struct ClaudeSession {
    /// The process.
    child: Child,
    /// The process with everything it starts: ended together, and with this value at the latest.
    tree: ProcessTree,
    /// Its stdin, until the session is closed.
    stdin: Option<ChildStdin>,
    /// The lines of its stdout.
    lines: Receiver<Line>,
    /// The last of what it wrote to stderr.
    errors: Arc<Mutex<VecDeque<String>>>,
    /// The id of the initialize request.
    request_id: String,
    /// Whether stdout has closed.
    ended: bool,
}

impl ClaudeSession {
    /// Starts claude and asks it to initialize; the prompt follows once it has answered.
    fn start(options: &QueryOptions<'_>) -> Result<ClaudeSession, String> {
        let args: Vec<String> = PROBE_ARGS.iter().map(|arg| (*arg).to_owned()).collect();
        let mut command = options.launcher.command(&args);
        let mut env = options.env.clone();
        if !env.has("CLAUDE_CODE_ENTRYPOINT") {
            env.set("CLAUDE_CODE_ENTRYPOINT", "sdk-ts");
        }
        env.remove("NODE_OPTIONS").remove("DEBUG");
        env.apply(&mut command);
        command
            .current_dir(options.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_window(&mut command);
        // In a group of its own, so that a hang can end the whole tree.
        own_group(&mut command);
        let mut child = command
            .spawn()
            .map_err(|error| format!("spawn claude: {error}"))?;
        let tree = ProcessTree::background(child.id());
        if options.low {
            lower_priority(child.id());
        }
        let (tx, lines) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            thread::spawn(move || {
                let mut reader = BufReader::new(stdout);
                let mut bytes = Vec::new();
                loop {
                    bytes.clear();
                    match reader.read_until(b'\n', &mut bytes) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if tx
                                .send(Line::Text(String::from_utf8_lossy(&bytes).into_owned()))
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                let _ = tx.send(Line::End);
            });
        }
        let errors = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(mut stderr) = child.stderr.take() {
            let errors = Arc::clone(&errors);
            thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut kept) = errors.lock() {
                        kept.push_back(String::from_utf8_lossy(&chunk[..n]).into_owned());
                        if kept.len() > 20 {
                            kept.pop_front();
                        }
                    }
                }
            });
        }
        let mut session = ClaudeSession {
            stdin: child.stdin.take(),
            child,
            tree,
            lines,
            errors,
            request_id: random_hex(8),
            ended: false,
        };
        // Byte for byte what the SDK sends when it is given no system prompt of its own.
        let request = json!({
            "request_id": session.request_id,
            "type": "control_request",
            "request": { "subtype": "initialize", "systemPrompt": [""] },
        });
        session.send(&request);
        Ok(session)
    }

    /// Writes one message to the process. A process that is gone shows as the end of its output.
    fn send(&mut self, message: &Value) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(format!("{message}\n").as_bytes());
            let _ = stdin.flush();
        }
    }

    /// The end of the process's output: over, or failed when it exited with an error.
    fn finish(&mut self) -> Next {
        self.ended = true;
        self.stdin = None;
        if !wait_exit(&mut self.child, 2000) {
            return Next::Ended;
        }
        match self.child.try_wait() {
            Ok(Some(status)) if !status.success() => Next::Failed(match status.code() {
                Some(code) => format!("Claude Code process exited with code {code}"),
                None => "Claude Code process was ended by a signal".to_owned(),
            }),
            _ => Next::Ended,
        }
    }
}

impl Session for ClaudeSession {
    fn next(&mut self, timeout: Duration) -> Next {
        if self.ended {
            return Next::Ended;
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(left) {
                Ok(Line::Text(line)) => line,
                Ok(Line::End) | Err(RecvTimeoutError::Disconnected) => return self.finish(),
                Err(RecvTimeoutError::Timeout) => return Next::Waiting,
            };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match message.text("type") {
                Some("control_response") => {
                    let response = message.at("response");
                    if response.text("request_id") != Some(self.request_id.as_str()) {
                        continue;
                    }
                    if response.text("subtype") == Some("error") {
                        let why = response
                            .text("error")
                            .unwrap_or("Claude Code refused to initialize");
                        return Next::Failed(why.to_owned());
                    }
                    let prompt = json!({
                        "type": "user",
                        "session_id": "",
                        "message": { "role": "user", "content": [{ "type": "text", "text": "/usage" }] },
                        "parent_tool_use_id": null,
                    });
                    self.send(&prompt);
                }
                Some("control_request") => {
                    // Nothing is offered to this session: no tools, no hooks, no MCP servers.
                    let refusal = json!({
                        "type": "control_response",
                        "response": {
                            "subtype": "error",
                            "request_id": message.at("request_id"),
                            "error": "not supported",
                        },
                    });
                    self.send(&refusal);
                }
                _ => return Next::Message(message),
            }
        }
    }

    fn abort(&mut self) {
        force_kill(&mut self.child, Some(&self.tree));
    }

    fn close(&mut self) {
        self.stdin = None;
        if !wait_exit(&mut self.child, 5000) {
            force_kill(&mut self.child, Some(&self.tree));
            wait_exit(&mut self.child, 5000);
        }
    }

    fn stderr(&self) -> String {
        self.errors
            .lock()
            .map(|kept| kept.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// What a read needs.
#[derive(Clone, Copy)]
pub struct ProbeRequest<'a> {
    /// Baton's paths.
    pub paths: &'a Paths,
    /// The account's id.
    pub id: &'a str,
    /// The environment the read starts from.
    pub env: &'a Env,
    /// How Claude Code is started.
    pub launcher: &'a Launcher,
    /// The read is ended after this long, in milliseconds.
    pub timeout_ms: u64,
    /// The clock.
    pub now: &'a (dyn Fn() -> Ms + Send + Sync),
    /// Set to call the read off: its claude is ended at once.
    pub stop: Option<&'a AtomicBool>,
    /// Whether claude runs at a low priority.
    pub low: bool,
    /// How the session is started; None for a real `claude`.
    pub query: Option<Query<'a>>,
}

impl<'a> ProbeRequest<'a> {
    /// A read of account `id` with the usual timeout and the system clock.
    pub fn new(paths: &'a Paths, id: &'a str, env: &'a Env, launcher: &'a Launcher) -> ProbeRequest<'a> {
        ProbeRequest {
            paths,
            id,
            env,
            launcher,
            timeout_ms: PROBE_TIMEOUT_MS,
            now: &now_ms,
            stop: None,
            low: false,
            query: None,
        }
    }
}

impl std::fmt::Debug for ProbeRequest<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeRequest")
            .field("id", &self.id)
            .field("launcher", &self.launcher)
            .field("timeout_ms", &self.timeout_ms)
            .field("low", &self.low)
            .finish_non_exhaustive()
    }
}

/// How often a waiting read looks at its stop flag and its clock.
const SLICE: Duration = Duration::from_millis(50);

/// Reads one account's usage. Always an outcome, never an error: [`usage_from_report`]'s, or a
/// timeout, a failure or a stop.
pub fn read_plan_usage(request: &ProbeRequest<'_>) -> Outcome {
    let cwd = probe_dir(request.paths);
    let env = probe_env(request.paths, request.id, request.env);
    let mut session: Option<Box<dyn Session>> = None;
    let mut report: Option<Value> = None;
    let mut reply: Option<String> = None;
    let mut result: Option<Value> = None;
    let mut failure: Option<String> = None;
    let mut timed_out = false;
    let mut stopped = false;
    let options = QueryOptions {
        cwd: &cwd,
        env: &env,
        launcher: request.launcher,
        low: request.low,
    };
    let started = fs::create_dir_all(&cwd)
        .map_err(|error| error.to_string())
        .and_then(|()| match request.query {
            Some(query) => query(&options),
            None => ClaudeSession::start(&options).map(|session| Box::new(session) as Box<dyn Session>),
        });
    match started {
        Err(error) => failure = Some(error),
        Ok(running) => {
            let running = session.insert(running);
            let deadline = Instant::now() + Duration::from_millis(request.timeout_ms);
            loop {
                if request.stop.is_some_and(|stop| stop.load(Ordering::SeqCst)) {
                    stopped = true;
                    break;
                }
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    timed_out = true;
                    break;
                }
                match running.next(left.min(SLICE)) {
                    Next::Waiting => {}
                    Next::Ended => break,
                    Next::Failed(error) => {
                        failure = Some(error);
                        break;
                    }
                    Next::Message(message) => match message.text("type") {
                        Some("assistant") => {
                            if report.is_none() && message.get("usage_report").is_some() {
                                report = message.get("usage_report").cloned();
                                reply = Some(text_of(&message));
                            } else if reply.is_none() {
                                reply = Some(text_of(&message));
                            }
                        }
                        Some("result") => {
                            result = Some(message);
                            break;
                        }
                        _ => {}
                    },
                }
            }
        }
    }
    let mut stderr = String::new();
    if let Some(running) = session.as_mut() {
        if timed_out || stopped || failure.is_some() {
            running.abort();
        }
        running.close();
        stderr = running.stderr();
    }
    // The next read tries again.
    let _ = remove_probe_transcripts(request.paths, request.id, &cwd);
    if stopped {
        return Outcome::Stopped;
    }
    if timed_out {
        return Outcome::Timeout {
            reason: format!(
                "no answer in {} s; claude was ended",
                (request.timeout_ms + 500) / 1000
            ),
        };
    }
    if let (Some(failure), None) = (&failure, &report) {
        let said = [one_line(failure, 200), one_line(&stderr, 200)]
            .into_iter()
            .find(|text| !text.is_empty());
        return Outcome::Failed {
            reason: said.unwrap_or_else(|| "claude failed".to_owned()),
        };
    }
    let text = reply.unwrap_or_else(|| {
        result
            .as_ref()
            .and_then(|result| result.text("result"))
            .unwrap_or_default()
            .to_owned()
    });
    if let (None, Some(result)) = (&report, &result)
        && result.truthy("is_error")
        && text.is_empty()
    {
        let subtype = one_line(result.text("subtype").unwrap_or_default(), 200);
        return Outcome::Failed {
            reason: if subtype.is_empty() {
                "claude failed".to_owned()
            } else {
                subtype
            },
        };
    }
    usage_from_report(report.as_ref(), &text, (request.now)())
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::paths::baton_paths;

    #[test]
    fn the_probe_environment_speaks_for_the_profile_only() {
        let paths = baton_paths(baton_core::paths::resolve("home"));
        let env = Env::from_pairs([
            ("CLAUDE_CONFIG_DIR", "elsewhere"),
            ("ClaudeCode", "1"),
            ("ANTHROPIC_API_KEY", "k"),
            ("BATON_WRAPPER", "w1"),
            ("BATON_HOME", "h"),
            ("KEEP", "1"),
        ]);
        let out = probe_env(&paths, "work", &env);
        let mut names: Vec<&str> = out.names().collect();
        names.sort_unstable();
        assert_eq!(names, ["BATON_HOME", "CLAUDE_CONFIG_DIR", "KEEP"]);
        assert_eq!(
            out.get_os("CLAUDE_CONFIG_DIR"),
            Some(profile_dir(&paths, "work").as_os_str())
        );
    }

    #[test]
    fn a_folders_transcript_name() {
        assert_eq!(
            project_dir_name(r"C:\Users\op\.baton\probe"),
            "C--Users-op--baton-probe"
        );
        assert_eq!(project_dir_name("/home/op/.baton/probe"), "-home-op--baton-probe");
        assert_eq!(project_dir_name("a\u{1F600}b"), "a--b");
    }

    #[test]
    fn only_the_probes_own_transcripts_go() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let cwd = probe_dir(&paths);
        let projects = profile_dir(&paths, "work").join("projects");
        let ours = projects.join(project_dir_name(&cwd.to_string_lossy()));
        let other = projects.join("some-other-project");
        fs::create_dir_all(&ours).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(ours.join("x.jsonl"), "{}").unwrap();
        assert_eq!(remove_probe_transcripts(&paths, "work", &cwd), vec![ours.clone()]);
        assert!(!ours.exists() && other.exists());
        // A folder that is not the probe's is never touched.
        assert!(remove_probe_transcripts(&paths, "work", &other).is_empty());
        assert!(remove_probe_transcripts(&paths, "nobody", &cwd).is_empty());
    }

    #[test]
    fn the_text_of_a_message() {
        assert_eq!(text_of(&json!({ "message": { "content": "plain" } })), "plain");
        assert_eq!(
            text_of(&json!({ "message": { "content": [
                { "type": "text", "text": "a" }, { "type": "tool_use" }, { "type": "text", "text": "b" }
            ] } })),
            "a\nb"
        );
        assert_eq!(text_of(&json!({})), "");
        assert_eq!(one_line("  a \n b\t c ", 4), "a b ");
    }
}
