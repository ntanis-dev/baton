//! The menu: `baton view`, in a terminal.
//!
//! It reads ~/.baton with the same core functions as the CLI, acts through them (the accounts, the
//! policy settings, hop requests), and follows changes live (a look at ~/.baton several times a
//! second, with the tick every `refresh_ms` as the fallback). State changes go through the reducer;
//! the screen is drawn on the alternate screen, rewriting only the rows that changed. A sign-in
//! suspends the menu: Claude Code's own `claude auth login` gets the terminal, then the menu comes
//! back.
//!
//! One thread owns the state and the terminal. The keys are read on a thread of their own, and so is
//! whatever takes time (asking a sign-in, asking usage); each sends what it has to the owner.

use std::io::{self, Read};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use baton_claude::auth::auth_status;
use baton_claude::lead::{ProcessSignals, Signals};
use baton_claude::poll_host::{PollHost, PollHostOptions};
use baton_claude::poller::{Poller, PollerOptions};
use baton_core::claude_path::{ClaudeNotFound, Launcher, resolve_claude};
use baton_core::clock::now_ms;
use baton_core::config::{load_config, name_of};
use baton_core::env::Env;
use baton_core::lines::Line;
use baton_core::paths::{Paths, profile_env};
use baton_core::term::color_depth;
use baton_core::theme::{GlyphSet, pick_glyphs};
use baton_profile::profile::{LinkOptions, link_profile};

use crate::console::{Console, StdConsole, restore_terminal};
use crate::layout::{Size, too_small};
use crate::paint::{Painter, SCREEN_OFF, SCREEN_ON, WRAP_OFF, WRAP_ON};
use crate::signin::{LoginStdio, SignIn, interrupts, sign_in_on};
use crate::state::{Action, Auth, Effect};
use crate::store::{AuthFn, Clock, Host, Msg, RefreshFn, Store, StoreOptions};
use crate::terminal::{ByteKeys, Keys, TerminalKeys};
use crate::ui::app::{layout, spins, turn};
use crate::ui::kit::theme_of;
use crate::watch::glance;

/// The time between two frames of a spinner.
const SPIN: Duration = Duration::from_millis(100);

/// What reports a panic.
type PanicHook = Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

/// What the menu is run with. Everything but the paths and the environment has a working default.
pub struct MenuOptions {
    /// Baton's paths.
    pub paths: Paths,
    /// The environment: `NO_COLOR`, `BATON_GLYPHS`, `BATON_SPLASH`, `BATON_MOUSE`, and what claude is
    /// started from.
    pub env: Env,
    /// The terminal to draw on; None for the process's own.
    pub console: Option<Box<dyn Console>>,
    /// The bytes a terminal sends, to read the keys from; None reads the process's own terminal.
    pub input: Option<Box<dyn Read + Send>>,
    /// How often ~/.baton is read again whatever happens, and the clock, the countdowns and the toasts
    /// move on: 2000 ms.
    pub refresh_ms: u64,
    /// How often ~/.baton is looked at for a change, which reads it again at once: 120 ms.
    pub watch_ms: u64,
    /// The clock; None for the real one.
    pub now: Option<Clock>,
    /// Whether colours are drawn; None for yes unless `NO_COLOR` is set.
    pub color: Option<bool>,
    /// Which glyph set is drawn; None picks by the environment.
    pub glyphs: Option<GlyphSet>,
    /// How Claude Code is started; None finds it (the config's path, `BATON_CLAUDE`, PATH).
    pub launcher: Option<Launcher>,
    /// What Claude Code's sign-in gets for its input.
    pub login_stdio: LoginStdio,
    /// Whether the menu owns the process: a panic then gives the terminal back before it is reported,
    /// and a request to end the process (Ctrl+Break, SIGTERM, SIGHUP) leaves the menu with that
    /// request's exit code (149, 143, 129), the terminal given back first.
    pub handle_signals: bool,
    /// The interrupts ignored while Claude Code's sign-in has the terminal (Ctrl+C is then Claude
    /// Code's to take); None for the process's own on its own terminal, and for nothing on a
    /// `console` that was handed in.
    pub signals: Option<Box<dyn Signals>>,
    /// Whether the splash screen shows first; None for yes unless `BATON_SPLASH` is `off`.
    pub splash: Option<bool>,
    /// Whether the mouse is used; None for yes unless `BATON_MOUSE` is `off`.
    pub mouse: Option<bool>,
    /// Whether the menu takes its turn at the usage poll while it runs (it is a baton instance); false
    /// leaves it out (tests).
    pub poll: bool,
    /// Asks usage, instead of the poller.
    pub refresh: Option<RefreshFn>,
}

impl MenuOptions {
    /// The menu on the process's terminal, with the defaults.
    pub fn new(paths: Paths, env: Env) -> MenuOptions {
        MenuOptions {
            paths,
            env,
            console: None,
            input: None,
            refresh_ms: 2000,
            watch_ms: 120,
            now: None,
            color: None,
            glyphs: None,
            launcher: None,
            login_stdio: LoginStdio::Inherit,
            handle_signals: false,
            signals: None,
            splash: None,
            mouse: None,
            poll: true,
            refresh: None,
        }
    }
}

impl std::fmt::Debug for MenuOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MenuOptions")
            .field("paths", &self.paths)
            .field("refresh_ms", &self.refresh_ms)
            .field("watch_ms", &self.watch_ms)
            .field("launcher", &self.launcher)
            .field("login_stdio", &self.login_stdio)
            .field("handle_signals", &self.handle_signals)
            .field("poll", &self.poll)
            .finish_non_exhaustive()
    }
}

/// How Claude Code is started: as the menu was told, else found.
fn claude(paths: &Paths, env: &Env, launcher: Option<&Launcher>) -> Result<Launcher, ClaudeNotFound> {
    match launcher {
        Some(launcher) => Ok(launcher.clone()),
        None => resolve_claude(load_config(paths).claude_path.as_deref(), env, Some(&paths.home)),
    }
}

/// The terminal side of the menu: what the store asks of its surroundings.
struct Shell {
    /// The terminal.
    console: Box<dyn Console>,
    /// Where the keys come from.
    keys: Option<Box<dyn Keys>>,
    /// What was last drawn.
    painter: Painter,
    /// Whether the terminal is one.
    tty: bool,
    /// Whether the mouse is used.
    mouse: bool,
    /// Whether colours are drawn.
    color: bool,
    /// Which glyph set is drawn.
    glyphs: GlyphSet,
    /// The terminal's colour depth, in bits.
    depth: u8,
    /// Baton's paths.
    paths: Paths,
    /// The environment.
    env: Env,
    /// How Claude Code is started, when the menu was told.
    launcher: Option<Launcher>,
    /// What Claude Code's sign-in gets for its input.
    login_stdio: LoginStdio,
    /// The process's interrupts.
    signals: Option<Box<dyn Signals>>,
    /// The operator asked to leave.
    quit: bool,
    /// The menu's screen is up.
    up: bool,
    /// The terminal reports the mouse and marks pastes.
    reporting: bool,
}

impl Shell {
    /// Puts the menu's screen up: raw keys, no line wrap, the alternate screen, no cursor.
    fn enter(&mut self) -> io::Result<()> {
        self.console.raw(true);
        self.up = true;
        self.painter.reset();
        if self.tty {
            self.console.write(&format!("{WRAP_OFF}{SCREEN_ON}"))?;
        }
        Ok(())
    }

    /// Gives the terminal back as it was.
    fn leave(&mut self) {
        if !self.up {
            return;
        }
        self.up = false;
        if self.tty {
            let _ = self.console.write(SCREEN_OFF);
            let _ = self.console.reports(self.mouse, false);
            let _ = self.console.write(WRAP_ON);
        }
        self.reporting = false;
        self.console.raw(false);
    }

    /// Sends a frame's changes, and after the first frame asks the terminal for its reports.
    fn show(&mut self, frame: &[Line], state: &crate::state::State) -> io::Result<()> {
        if !self.tty || !self.up {
            return Ok(());
        }
        let out = self.painter.paint(frame, &theme_of(state), self.depth);
        if !out.is_empty() {
            self.console.write(&out)?;
        }
        if !self.reporting {
            self.reporting = true;
            self.console.reports(self.mouse, true)?;
        }
        Ok(())
    }

    /// The terminal's size.
    fn size(&self) -> Size {
        let (cols, rows) = self.console.size().unwrap_or((80, 24));
        Size { cols, rows }
    }
}

impl Host for Shell {
    fn quit(&mut self) {
        self.quit = true;
    }

    fn redraw(&mut self) {
        self.painter.reset();
    }

    // Claude Code's own sign-in with the terminal: the menu steps aside (the main screen back, raw
    // keys off, its input let go) and returns.
    fn sign_in(&mut self, account: &str) -> i32 {
        // What linking could not do, baton doctor reports.
        let _ = link_profile(&self.paths, account, &LinkOptions::default());
        let Ok(launcher) = claude(&self.paths, &self.env, self.launcher.as_ref()) else {
            return 1;
        };
        if let Some(keys) = self.keys.as_mut() {
            keys.pause();
        }
        self.leave();
        let names = interrupts();
        if let Some(signals) = self.signals.as_mut() {
            signals.ignore(names);
        }
        let name = name_of(&load_config(&self.paths), account);
        let code = sign_in_on(
            self.console.as_mut(),
            &SignIn {
                launcher: &launcher,
                env: &profile_env(&self.paths, account, &self.env),
                name: &name,
                stdio: self.login_stdio,
                color: self.color,
                glyphs: self.glyphs,
                depth: self.depth,
            },
        );
        if let Some(signals) = self.signals.as_mut() {
            signals.restore(names);
        }
        if !self.quit {
            let _ = self.enter();
            if let Some(keys) = self.keys.as_mut() {
                keys.resume();
            }
        }
        code
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        if let Some(keys) = self.keys.as_mut() {
            keys.close();
        }
        self.leave();
    }
}

/// The poller the menu asks for the polls the operator wants: the poll host's own once it runs, else
/// one of the menu's own.
#[derive(Default)]
struct Usage {
    /// The poll host's poller.
    hosted: Option<Arc<Poller>>,
    /// The menu's own, made when first asked.
    own: Option<Arc<Poller>>,
}

/// Runs the menu until the operator leaves it. Returns its exit code, 0; an error when the terminal
/// could not be written to (it is given back as it was either way).
pub fn run_menu(options: MenuOptions) -> io::Result<i32> {
    let MenuOptions {
        paths,
        env,
        console,
        input,
        refresh_ms,
        watch_ms,
        now,
        color,
        glyphs,
        launcher,
        login_stdio,
        handle_signals,
        signals,
        splash,
        mouse,
        poll,
        refresh,
    } = options;
    let own_terminal = console.is_none();
    let signals = signals.or_else(|| own_terminal.then(|| Box::new(ProcessSignals) as Box<dyn Signals>));
    let console: Box<dyn Console> = console.unwrap_or_else(|| Box::new(StdConsole::new()));
    let tty = console.is_tty();
    let color = color.unwrap_or(!env.has("NO_COLOR"));
    let glyphs = glyphs.unwrap_or_else(|| pick_glyphs(&env));
    let splash = splash.unwrap_or(env.get("BATON_SPLASH") != Some("off"));
    let mouse = mouse.unwrap_or(env.get("BATON_MOUSE") != Some("off")) && tty;
    let now: Clock = now.unwrap_or_else(|| Arc::new(now_ms));
    let usage = Arc::new(Mutex::new(Usage::default()));
    let refresh: RefreshFn = refresh.unwrap_or_else(|| {
        let (usage, paths, env, launcher) =
            (Arc::clone(&usage), paths.clone(), env.clone(), launcher.clone());
        Arc::new(move |account| {
            let poller = {
                let mut usage = usage.lock().map_err(|_| "the poller is not there".to_owned())?;
                match usage.hosted.clone() {
                    Some(hosted) => hosted,
                    None => Arc::clone(usage.own.get_or_insert_with(|| {
                        Arc::new(Poller::new(PollerOptions {
                            launcher: launcher.clone(),
                            ..PollerOptions::new(paths.clone(), env.clone())
                        }))
                    })),
                }
            };
            Ok(poller.refresh_wait(account, "menu"))
        })
    });
    let auth_of: AuthFn = {
        let (paths, env, launcher) = (paths.clone(), env.clone(), launcher.clone());
        Arc::new(move |id| match claude(&paths, &env, launcher.as_ref()) {
            Ok(launcher) => {
                let status = auth_status(&launcher, &profile_env(&paths, id, &env));
                if status.ok {
                    Auth {
                        logged_in: Some(status.logged_in),
                        auth_method: status.auth_method,
                        error: None,
                    }
                } else {
                    Auth {
                        error: Some(status.error.unwrap_or_else(|| "no answer".to_owned())),
                        ..Auth::default()
                    }
                }
            }
            Err(error) => Auth {
                error: Some(error.0),
                ..Auth::default()
            },
        })
    };
    let mut shell = Shell {
        depth: if color { color_depth(&env) } else { 1 },
        console,
        keys: None,
        painter: Painter::new(),
        tty,
        mouse,
        color,
        glyphs,
        paths: paths.clone(),
        env: env.clone(),
        launcher: launcher.clone(),
        login_stdio,
        signals,
        quit: false,
        up: false,
        reporting: false,
    };
    let mut store = Store::new(StoreOptions {
        size: shell.size(),
        now: Arc::clone(&now),
        color,
        glyphs,
        splash: tty && splash,
        auth_of: Some(auth_of),
        refresh: Some(refresh),
        ..StoreOptions::new(paths.clone(), env.clone())
    });
    // A panic gives the terminal back before it is reported, so that the report can be read.
    let before: Option<PanicHook> = (handle_signals && own_terminal).then(|| {
        let before: PanicHook = Arc::from(std::panic::take_hook());
        let report = Arc::clone(&before);
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            report(info);
        }));
        before
    });
    // A request to end (Ctrl+Break, SIGTERM, SIGHUP) leaves the menu with that request's exit code, the
    // terminal given back first.
    let watch_ends = handle_signals && own_terminal;
    if watch_ends {
        baton_core::sys::watch_end_requests();
    }
    let mut ended: Option<i32> = None;
    let mut host: Option<PollHost> = None;
    let done = (|| -> io::Result<()> {
        shell.enter()?;
        shell.keys = Some(match input {
            Some(source) => Box::new(ByteKeys::start(source, store.sender(), mouse)) as Box<dyn Keys>,
            None => Box::new(TerminalKeys::start(store.sender(), mouse)),
        });
        store.dispatch(Action::Tick, &mut shell);
        store.perform(Effect::Auth { accounts: None }, &mut shell);
        let refresh_every = Duration::from_millis(refresh_ms.max(1));
        let watch_every = Duration::from_millis(watch_ms.max(1));
        let started = Instant::now();
        let (mut next_tick, mut next_look, mut next_spin) =
            (started + refresh_every, started + watch_every, started + SPIN);
        let mut seen = glance(&paths, store.state(), now());
        let mut rows: Vec<Line> = Vec::new();
        let mut drawn: Option<u64> = None;
        let mut tick = 0u64;
        while !shell.quit {
            if watch_ends && let Some(code) = baton_core::sys::end_requested() {
                ended = Some(code);
                break;
            }
            // The usage poll starts once the menu itself shows.
            if poll && host.is_none() && !store.state().splash && !too_small(store.state()) {
                // Without a claude of its own the poller finds one itself, or records why not.
                let own = claude(&paths, &env, launcher.as_ref()).ok();
                let polling = PollHost::new(PollHostOptions {
                    launcher: own,
                    ..PollHostOptions::new(paths.clone(), "menu", env.clone())
                });
                polling.start();
                if let Ok(mut usage) = usage.lock() {
                    usage.hosted = polling.poller();
                }
                host = Some(polling);
            }
            if drawn != Some(store.version()) {
                rows = layout(store.state());
                drawn = Some(store.version());
            }
            if spins(&rows) {
                shell.show(&turn(&rows, store.state(), tick), store.state())?;
            } else {
                shell.show(&rows, store.state())?;
            }
            let wait = next_tick
                .min(next_look)
                .min(next_spin)
                .saturating_duration_since(Instant::now());
            match store.next(wait) {
                Some(Msg::InputEnded) => break,
                Some(msg) => {
                    store.handle(msg, &mut shell);
                    // What else is there already, before drawing.
                    while let Some(msg) = store.next(Duration::ZERO) {
                        if matches!(msg, Msg::InputEnded) {
                            shell.quit = true;
                            break;
                        }
                        store.handle(msg, &mut shell);
                    }
                }
                None => {}
            }
            let size = shell.size();
            if size != store.state().size {
                store.dispatch(
                    Action::Resize {
                        cols: size.cols,
                        rows: size.rows,
                    },
                    &mut shell,
                );
            }
            let at = Instant::now();
            if at >= next_tick {
                next_tick = at + refresh_every;
                store.dispatch(Action::Tick, &mut shell);
            }
            if at >= next_look {
                next_look = at + watch_every;
                let look = glance(&paths, store.state(), now());
                if look != seen {
                    // What the tick reads may add a session or an account to look at: the next look
                    // then differs once more, and reads again.
                    seen = look;
                    store.dispatch(Action::Tick, &mut shell);
                }
            }
            if at >= next_spin {
                next_spin = at + SPIN;
                tick += 1;
            }
        }
        Ok(())
    })();
    store.close();
    // The terminal first, then the poll: its lease, and a read in progress.
    drop(shell);
    if let Some(before) = before {
        std::panic::set_hook(Box::new(move |info| before(info)));
    }
    if let Some(host) = host {
        host.stop();
    }
    if let Ok(mut usage) = usage.lock()
        && let Some(own) = usage.own.take()
    {
        own.stop();
    }
    done.map(|()| ended.unwrap_or(0))
}
