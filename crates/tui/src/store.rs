//! The menu's state with everything that changes it: actions go through the reducer, and the effects
//! the reducer asks for are carried out through the same core functions as the CLI.
//!
//! The store is owned by one thread. What takes time (asking Claude Code whether an account is signed
//! in, asking an account's usage) runs on a thread of its own and sends its answer back as a
//! [`Msg`]; whoever owns the store takes the messages and hands them to [`Store::handle`].

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use baton_claude::poller::PollResult;
use baton_core::clock::{Ms, local, now_ms, parse_ms};
use baton_core::config::{By, load_config, name_of};
use baton_core::env::Env;
use baton_core::format::poll_words;
use baton_core::history::History;
use baton_core::hop::{HopRequest, hop};
use baton_core::json::{Object, opt_num};
use baton_core::paths::Paths;
use baton_core::text::round;
use baton_core::theme::GlyphSet;
use baton_profile::accounts::{
    AccountChanges, AddOptions, add_account, move_account, remove_account, rename_account, set_account,
    set_policy,
};
use serde_json::json;

use crate::data::{MenuData, load_menu_data, load_menu_data_with};
use crate::layout::Size;
use crate::reducer::reduce_at;
use crate::state::{Action, Auth, Effect, Init, Op, State, initial_state};

/// The time now, in epoch milliseconds.
pub type Clock = Arc<dyn Fn() -> Ms + Send + Sync>;
/// Reads everything the menu shows, at a moment.
pub type LoadFn = Arc<dyn Fn(&Paths, Ms) -> MenuData + Send + Sync>;
/// Asks whether an account (by its id) is signed in. Runs on a thread of its own.
pub type AuthFn = Arc<dyn Fn(&str) -> Auth + Send + Sync>;
/// Asks one account's usage (by its id), or every account's; the outcomes, or why it could not ask.
/// Runs on a thread of its own.
pub type RefreshFn = Arc<dyn Fn(Option<&str>) -> Result<Vec<PollResult>, String> + Send + Sync>;

/// What reaches the thread that owns the store.
#[derive(Debug)]
pub enum Msg {
    /// An action: a key, a click, an answer about a sign-in.
    Action(Action),
    /// An account's usage (or every account's) was asked.
    Refreshed {
        /// Without a word about it: after a rename or a sign-in.
        quiet: bool,
        /// Whose usage, in words: "acct2's", "every account's".
        who: String,
        /// The outcomes, or why it could not be asked.
        outcome: Result<Vec<PollResult>, String>,
    },
    /// Nothing more will be typed: the input ended.
    InputEnded,
}

/// What the store asks of its surroundings.
pub trait Host {
    /// The operator asked to leave the menu.
    fn quit(&mut self) {}

    /// The whole screen is to be drawn again.
    fn redraw(&mut self) {}

    /// Runs Claude Code's sign-in for an account (by its id) and returns its exit code. The terminal
    /// is the sign-in's while it runs.
    fn sign_in(&mut self, _account: &str) -> i32 {
        1
    }
}

/// A host that does nothing: quitting, redrawing and signing in are left out.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHost;

impl Host for NoHost {}

/// What a store is made of. Everything but the paths and the environment has a working default.
#[derive(Clone)]
pub struct StoreOptions {
    /// Baton's paths.
    pub paths: Paths,
    /// The environment (for a hop request).
    pub env: Env,
    /// The terminal's size.
    pub size: Size,
    /// The clock.
    pub now: Clock,
    /// Whether colours are drawn.
    pub color: bool,
    /// Which glyph set is drawn.
    pub glyphs: GlyphSet,
    /// Whether the splash screen shows first.
    pub splash: bool,
    /// Reads what the menu shows.
    pub load: LoadFn,
    /// Asks an account's sign-in; None leaves every sign-in unknown.
    pub auth_of: Option<AuthFn>,
    /// Asks usage; None makes asking do nothing.
    pub refresh: Option<RefreshFn>,
}

impl StoreOptions {
    /// A store for these paths, with the defaults: 120 x 30, the real clock, colours, the rich glyphs,
    /// no splash, nothing asked of Claude Code.
    pub fn new(paths: Paths, env: Env) -> StoreOptions {
        StoreOptions {
            paths,
            env,
            size: Size { cols: 120, rows: 30 },
            now: Arc::new(now_ms),
            color: true,
            glyphs: GlyphSet::Rich,
            splash: false,
            load: kept_history_load(),
            auth_of: None,
            refresh: None,
        }
    }
}

/// The menu's own [`LoadFn`]: [`load_menu_data_with`] through one history kept from one look to the
/// next, which then reads only what was added to the history since.
fn kept_history_load() -> LoadFn {
    let history = Mutex::new(History::new());
    Arc::new(move |paths: &Paths, now: Ms| match history.lock() {
        Ok(mut history) => load_menu_data_with(paths, now, &mut history),
        Err(_) => load_menu_data(paths, now),
    })
}

impl std::fmt::Debug for StoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreOptions")
            .field("paths", &self.paths)
            .field("size", &self.size)
            .field("color", &self.color)
            .field("glyphs", &self.glyphs)
            .field("splash", &self.splash)
            .finish_non_exhaustive()
    }
}

/// A local time of day, `15:20:00`.
fn time_of(at: Ms) -> String {
    let at = local(at);
    format!("{:02}:{:02}:{:02}", at.hour, at.minute, at.second)
}

/// A refresh's outcomes as one toast.
pub fn refresh_summary(results: &[PollResult], now: Ms) -> String {
    if let [r] = results {
        if r.status == "ok" {
            let reading = r.reading.as_ref();
            let part =
                |label: &str, used: Option<f64>| used.map(|used| format!("{label} {}%", round(used) as i64));
            let parts: Vec<String> = [
                part(
                    "5-hour",
                    reading.and_then(|w| w.five_hour.as_ref()).map(|w| w.used),
                ),
                part(
                    "weekly",
                    reading.and_then(|w| w.seven_day.as_ref()).map(|w| w.used),
                ),
            ]
            .into_iter()
            .flatten()
            .collect();
            let said = if parts.is_empty() {
                "no meters reported".to_owned()
            } else {
                parts.join(" \u{b7} ")
            };
            return format!("{}: {said}.", r.name);
        }
        if r.status == "backoff" {
            let until = r
                .until
                .as_deref()
                .and_then(parse_ms)
                .map_or_else(|| "later".to_owned(), time_of);
            return format!(
                "{}: not asked; the usage endpoint asked to wait until {until}.",
                r.name
            );
        }
        return format!("{}: {}.", r.name, poll_words(Some(&r.entry()), now));
    }
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for r in results {
        let word = match r.status.as_str() {
            "ok" => "polled",
            "signed-out" => "signed out",
            "rate-limited" => "rate limited",
            "backoff" => "waiting out a rate limit",
            other => other,
        };
        match counts.iter_mut().find(|(seen, _)| *seen == word) {
            Some(entry) => entry.1 += 1,
            None => counts.push((word, 1)),
        }
    }
    if counts.is_empty() {
        return "Usage: no accounts.".to_owned();
    }
    let said: Vec<String> = counts.iter().map(|(word, n)| format!("{n} {word}")).collect();
    format!("Usage: {}.", said.join(" \u{b7} "))
}

/// What came of an operation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Done {
    /// What to say instead of the effect's own words.
    pub text: Option<String>,
    /// What follows it.
    pub then: Option<Effect>,
}

/// Carries out one of the menu's operations through the core functions the CLI uses. Accounts are
/// named by their id (the menu's rows), new ones by name. An error is why it was not done, in words
/// for the operator.
pub fn apply_op(paths: &Paths, env: &Env, now: Ms, op: &Op) -> Result<Done, String> {
    let failed = |error: baton_profile::accounts::AccountError| error.to_string();
    match op {
        Op::Mark { id, mark } => {
            let changes = AccountChanges {
                mark: Some(mark.as_str().to_owned()),
                ..AccountChanges::default()
            };
            set_account(paths, id, &changes, By::Id).map_err(failed)?;
        }
        Op::Move { id, position } => {
            move_account(paths, id, &json!(position), By::Id).map_err(failed)?;
        }
        Op::Thresholds {
            id,
            five_hour,
            weekly,
        } => {
            let changes = AccountChanges {
                five_hour: Some(opt_num(*five_hour)),
                weekly: Some(opt_num(*weekly)),
                ..AccountChanges::default()
            };
            set_account(paths, id, &changes, By::Id).map_err(failed)?;
        }
        Op::Rename { id, name } => {
            rename_account(paths, id, name, By::Id).map_err(failed)?;
            let then = Effect::Refresh {
                account: Some(id.clone()),
                quiet: true,
            };
            return Ok(Done {
                text: None,
                then: Some(then),
            });
        }
        Op::Add { name } => {
            let added = add_account(paths, name, &AddOptions::default()).map_err(failed)?;
            return Ok(Done {
                text: None,
                then: Some(Effect::SignIn {
                    account: added.account.id,
                }),
            });
        }
        Op::Remove { id } => {
            remove_account(paths, id, By::Id).map_err(failed)?;
        }
        Op::Policy { changes } => {
            let changes: Object = changes.iter().cloned().collect();
            set_policy(paths, &changes).map_err(failed)?;
        }
        Op::Hop { session, to } => {
            let request = HopRequest {
                session: Some(session),
                to: to.as_deref(),
                to_by: By::Id,
                env,
                by: "menu",
                now,
            };
            let moved = hop(paths, &request).map_err(|error| error.message)?;
            let short: String = moved.session.chars().take(8).collect();
            let when = if moved.between {
                "now"
            } else {
                "when its turn ends"
            };
            let text = format!("{short} moves from {} to {} {when}.", moved.from, moved.to);
            return Ok(Done {
                text: Some(text),
                then: None,
            });
        }
    }
    Ok(Done::default())
}

/// The menu's state and what changes it.
pub struct Store {
    /// The state.
    state: State,
    /// How often the state changed.
    version: u64,
    /// Whether the store was closed: nothing changes it any more.
    closed: bool,
    /// Baton's paths.
    paths: Paths,
    /// The environment.
    env: Env,
    /// The clock.
    now: Clock,
    /// Reads what the menu shows.
    load: LoadFn,
    /// Asks an account's sign-in.
    auth_of: Option<AuthFn>,
    /// Asks usage.
    refresh: Option<RefreshFn>,
    /// Where the workers send their answers.
    tx: Sender<Msg>,
    /// Where they arrive.
    rx: Receiver<Msg>,
    /// How many workers are still out.
    working: Arc<AtomicUsize>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("version", &self.version)
            .field("closed", &self.closed)
            .field("selected", &self.state.selected)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// A store. Nothing is read until it is told to (a tick reads ~/.baton).
    pub fn new(options: StoreOptions) -> Store {
        let (tx, rx) = mpsc::channel();
        let state = initial_state(Init {
            size: options.size,
            now: (options.now)(),
            color: options.color,
            glyphs: options.glyphs,
            data: None,
            splash: options.splash,
        });
        Store {
            state,
            version: 0,
            closed: false,
            paths: options.paths,
            env: options.env,
            now: options.now,
            load: options.load,
            auth_of: options.auth_of,
            refresh: options.refresh,
            tx,
            rx,
            working: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The state.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// A number that changes whenever the state does.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Where to send what the store is to be told from another thread (keys, clicks).
    pub fn sender(&self) -> Sender<Msg> {
        self.tx.clone()
    }

    /// The next message, waiting up to `timeout` for one.
    pub fn next(&self, timeout: Duration) -> Option<Msg> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Whether a worker is still out (a sign-in status or a usage poll being asked).
    pub fn working(&self) -> bool {
        self.working.load(Ordering::SeqCst) > 0
    }

    /// Stops the store: later actions and answers change nothing.
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// An action through the reducer, and what it asks for carried out.
    pub fn dispatch(&mut self, action: Action, host: &mut dyn Host) {
        if self.closed {
            return;
        }
        let out = reduce_at(&self.state, (self.now)(), action);
        if out.state != self.state {
            self.state = out.state;
            self.version += 1;
        }
        for effect in out.effects {
            self.perform(effect, host);
        }
    }

    /// A message from a worker or from the input. The end of the input is the caller's to act on.
    pub fn handle(&mut self, msg: Msg, host: &mut dyn Host) {
        match msg {
            Msg::Action(action) => self.dispatch(action, host),
            Msg::Refreshed { quiet: true, .. } => self.dispatch(Action::Tick, host),
            Msg::Refreshed { who, outcome, .. } => {
                let applied = match outcome {
                    Ok(results) => Action::Applied {
                        text: refresh_summary(&results, (self.now)()),
                        ok: true,
                    },
                    Err(error) => Action::Applied {
                        text: format!("Could not ask {who} usage: {error}"),
                        ok: false,
                    },
                };
                self.dispatch(applied, host);
            }
            Msg::InputEnded => {}
        }
    }

    /// Takes the workers' answers until none is out any more, or `timeout` is over. Whether every
    /// worker answered.
    pub fn settle(&mut self, host: &mut dyn Host, timeout: Duration) -> bool {
        let started = Instant::now();
        loop {
            while let Ok(msg) = self.rx.try_recv() {
                self.handle(msg, host);
            }
            if !self.working() {
                // An answer sent between the last look and the worker signing off.
                while let Ok(msg) = self.rx.try_recv() {
                    self.handle(msg, host);
                }
                if !self.working() {
                    return true;
                }
            }
            if started.elapsed() >= timeout {
                return false;
            }
            if let Some(msg) = self.next(Duration::from_millis(10)) {
                self.handle(msg, host);
            }
        }
    }

    /// Runs `work` on a thread of its own; what it returns arrives as a message.
    fn spawn(&self, work: impl FnOnce() -> Msg + Send + 'static) {
        let (tx, working) = (self.tx.clone(), Arc::clone(&self.working));
        working.fetch_add(1, Ordering::SeqCst);
        thread::spawn(move || {
            let _ = tx.send(work());
            working.fetch_sub(1, Ordering::SeqCst);
        });
    }

    /// Carries an effect out.
    pub fn perform(&mut self, effect: Effect, host: &mut dyn Host) {
        if self.closed {
            return;
        }
        match effect {
            Effect::Quit => host.quit(),
            Effect::Redraw => host.redraw(),
            Effect::Reload => {
                let data = (self.load)(&self.paths, (self.now)());
                self.dispatch(Action::Data(Arc::new(data)), host);
            }
            Effect::Auth { accounts } => {
                let Some(auth_of) = self.auth_of.clone() else {
                    return;
                };
                let ids = accounts.unwrap_or_else(|| {
                    load_config(&self.paths)
                        .accounts
                        .into_iter()
                        .map(|a| a.id)
                        .collect()
                });
                for account in ids {
                    let auth_of = Arc::clone(&auth_of);
                    self.spawn(move || {
                        let status = auth_of(&account);
                        Msg::Action(Action::Auth { account, status })
                    });
                }
            }
            Effect::Apply { op, done } => match apply_op(&self.paths, &self.env, (self.now)(), &op) {
                Ok(outcome) => {
                    let text = outcome.text.or(done).unwrap_or_else(|| "Done.".to_owned());
                    self.dispatch(Action::Applied { text, ok: true }, host);
                    if let Some(then) = outcome.then {
                        self.perform(then, host);
                    }
                }
                Err(text) => self.dispatch(Action::Applied { text, ok: false }, host),
            },
            // The usage poller: one account (its id) or all. quiet: after a rename or a sign-in.
            Effect::Refresh { account, quiet } => {
                let Some(refresh) = self.refresh.clone() else {
                    return;
                };
                let who = match &account {
                    Some(id) => format!("{}'s", name_of(&load_config(&self.paths), id)),
                    None => "every account's".to_owned(),
                };
                if !quiet {
                    self.dispatch(
                        Action::Applied {
                            text: format!("Asking {who} usage."),
                            ok: true,
                        },
                        host,
                    );
                }
                self.spawn(move || Msg::Refreshed {
                    quiet,
                    outcome: refresh(account.as_deref()),
                    who,
                });
            }
            Effect::SignIn { account } => {
                let name = name_of(&load_config(&self.paths), &account);
                self.dispatch(Action::Busy(Some(format!("signing in {name}"))), host);
                let code = host.sign_in(&account);
                self.dispatch(Action::Busy(None), host);
                let text = if code == 0 {
                    format!("{name}: Claude Code's sign-in finished.")
                } else {
                    format!("{name}: the sign-in did not finish (code {code}).")
                };
                self.dispatch(Action::Applied { text, ok: code == 0 }, host);
                self.perform(
                    Effect::Auth {
                        accounts: Some(vec![account.clone()]),
                    },
                    host,
                );
                if code == 0 {
                    self.perform(
                        Effect::Refresh {
                            account: Some(account),
                            quiet: true,
                        },
                        host,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::clock::iso;
    use baton_core::usage::{Reading, WindowReading};

    fn result(name: &str, status: &str) -> PollResult {
        PollResult {
            id: name.to_owned(),
            name: name.to_owned(),
            status: status.to_owned(),
            ..PollResult::default()
        }
    }

    fn window(used: f64) -> Option<WindowReading> {
        Some(WindowReading {
            used,
            resets_at: None,
            read_at: None,
            source: None,
            label: None,
        })
    }

    #[test]
    fn a_refresh_in_a_few_words() {
        let now = 1_780_000_000_000;
        let mut ok = result("acct2", "ok");
        ok.reading = Some(Reading {
            five_hour: window(41.6),
            seven_day: window(63.0),
            ..Reading::default()
        });
        assert_eq!(
            refresh_summary(&[ok.clone()], now),
            "acct2: 5-hour 42% \u{b7} weekly 63%."
        );
        ok.reading = Some(Reading {
            five_hour: window(42.0),
            ..Reading::default()
        });
        assert_eq!(refresh_summary(&[ok.clone()], now), "acct2: 5-hour 42%.");
        ok.reading = None;
        assert_eq!(refresh_summary(&[ok.clone()], now), "acct2: no meters reported.");
        let mut waiting = result("acct3", "backoff");
        waiting.until = Some(iso(baton_core::clock::local_ms(2026, 3, 10, 15, 14).unwrap_or(0)));
        assert_eq!(
            refresh_summary(&[waiting.clone()], now),
            "acct3: not asked; the usage endpoint asked to wait until 15:14:00."
        );
        let mut out = result("acct4", "signed-out");
        out.at = Some(iso(now));
        assert_eq!(
            refresh_summary(&[out.clone()], now),
            "acct4: signed out: not polled."
        );
        let mut failed = result("acct1", "timeout");
        failed.at = Some(iso(now - 5_000));
        assert_eq!(
            refresh_summary(&[failed.clone()], now),
            "acct1: poll timed out (just now)."
        );
        assert_eq!(
            refresh_summary(
                &[ok.clone(), ok, waiting, out, failed, result("x", "rate-limited")],
                now
            ),
            "Usage: 2 polled \u{b7} 1 waiting out a rate limit \u{b7} 1 signed out \u{b7} 1 timeout \u{b7} 1 rate limited."
        );
        assert_eq!(refresh_summary(&[], now), "Usage: no accounts.");
    }
}
