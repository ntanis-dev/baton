//! The usage poller: asks every signed-in account's plan usage (`probe`) and records it exactly like
//! the status line does (`usage/<id>.json` and the readings history), with source "poll".
//!
//! It runs only inside a running baton instance (the menu, a baton session wrapper; `poll_host` elects
//! one of them): when that instance starts if the last poll is older than usagePollMinutes, every
//! usagePollMinutes, after an account is added, signed in or renamed, and on request (the menu's r;
//! `baton usage --refresh` polls in its own process and exits). One account at a time, in the order of
//! the accounts.
//!
//! Requests wait their turn on one worker thread; whoever asked gets the outcomes on a channel, so the
//! menu's loop never blocks and the command line simply waits.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use baton_core::claude_path::{ClaudeNotFound, Launcher, resolve_claude};
use baton_core::clock::{HOUR, Ms, iso, now_ms, parse_ms};
use baton_core::config::{effective_policy, load_config, name_of};
use baton_core::env::Env;
use baton_core::json::{Object, opt_num};
use baton_core::paths::{Paths, profile_env};
use baton_core::poll::{
    BACKOFF_FIRST_MS, BACKOFF_MAX_MS, PollEntry, PollState, Running, poll_every_ms, read_poll_state,
    write_poll_state,
};
use baton_core::usage::{Reading, RecordOptions, record_reading};
use serde::{Serialize, Serializer};
use serde_json::{Value, json};

use crate::auth::{AuthStatus, auth_status_with};
use crate::probe::{Outcome, PROBE_TIMEOUT_MS, ProbeRequest, read_plan_usage};

/// A time the usage endpoint names is trusted this far ahead at most.
const UNTIL_MAX_MS: Ms = 24 * HOUR;

/// The clock.
pub type Clock = Arc<dyn Fn() -> Ms + Send + Sync>;
/// Reads one account's usage.
pub type ProbeFn = Arc<dyn Fn(&ProbeRequest<'_>) -> Outcome + Send + Sync>;
/// Whether an account (its id) is signed in, asked of this launcher; None when it cannot be told.
pub type AuthFn = Arc<dyn Fn(&str, &Launcher) -> Option<AuthStatus> + Send + Sync>;
/// Whether this process may still poll on the schedule.
pub type MayFn = Arc<dyn Fn() -> bool + Send + Sync>;
/// Takes one line per polled account.
pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

/// What a poller is made of. Everything but the paths and the environment has a working default.
#[derive(Clone)]
pub struct PollerOptions {
    /// Baton's paths.
    pub paths: Paths,
    /// The environment claude is started from.
    pub env: Env,
    /// The clock.
    pub now: Clock,
    /// Reads one account's usage; None for [`read_plan_usage`].
    pub probe: Option<ProbeFn>,
    /// Whether an account is signed in; None asks `claude auth status`.
    pub auth: Option<AuthFn>,
    /// How Claude Code is started; None finds it (the configured path, `BATON_CLAUDE`, PATH).
    pub launcher: Option<Launcher>,
    /// A read is ended after this long, in milliseconds.
    pub timeout_ms: u64,
    /// Takes one line per polled account; None says nothing.
    pub log: Option<LogFn>,
    /// For scheduled polls: whether this process may still poll (it holds the poll lease); asked
    /// before each account.
    pub may: Option<MayFn>,
    /// Claude Code's processes at a low priority (the baton session wrapper).
    pub low: bool,
}

impl PollerOptions {
    /// A poller for these paths and this environment, with the defaults.
    pub fn new(paths: Paths, env: Env) -> PollerOptions {
        PollerOptions {
            paths,
            env,
            now: Arc::new(now_ms),
            probe: None,
            auth: None,
            launcher: None,
            timeout_ms: PROBE_TIMEOUT_MS,
            log: None,
            may: None,
            low: false,
        }
    }
}

impl std::fmt::Debug for PollerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollerOptions")
            .field("paths", &self.paths)
            .field("launcher", &self.launcher)
            .field("timeout_ms", &self.timeout_ms)
            .field("low", &self.low)
            .finish_non_exhaustive()
    }
}

/// What came of asking one account.
///
/// As JSON it has the shape it always had: an account that was asked is `{ id, name, at, status, reason,
/// okAt, backoffUntil, backoffMs, failures, reading }`; one that was not is `{ id, name, status, reason }`
/// (unknown), `{ id, name, status, until, reason }` (backoff) or `{ id, name, status }` (stopped).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PollResult {
    /// The account's id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// ok, unavailable, rate-limited, timeout, failed, signed-out; or, for an account that was not
    /// asked: unknown (not an account), backoff (still waiting), stopped.
    pub status: String,
    /// Why, for anything but ok.
    pub reason: Option<String>,
    /// When it was asked.
    pub at: Option<String>,
    /// When it last answered.
    pub ok_at: Option<String>,
    /// Not asked again before this.
    pub backoff_until: Option<String>,
    /// The current wait, when the endpoint named no time.
    pub backoff_ms: Option<f64>,
    /// Failures in a row.
    pub failures: u32,
    /// For backoff: until when the account is not asked.
    pub until: Option<String>,
    /// The reading as recorded, for ok.
    pub reading: Option<Reading>,
}

impl PollResult {
    /// The poller's word on the account, as the poll's state keeps it (for `poll_words`).
    pub fn entry(&self) -> PollEntry {
        PollEntry {
            at: self.at.clone(),
            status: Some(self.status.clone()),
            reason: self.reason.clone(),
            ok_at: self.ok_at.clone(),
            backoff_until: self.backoff_until.clone(),
            backoff_ms: self.backoff_ms,
            failures: Some(self.failures),
        }
    }
}

impl Serialize for PollResult {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = Object::new();
        let mut put = |key: &str, value: Value| {
            out.insert(key.to_owned(), value);
        };
        put("id", json!(self.id));
        put("name", json!(self.name));
        match self.status.as_str() {
            "unknown" => {
                put("status", json!(self.status));
                put("reason", json!(self.reason));
            }
            "backoff" => {
                put("status", json!(self.status));
                put("until", json!(self.until));
                put("reason", json!(self.reason));
            }
            "stopped" => put("status", json!(self.status)),
            _ => {
                put("at", json!(self.at));
                put("status", json!(self.status));
                put("reason", json!(self.reason));
                put("okAt", json!(self.ok_at));
                put("backoffUntil", json!(self.backoff_until));
                put("backoffMs", opt_num(self.backoff_ms));
                put("failures", json!(self.failures));
                put("reading", json!(self.reading));
            }
        }
        Value::Object(out).serialize(serializer)
    }
}

/// One request to the worker.
struct Job {
    /// The account to ask, or None for all.
    account: Option<String>,
    /// Why: schedule, request, a command's reason.
    reason: String,
    /// Where the outcomes go.
    reply: Sender<Vec<PollResult>>,
}

/// What the poller and its worker share.
struct Inner {
    /// What it is made of.
    options: PollerOptions,
    /// Requests queued or running.
    pending: Mutex<usize>,
    /// Signalled when `pending` drops.
    idle: Condvar,
    /// Whether the poller was stopped.
    stopped: AtomicBool,
    /// Calls a read in progress off.
    stop: AtomicBool,
}

/// The usage poller.
pub struct Poller {
    /// What it shares with its worker.
    inner: Arc<Inner>,
    /// Where requests go; the worker starts with the first one.
    jobs: Mutex<Option<Sender<Job>>>,
}

impl std::fmt::Debug for Poller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Poller")
            .field("options", &self.inner.options)
            .finish_non_exhaustive()
    }
}

impl Poller {
    /// A poller. Nothing runs until it is asked.
    pub fn new(options: PollerOptions) -> Poller {
        Poller {
            inner: Arc::new(Inner {
                options,
                pending: Mutex::new(0),
                idle: Condvar::new(),
                stopped: AtomicBool::new(false),
                stop: AtomicBool::new(false),
            }),
            jobs: Mutex::new(None),
        }
    }

    /// The poll's state, as it is on disk.
    pub fn state(&self) -> PollState {
        read_poll_state(&self.inner.options.paths)
    }

    /// How many requests are queued or running.
    pub fn pending(&self) -> usize {
        self.inner.pending.lock().map(|pending| *pending).unwrap_or(0)
    }

    /// Stops: a read in progress ends at once (its claude with it); returns once nothing runs any
    /// more.
    pub fn stop(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.stop.store(true, Ordering::SeqCst);
        let Ok(mut pending) = self.inner.pending.lock() else {
            return;
        };
        while *pending > 0 {
            match self.inner.idle.wait(pending) {
                Ok(guard) => pending = guard,
                Err(_) => return,
            }
        }
    }

    /// A scheduled poll of every account when one is due (and none is queued). None when none is.
    pub fn tick(&self) -> Option<Receiver<Vec<PollResult>>> {
        if self.inner.stopped.load(Ordering::SeqCst) || self.pending() > 0 {
            return None;
        }
        let options = &self.inner.options;
        let policy = effective_policy(&load_config(&options.paths));
        let every = poll_every_ms(&policy)?;
        let last = self.state().last_run_at.as_deref().and_then(parse_ms);
        if last.is_some_and(|last| (options.now)() - last < every) {
            return None;
        }
        Some(self.refresh(None, "schedule"))
    }

    /// Polls one account (its id) or all; the outcomes, one per account, arrive on the channel once
    /// they are done. Requests wait their turn.
    pub fn refresh(&self, account: Option<&str>, reason: &str) -> Receiver<Vec<PollResult>> {
        let (reply, results) = mpsc::channel();
        let job = Job {
            account: account.map(str::to_owned),
            reason: reason.to_owned(),
            reply,
        };
        if let Ok(mut pending) = self.inner.pending.lock() {
            *pending += 1;
        }
        let queued = self.jobs.lock().is_ok_and(|mut jobs| {
            let sender = jobs.get_or_insert_with(|| {
                let (tx, rx) = mpsc::channel::<Job>();
                let inner = Arc::clone(&self.inner);
                thread::spawn(move || {
                    for job in rx {
                        let results = inner.run(job.account.as_deref(), &job.reason);
                        // Whoever asked may have stopped waiting.
                        let _ = job.reply.send(results);
                        inner.done();
                    }
                });
                tx
            });
            sender.send(job).is_ok()
        });
        if !queued {
            self.inner.done();
        }
        results
    }

    /// Polls one account or all and waits for the outcomes.
    pub fn refresh_wait(&self, account: Option<&str>, reason: &str) -> Vec<PollResult> {
        self.refresh(account, reason).recv().unwrap_or_default()
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        // Nobody is left to ask: a read in progress ends, its claude with it.
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.stop.store(true, Ordering::SeqCst);
    }
}

impl Inner {
    /// One request less to wait for.
    fn done(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            *pending = pending.saturating_sub(1);
        }
        self.idle.notify_all();
    }

    /// Changes the poll's state on disk.
    fn save(&self, change: impl FnOnce(&mut PollState)) {
        let mut state = read_poll_state(&self.options.paths);
        change(&mut state);
        let _ = write_poll_state(&self.options.paths, &state);
    }

    /// The time now.
    fn now(&self) -> Ms {
        (self.options.now)()
    }

    /// Runs one request: every account in order, or the one named.
    fn run(&self, account: Option<&str>, reason: &str) -> Vec<PollResult> {
        let config = load_config(&self.options.paths);
        let ids: Vec<String> = match account {
            Some(account) => vec![account.to_owned()],
            None => config.accounts.iter().map(|a| a.id.clone()).collect(),
        };
        if account.is_none() {
            let at = iso(self.now());
            self.save(|state| state.last_run_at = Some(at));
        }
        let mut results = Vec::new();
        for id in ids {
            if self.stopped.load(Ordering::SeqCst) {
                break;
            }
            if reason == "schedule" && self.options.may.as_ref().is_some_and(|may| !may()) {
                break;
            }
            results.push(self.poll_one(&id, reason));
        }
        self.save(|state| state.running = None);
        results
    }

    /// How Claude Code is started.
    fn launcher(&self) -> Result<Launcher, ClaudeNotFound> {
        if let Some(launcher) = &self.options.launcher {
            return Ok(launcher.clone());
        }
        let configured = load_config(&self.options.paths).claude_path;
        resolve_claude(
            configured.as_deref(),
            &self.options.env,
            Some(&self.options.paths.home),
        )
    }

    /// Whether the account is signed in; None when it cannot be told.
    fn signed_in(&self, id: &str, launcher: &Launcher) -> Option<AuthStatus> {
        if let Some(auth) = &self.options.auth {
            return auth(id, launcher);
        }
        let env = profile_env(&self.options.paths, id, &self.options.env);
        Some(auth_status_with(
            launcher,
            &env,
            self.options.low,
            Some(&self.stop),
        ))
    }

    /// Asks one account.
    fn poll_one(&self, id: &str, reason: &str) -> PollResult {
        let options = &self.options;
        let config = load_config(&options.paths);
        let name = name_of(&config, id);
        let result = |status: &str| PollResult {
            id: id.to_owned(),
            name: name.clone(),
            status: status.to_owned(),
            ..PollResult::default()
        };
        if !config.accounts.iter().any(|a| a.id == id) {
            return PollResult {
                reason: Some("not one of Baton's accounts".to_owned()),
                ..result("unknown")
            };
        }
        let at = self.now();
        let before = read_poll_state(&options.paths)
            .accounts
            .remove(id)
            .unwrap_or_default();
        if before
            .backoff_until
            .as_deref()
            .and_then(parse_ms)
            .is_some_and(|until| until > at)
        {
            return PollResult {
                until: before.backoff_until,
                reason: before.reason,
                ..result("backoff")
            };
        }
        let running = Running {
            account: id.to_owned(),
            since: Some(iso(at)),
            reason: Some(reason.to_owned()),
        };
        self.save(|state| state.running = Some(running));
        let launcher = match self.launcher() {
            Ok(launcher) => launcher,
            Err(error) => return self.record(id, &name, &Outcome::Failed { reason: error.0 }, at),
        };
        if self
            .signed_in(id, &launcher)
            .is_some_and(|auth| auth.signed_out())
        {
            let reason = format!("not signed in: baton login {name}");
            return self.record(id, &name, &Outcome::SignedOut { reason }, at);
        }
        let request = ProbeRequest {
            paths: &options.paths,
            id,
            env: &options.env,
            launcher: &launcher,
            timeout_ms: options.timeout_ms,
            now: options.now.as_ref(),
            stop: Some(&self.stop),
            low: options.low,
            query: None,
        };
        let outcome = match &options.probe {
            Some(probe) => probe(&request),
            None => read_plan_usage(&request),
        };
        if self.stopped.load(Ordering::SeqCst) {
            return result("stopped");
        }
        self.record(id, &name, &outcome, self.now())
    }

    /// Records what came of asking one account: the reading, and the poller's word on the account.
    fn record(&self, id: &str, name: &str, outcome: &Outcome, at: Ms) -> PollResult {
        let options = &self.options;
        let before = read_poll_state(&options.paths)
            .accounts
            .remove(id)
            .unwrap_or_default();
        let mut entry = PollEntry {
            at: Some(iso(at)),
            status: Some(outcome.status().to_owned()),
            reason: outcome.reason().map(str::to_owned),
            ok_at: before.ok_at,
            backoff_until: None,
            backoff_ms: None,
            failures: Some(0),
        };
        let mut reading = None;
        match outcome {
            Outcome::Ok {
                reading: windows,
                limits,
                extra_usage,
            } => {
                let record = RecordOptions {
                    source: "poll".to_owned(),
                    refresh_ms: 0,
                    limits: Some(Some(limits.clone())),
                    extra_usage: Some(extra_usage.clone()),
                    ..RecordOptions::at(at)
                };
                reading = record_reading(&options.paths, id, windows, record).reading;
                entry.ok_at = Some(iso(at));
            }
            Outcome::SignedOut { .. } => {}
            other => {
                entry.failures = Some(before.failures.unwrap_or(0) + 1);
                if let Outcome::RateLimited { until, .. } = other {
                    match until.filter(|until| *until != 0) {
                        Some(until) => entry.backoff_until = Some(iso(until.min(at + UNTIL_MAX_MS))),
                        None => {
                            let wait = match before.backoff_ms.filter(|ms| *ms != 0.0) {
                                Some(ms) => ms * 2.0,
                                None => BACKOFF_FIRST_MS as f64,
                            }
                            .min(BACKOFF_MAX_MS as f64);
                            entry.backoff_ms = Some(wait);
                            entry.backoff_until = Some(iso(at + wait as Ms));
                        }
                    }
                }
            }
        }
        let stored = entry.clone();
        self.save(|state| {
            state.accounts.insert(id.to_owned(), stored);
        });
        if let Some(log) = &options.log {
            let status = entry.status.as_deref().unwrap_or_default();
            let why = entry
                .reason
                .as_deref()
                .filter(|r| !r.is_empty())
                .map(|r| format!(" ({r})"));
            let next = entry
                .backoff_until
                .as_deref()
                .map(|until| format!("; next ask after {until}"));
            log(&format!(
                "{name}: {status}{}{}",
                why.unwrap_or_default(),
                next.unwrap_or_default()
            ));
        }
        PollResult {
            id: id.to_owned(),
            name: name.to_owned(),
            status: entry.status.unwrap_or_default(),
            reason: entry.reason,
            at: entry.at,
            ok_at: entry.ok_at,
            backoff_until: entry.backoff_until,
            backoff_ms: entry.backoff_ms,
            failures: entry.failures.unwrap_or(0),
            until: None,
            reading,
        }
    }
}
