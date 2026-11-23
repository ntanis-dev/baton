//! Ending the `claude` process at the end of a turn, as gently as the platform allows, so it runs its
//! own shutdown (transcript flushed, MCP servers and background shells stopped, terminal restored,
//! SessionEnd hooks run):
//!
//! - Windows: Claude Code reads the console in raw mode, so Ctrl+C is a key to it, not a signal, and it
//!   exits on two Ctrl+C at its prompt. A console control event cannot be sent to one process
//!   (CTRL_BREAK would reach every process on the console, the operator's shell included). Baton types
//!   the keys: it is on that console itself, and writes two Ctrl+C key events into its input, exactly
//!   what the operator's keyboard would produce ([`baton_core::sys::type_console_keys`]). Up to three
//!   rounds (a first Ctrl+C may only clear typed text, close a dialog or cancel Claude Code's own limit
//!   wait); if claude is still running, its whole process tree is ended and Baton puts the terminal
//!   back in order.
//! - Elsewhere: SIGTERM (Claude Code shuts down gracefully on it), SIGKILL after a timeout, to claude
//!   and to what it started.
//! - "kill" (tests, or config endMethod): terminate at once.

use std::io::{self, IsTerminal, Write};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use baton_core::config::EndMethod;
use baton_core::sys::ProcessTree;

use crate::proc::{POLL, exited, hide_window};

/// How keys are typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyTiming {
    /// The pause between two keys, in milliseconds.
    pub delay_ms: u64,
}

impl Default for KeyTiming {
    fn default() -> KeyTiming {
        KeyTiming { delay_ms: 250 }
    }
}

/// Types keys (their character codes) into the console this process is on (Windows). Returns "ok" or
/// the failing step.
pub fn type_console_keys(codes: &[u32], timing: KeyTiming) -> String {
    baton_core::sys::type_console_keys(codes, timing.delay_ms)
}

/// Waits for a child to exit, at most `timeout_ms`. Whether it has.
pub fn wait_exit(child: &mut Child, timeout_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if exited(child) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(POLL);
    }
}

/// What a forcibly ended full-screen program may leave behind: alternate screen, hidden cursor, mouse
/// and focus reporting, bracketed paste, the kitty keyboard protocol, Windows Terminal's
/// win32-input-mode, raw input.
pub const TERMINAL_RESET: &str = "\x1b[?1049l\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[<u\x1b[?9001l\x1b[0m";

/// Puts this process's terminal back in order after a full-screen program was ended by force. Best
/// effort.
pub fn restore_terminal() {
    let mut output = io::stdout();
    let is_terminal = output.is_terminal();
    restore_terminal_on(&mut output, is_terminal, io::stdin().is_terminal());
}

/// [`restore_terminal`] on a given output: the reset sequence goes out only when the output is a
/// terminal, and raw input is switched off only when the input is one.
pub fn restore_terminal_on(output: &mut dyn Write, output_is_terminal: bool, input_is_terminal: bool) {
    if output_is_terminal {
        let _ = output.write_all(TERMINAL_RESET.as_bytes());
        let _ = output.flush();
    }
    if input_is_terminal {
        let _ = crossterm::terminal::enable_raw_mode();
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Types keys (their character codes) into this process's console; answers "ok" or the failing step.
pub type TypeKeys<'a> = &'a dyn Fn(&[u32]) -> String;

/// How `claude` is ended.
pub struct EndOptions<'a> {
    /// How to ask: auto is keys on Windows, a signal elsewhere.
    pub method: EndMethod,
    /// Claude with everything it started, for an end by force; None ends what can be found of it.
    pub tree: Option<&'a ProcessTree>,
    /// Types keys into the console; None for [`type_console_keys`].
    pub type_keys: Option<TypeKeys<'a>>,
    /// How many times the keys are typed before giving up.
    pub rounds: u32,
    /// How long claude gets to exit after each round, in milliseconds.
    pub round_wait_ms: u64,
    /// How long claude gets to exit after a signal or a kill, in milliseconds.
    pub signal_wait_ms: u64,
    /// Puts the terminal back in order after a forced end; None for [`restore_terminal`].
    pub restore: Option<&'a dyn Fn()>,
}

impl Default for EndOptions<'_> {
    fn default() -> Self {
        EndOptions {
            method: EndMethod::Auto,
            tree: None,
            type_keys: None,
            rounds: 3,
            round_wait_ms: 2000,
            signal_wait_ms: 5000,
            restore: None,
        }
    }
}

impl std::fmt::Debug for EndOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndOptions")
            .field("method", &self.method)
            .field("tree", &self.tree)
            .field("rounds", &self.rounds)
            .field("round_wait_ms", &self.round_wait_ms)
            .field("signal_wait_ms", &self.signal_wait_ms)
            .finish_non_exhaustive()
    }
}

/// How `claude` ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    /// What was done: `none` (it had exited already), `keys`, `signal` or `kill`.
    pub method: &'static str,
    /// Whether it had to be ended by force.
    pub forced: bool,
    /// For keys that worked: the round after which it exited.
    pub rounds: Option<u32>,
    /// For keys: what each round of typing answered.
    pub detail: Vec<String>,
}

/// Ends `claude`: asks the way `options.method` says, and ends its process tree when it does not
/// leave by itself.
pub fn end_claude(child: &mut Child, options: &EndOptions<'_>) -> Ended {
    let ended = |method, forced, rounds, detail| Ended {
        method,
        forced,
        rounds,
        detail,
    };
    if exited(child) {
        return ended("none", false, None, Vec::new());
    }
    let restore = || match options.restore {
        Some(restore) => restore(),
        None => restore_terminal(),
    };
    let chosen = match options.method {
        EndMethod::Auto if cfg!(windows) => EndMethod::Keys,
        EndMethod::Auto => EndMethod::Signal,
        other => other,
    };
    match chosen {
        EndMethod::Keys => {
            let mut detail = Vec::new();
            let mut round = 0;
            while round < options.rounds && !exited(child) {
                detail.push(match options.type_keys {
                    Some(type_keys) => type_keys(&[3, 3]),
                    None => type_console_keys(&[3, 3], KeyTiming::default()),
                });
                if wait_exit(child, options.round_wait_ms) {
                    return ended("keys", false, Some(round + 1), detail);
                }
                round += 1;
            }
            force_kill(child, options.tree);
            wait_exit(child, options.signal_wait_ms);
            restore();
            ended("keys", true, None, detail)
        }
        EndMethod::Signal => {
            if !(cfg!(unix) && baton_core::sys::terminate(child.id())) {
                let _ = child.kill();
            }
            if wait_exit(child, options.signal_wait_ms) {
                return ended("signal", false, None, Vec::new());
            }
            force_kill(child, options.tree);
            wait_exit(child, options.signal_wait_ms);
            restore();
            ended("signal", true, None, Vec::new())
        }
        _ => {
            force_kill(child, options.tree);
            wait_exit(child, options.signal_wait_ms);
            ended("kill", true, None, Vec::new())
        }
    }
}

/// Ends a child at once, with everything it started (claude's MCP servers and shells would outlive
/// it). `tree` is the child's tree when the caller made one at its start; without it, what Windows'
/// `taskkill` still finds of the tree is ended, and elsewhere the child alone.
pub fn force_kill(child: &mut Child, tree: Option<&ProcessTree>) {
    if exited(child) {
        return;
    }
    if tree.is_some_and(ProcessTree::kill) && wait_exit(child, 500) {
        return;
    }
    if cfg!(windows) {
        let mut taskkill = Command::new("taskkill");
        taskkill.args(["/PID", &child.id().to_string(), "/T", "/F"]);
        taskkill
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_window(&mut taskkill);
        let _ = taskkill.status();
        if exited(child) {
            return;
        }
    }
    let _ = child.kill();
}
