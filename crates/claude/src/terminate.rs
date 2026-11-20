//! Ending the `claude` process at the end of a turn, as gently as the platform allows, so it runs its
//! own shutdown (transcript flushed, MCP servers and background shells stopped, terminal restored,
//! SessionEnd hooks run):
//!
//! - Windows: Claude Code reads the console in raw mode, so Ctrl+C is a key to it, not a signal, and it
//!   exits on two Ctrl+C at its prompt. A console control event cannot be sent to one process
//!   (CTRL_BREAK would reach every process on the console, the operator's shell included). Baton types
//!   the keys: a short PowerShell helper with its own hidden console attaches to Baton's console and
//!   writes two Ctrl+C key events into its input buffer with WriteConsoleInput, exactly what the
//!   operator's keyboard would produce. Up to three rounds (a first Ctrl+C may only clear typed text,
//!   close a dialog or cancel Claude Code's own limit wait); if claude is still running, `taskkill /T
//!   /F` ends its process tree and Baton restores the terminal modes.
//! - Elsewhere: SIGTERM (Claude Code shuts down gracefully on it), SIGKILL after a timeout.
//! - "kill" (tests, or config endMethod): terminate at once.

use std::io::{self, IsTerminal, Write};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use baton_core::config::EndMethod;

use crate::proc::{POLL, exited, hide_window, run_captured};

/// The helper that types keys into a console. `__PID__`, `__CODES__` and `__DELAY__` are filled in.
const KEYS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Threading;
public static class BatonConsoleKeys {
  [StructLayout(LayoutKind.Explicit, CharSet = CharSet.Unicode)]
  public struct KEY_EVENT_RECORD {
    [FieldOffset(0)] public int bKeyDown;
    [FieldOffset(4)] public ushort wRepeatCount;
    [FieldOffset(6)] public ushort wVirtualKeyCode;
    [FieldOffset(8)] public ushort wVirtualScanCode;
    [FieldOffset(10)] public char UnicodeChar;
    [FieldOffset(12)] public uint dwControlKeyState;
  }
  [StructLayout(LayoutKind.Explicit)]
  public struct INPUT_RECORD {
    [FieldOffset(0)] public ushort EventType;
    [FieldOffset(4)] public KEY_EVENT_RECORD KeyEvent;
  }
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool FreeConsole();
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool AttachConsole(uint pid);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool SetConsoleCtrlHandler(IntPtr h, bool add);
  [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern IntPtr CreateFileW(string name, uint access, uint share, IntPtr sa, uint disp, uint flags, IntPtr tmpl);
  [DllImport("kernel32.dll", SetLastError = true)] static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
  static extern bool WriteConsoleInputW(IntPtr h, INPUT_RECORD[] recs, uint n, out uint written);
  public static string Send(uint pid, int[] codes, int delayMs) {
    FreeConsole();
    SetConsoleCtrlHandler(IntPtr.Zero, true);
    if (!AttachConsole(pid)) return "attach " + Marshal.GetLastWin32Error();
    IntPtr h = CreateFileW("CONIN$", 0xC0000000, 3, IntPtr.Zero, 3, 0, IntPtr.Zero);
    if (h == new IntPtr(-1)) { FreeConsole(); return "open " + Marshal.GetLastWin32Error(); }
    try {
      for (int i = 0; i < codes.Length; i++) {
        if (i > 0) Thread.Sleep(delayMs);
        char c = (char)codes[i];
        bool ctrl = c < ' ';
        var recs = new INPUT_RECORD[2];
        for (int d = 0; d < 2; d++) {
          recs[d].EventType = 1;
          recs[d].KeyEvent.bKeyDown = d == 0 ? 1 : 0;
          recs[d].KeyEvent.wRepeatCount = 1;
          recs[d].KeyEvent.wVirtualKeyCode = ctrl ? (ushort)(c + 0x40) : (ushort)char.ToUpperInvariant(c);
          recs[d].KeyEvent.UnicodeChar = c;
          recs[d].KeyEvent.dwControlKeyState = ctrl ? 0x0008u : 0u;
        }
        uint written;
        if (!WriteConsoleInputW(h, recs, 2, out written)) return "write " + Marshal.GetLastWin32Error();
      }
      return "ok";
    } finally { CloseHandle(h); FreeConsole(); }
  }
}
'@
[BatonConsoleKeys]::Send([uint32]__PID__, [int[]]@(__CODES__), __DELAY__)
"#;

/// A PowerShell script as `-EncodedCommand` takes it: base64 of its UTF-16LE text.
fn encode(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// How keys are typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyTiming {
    /// The pause between two keys, in milliseconds.
    pub delay_ms: u64,
    /// The helper is ended after this long, in milliseconds.
    pub timeout_ms: u64,
}

impl Default for KeyTiming {
    fn default() -> KeyTiming {
        KeyTiming {
            delay_ms: 250,
            timeout_ms: 20_000,
        }
    }
}

/// Types keys (their character codes) into the console of process `pid` (Windows). Returns "ok" or
/// the failing step.
pub fn type_console_keys(pid: u32, codes: &[u32], timing: KeyTiming) -> String {
    let codes: Vec<String> = codes.iter().map(u32::to_string).collect();
    let script = KEYS_SCRIPT
        .replacen("__PID__", &pid.to_string(), 1)
        .replacen("__CODES__", &codes.join(","), 1)
        .replacen("__DELAY__", &timing.delay_ms.to_string(), 1);
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &encode(&script),
    ]);
    // Hidden, with no inherited stdio: the helper gets a hidden console of its own, so PowerShell never
    // touches the operator's console until the helper attaches to it for the key write.
    let done = match run_captured(
        &mut command,
        Duration::from_millis(timing.timeout_ms),
        None,
        false,
    ) {
        Ok(done) => done,
        Err(error) => return format!("spawn {error}"),
    };
    // stderr carries PowerShell's progress records (CLIXML); the answer is on stdout.
    match done
        .stdout
        .trim()
        .lines()
        .next_back()
        .filter(|line| !line.is_empty())
    {
        Some(line) => line.trim_end_matches('\r').to_owned(),
        None => format!(
            "no output {}",
            done.stderr.trim().chars().take(200).collect::<String>()
        ),
    }
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

/// Types keys (their character codes) into the console of a process; answers "ok" or the failing step.
pub type TypeKeys<'a> = &'a dyn Fn(u32, &[u32]) -> String;

/// How `claude` is ended.
pub struct EndOptions<'a> {
    /// How to ask: auto is keys on Windows, a signal elsewhere.
    pub method: EndMethod,
    /// The process whose console the keys are typed into.
    pub console_pid: u32,
    /// Types keys into a console; None for [`type_console_keys`].
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
            console_pid: std::process::id(),
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
            .field("console_pid", &self.console_pid)
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
                    Some(type_keys) => type_keys(options.console_pid, &[3, 3]),
                    None => type_console_keys(options.console_pid, &[3, 3], KeyTiming::default()),
                });
                if wait_exit(child, options.round_wait_ms) {
                    return ended("keys", false, Some(round + 1), detail);
                }
                round += 1;
            }
            force_kill(child);
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
            force_kill(child);
            wait_exit(child, options.signal_wait_ms);
            restore();
            ended("signal", true, None, Vec::new())
        }
        _ => {
            force_kill(child);
            wait_exit(child, options.signal_wait_ms);
            ended("kill", true, None, Vec::new())
        }
    }
}

/// Ends a child at once: on Windows its whole process tree (claude's MCP servers and shells would
/// outlive it).
pub fn force_kill(child: &mut Child) {
    if exited(child) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helper_goes_out_as_utf16_base64() {
        assert_eq!(encode("hi"), "aABpAA==");
        assert!(
            KEYS_SCRIPT.contains("[BatonConsoleKeys]::Send([uint32]__PID__, [int[]]@(__CODES__), __DELAY__)")
        );
        assert!(KEYS_SCRIPT.starts_with("\n$ErrorActionPreference = 'Stop'\n"));
    }
}
