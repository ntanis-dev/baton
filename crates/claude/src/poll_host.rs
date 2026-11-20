//! The usage poll runs only while a baton instance the operator started runs (there is no background
//! service): the menu (`baton view`) and every `baton` wrapper carry a [`PollHost`], and exactly one of
//! them, the holder of the poll lease, polls on the schedule. No Baton process outlives the last
//! instance: when it exits, nothing polls.
//!
//! The lease (`~/.baton/poll/lease.json`) is renewed every heartbeat (3 s); a lease whose process is
//! gone, or not renewed for 10 s, is taken by another live instance (one at a time, through
//! `lease.json.take`). A poll asked for by a command (`~/.baton/poll/requests/`) is taken by the holder
//! at its next heartbeat; with no instance running, nothing polls.

use std::fs::{self, OpenOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use baton_core::claude_path::Launcher;
use baton_core::clock::{Ms, iso, now_ms};
use baton_core::env::Env;
use baton_core::fsutil::{is_alive, mtime_ms, read_json, remove_file, write_json_atomic};
use baton_core::json::Get;
use baton_core::paths::Paths;
use baton_core::poll::{
    HEARTBEAT_MS, LEASE_STALE_MS, Lease, lease_file, lease_live, poll_dir, read_lease, request_dir,
};
use uuid::Uuid;

use crate::poller::{Clock, LogFn, Poller, PollerOptions};

/// Whether a process is alive.
pub type AliveFn = Arc<dyn Fn(i64) -> bool + Send + Sync>;

/// The polling a host drives: the real [`Poller`], or a test's recorder.
pub trait PollWork: Send + Sync {
    /// Asks for a poll of one account (its id) or all; it runs in the background.
    fn refresh(&self, account: Option<&str>, reason: &str);

    /// Starts a scheduled poll of every account when one is due.
    fn tick(&self);

    /// Ends what runs and returns when nothing does.
    fn stop(&self);
}

impl PollWork for Poller {
    fn refresh(&self, account: Option<&str>, reason: &str) {
        // The outcomes are on disk; nobody waits for them here.
        drop(Poller::refresh(self, account, reason));
    }

    fn tick(&self) {
        drop(Poller::tick(self));
    }

    fn stop(&self) {
        Poller::stop(self);
    }
}

/// What a poll host is made of. Everything but the paths, the kind and the environment has a working
/// default.
#[derive(Clone)]
pub struct PollHostOptions {
    /// Baton's paths.
    pub paths: Paths,
    /// What the operator ran: `menu` or `start`.
    pub kind: String,
    /// The environment claude is started from.
    pub env: Env,
    /// How Claude Code is started; None lets the poller find it.
    pub launcher: Option<Launcher>,
    /// Claude Code's processes at a low priority.
    pub low: bool,
    /// The polling to drive; None for a [`Poller`] of its own.
    pub work: Option<Arc<dyn PollWork>>,
    /// The clock.
    pub now: Clock,
    /// Whether a process is alive.
    pub alive: AliveFn,
    /// This instance's process id.
    pub pid: i64,
    /// The time between two heartbeats, in milliseconds.
    pub heartbeat_ms: u64,
    /// A lease not renewed for this long is free, in milliseconds.
    pub stale_ms: Ms,
    /// Takes one line per polled account; None says nothing.
    pub log: Option<LogFn>,
}

impl PollHostOptions {
    /// A host for these paths, of this kind (`menu` or `start`), with the defaults.
    pub fn new(paths: Paths, kind: &str, env: Env) -> PollHostOptions {
        PollHostOptions {
            paths,
            kind: kind.to_owned(),
            env,
            launcher: None,
            low: false,
            work: None,
            now: Arc::new(now_ms),
            alive: Arc::new(is_alive),
            pid: i64::from(std::process::id()),
            heartbeat_ms: HEARTBEAT_MS as u64,
            stale_ms: LEASE_STALE_MS,
            log: None,
        }
    }
}

impl std::fmt::Debug for PollHostOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollHostOptions")
            .field("paths", &self.paths)
            .field("kind", &self.kind)
            .field("pid", &self.pid)
            .field("heartbeat_ms", &self.heartbeat_ms)
            .field("stale_ms", &self.stale_ms)
            .finish_non_exhaustive()
    }
}

/// What a host, its heartbeat thread and its poller share.
struct Core {
    /// Baton's paths.
    paths: Paths,
    /// What the operator ran.
    kind: String,
    /// This instance's own id.
    instance: String,
    /// This instance's process id.
    pid: i64,
    /// The clock.
    now: Clock,
    /// Whether a process is alive.
    alive: AliveFn,
    /// A lease not renewed for this long is free.
    stale_ms: Ms,
    /// Whether the host was stopped; its own poller looks at it too.
    stopped: Arc<AtomicBool>,
    /// Whether the last heartbeat held the lease.
    holding: AtomicBool,
    /// One heartbeat at a time.
    beating: Mutex<()>,
    /// The polling it drives.
    work: Arc<dyn PollWork>,
}

/// The heartbeat thread of a started host.
struct Timer {
    /// Dropped (or sent to) to end the thread.
    stop: Sender<()>,
    /// The thread.
    thread: JoinHandle<()>,
}

/// One baton instance's part in the usage poll.
pub struct PollHost {
    /// What it shares with its heartbeat thread.
    core: Arc<Core>,
    /// Its own poller, unless it was given the polling to drive.
    poller: Option<Arc<Poller>>,
    /// The time between two heartbeats.
    heartbeat: Duration,
    /// The heartbeat thread, once started.
    timer: Mutex<Option<Timer>>,
}

impl std::fmt::Debug for PollHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PollHost")
            .field("kind", &self.core.kind)
            .field("instance", &self.core.instance)
            .field("pid", &self.core.pid)
            .finish_non_exhaustive()
    }
}

/// Whether the lease on disk is this instance's.
fn lease_is(paths: &Paths, instance: &str) -> bool {
    read_lease(paths).is_some_and(|lease| lease.instance.as_deref() == Some(instance))
}

impl Core {
    /// Whether the lease is this instance's now.
    fn holds(&self) -> bool {
        !self.stopped.load(Ordering::SeqCst) && lease_is(&self.paths, &self.instance)
    }

    /// One heartbeat.
    fn beat(&self) -> bool {
        let Ok(_one) = self.beating.lock() else {
            return false;
        };
        if self.stopped.load(Ordering::SeqCst) {
            return false;
        }
        // A heartbeat never fails the instance.
        let holding = self.hold();
        self.holding.store(holding, Ordering::SeqCst);
        if !holding {
            return false;
        }
        for (account, reason) in self.take_requests() {
            self.work.refresh(Some(&account), &reason);
        }
        self.work.tick();
        true
    }

    /// Holds the lease: renews it, or takes one that is free or stale.
    fn hold(&self) -> bool {
        let file = lease_file(&self.paths);
        let now = (self.now)();
        let lease = read_lease(&self.paths);
        let record = |since: String| Lease {
            pid: Some(self.pid),
            kind: Some(self.kind.clone()),
            instance: Some(self.instance.clone()),
            since: Some(since),
            heartbeat_at: Some(iso(now)),
        };
        let mine = |lease: &Option<Lease>| {
            lease
                .as_ref()
                .is_some_and(|lease| lease.instance.as_deref() == Some(self.instance.as_str()))
        };
        if mine(&lease) {
            let since = lease.and_then(|lease| lease.since).unwrap_or_else(|| iso(now));
            return write_json_atomic(&file, &record(since)).is_ok();
        }
        if lease_live(lease.as_ref(), now, self.alive.as_ref(), self.stale_ms) {
            return false;
        }
        // Taking a free or stale lease: one instance at a time.
        if fs::create_dir_all(poll_dir(&self.paths)).is_err() {
            return false;
        }
        let mut guard = file.clone().into_os_string();
        guard.push(".take");
        let guard = std::path::PathBuf::from(guard);
        if OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&guard)
            .is_err()
        {
            // A guard left by an instance that died while taking the lease.
            if mtime_ms(&guard).is_some_and(|made| now_ms() - made > self.stale_ms) {
                let _ = remove_file(&guard);
            }
            return false;
        }
        let again = read_lease(&self.paths);
        let taken = if !mine(&again) && lease_live(again.as_ref(), now, self.alive.as_ref(), self.stale_ms) {
            false
        } else {
            write_json_atomic(&file, &record(iso(now))).is_ok()
        };
        let _ = remove_file(&guard);
        taken
    }

    /// The polls commands asked for, oldest first, one per account: (account, reason).
    fn take_requests(&self) -> Vec<(String, String)> {
        let dir = request_dir(&self.paths);
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.ends_with(".json"))
            .collect();
        names.sort();
        let mut out: Vec<(String, String)> = Vec::new();
        for name in names {
            let file = dir.join(name);
            let request = read_json(&file);
            let _ = remove_file(&file);
            let Some(account) = request.as_ref().and_then(|request| request.text("account")) else {
                continue;
            };
            // One poll per account.
            if out.iter().any(|(seen, _)| seen == account) {
                continue;
            }
            let reason = request
                .as_ref()
                .and_then(|request| request.text("reason"))
                .unwrap_or("request");
            out.push((account.to_owned(), reason.to_owned()));
        }
        out
    }
}

impl PollHost {
    /// A host. Nothing runs until it is started or made to beat.
    pub fn new(options: PollHostOptions) -> PollHost {
        let instance = Uuid::new_v4().to_string();
        let stopped = Arc::new(AtomicBool::new(false));
        let mut poller = None;
        let work: Arc<dyn PollWork> = match options.work {
            Some(work) => work,
            None => {
                // The poller asks, before each scheduled account, whether the lease is still ours.
                let (paths, own, flag) = (options.paths.clone(), instance.clone(), Arc::clone(&stopped));
                let own_poller = Arc::new(Poller::new(PollerOptions {
                    launcher: options.launcher.clone(),
                    low: options.low,
                    now: Arc::clone(&options.now),
                    log: options.log.clone(),
                    may: Some(Arc::new(move || {
                        !flag.load(Ordering::SeqCst) && lease_is(&paths, &own)
                    })),
                    ..PollerOptions::new(options.paths.clone(), options.env.clone())
                }));
                poller = Some(Arc::clone(&own_poller));
                own_poller
            }
        };
        let core = Arc::new(Core {
            paths: options.paths,
            kind: options.kind,
            instance,
            pid: options.pid,
            now: options.now,
            alive: options.alive,
            stale_ms: options.stale_ms,
            stopped,
            holding: AtomicBool::new(false),
            beating: Mutex::new(()),
            work,
        });
        PollHost {
            core,
            poller,
            heartbeat: Duration::from_millis(options.heartbeat_ms.max(1)),
            timer: Mutex::new(None),
        }
    }

    /// This instance's own id.
    pub fn instance(&self) -> &str {
        &self.core.instance
    }

    /// The host's own poller: the menu asks it for the polls the operator wants. None when the host
    /// was given the polling to drive.
    pub fn poller(&self) -> Option<Arc<Poller>> {
        self.poller.clone()
    }

    /// Whether the lease is this instance's now.
    pub fn holds(&self) -> bool {
        self.core.holds()
    }

    /// Whether the last heartbeat held the lease.
    pub fn holding(&self) -> bool {
        self.core.holding.load(Ordering::SeqCst)
    }

    /// Starts the heartbeat: one now, then one every `heartbeat_ms`.
    pub fn start(&self) {
        let Ok(mut timer) = self.timer.lock() else {
            return;
        };
        if timer.is_some() || self.core.stopped.load(Ordering::SeqCst) {
            return;
        }
        let (stop, stopping) = mpsc::channel::<()>();
        let (core, heartbeat) = (Arc::clone(&self.core), self.heartbeat);
        let thread = thread::spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stopping.recv_timeout(heartbeat) {
                core.beat();
            }
        });
        *timer = Some(Timer { stop, thread });
        drop(timer);
        self.core.beat();
    }

    /// Every heartbeat: hold (renew or take) the lease; the holder polls when a poll is due or asked
    /// for. The poll itself runs in the background: a heartbeat never waits for it. Whether this
    /// instance holds the lease.
    pub fn beat(&self) -> bool {
        self.core.beat()
    }

    /// Lets the lease go (another live instance takes it within a heartbeat) and ends a read in
    /// progress, its claude with it.
    pub fn stop(&self) {
        if self.core.stopped.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(timer) = self.timer.lock().ok().and_then(|mut timer| timer.take()) {
            drop(timer.stop);
            let _ = timer.thread.join();
        }
        {
            // A heartbeat in progress is over before the lease goes.
            let _beat = self.core.beating.lock();
        }
        if lease_is(&self.core.paths, &self.core.instance) {
            let _ = remove_file(&lease_file(&self.core.paths));
        }
        self.core.work.stop();
    }
}

impl Drop for PollHost {
    fn drop(&mut self) {
        self.stop();
    }
}
