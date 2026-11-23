//! A one-line prompt that redraws itself and waits for a key: Enter is "go", Ctrl+C is "quit", any
//! other key is "stay", or whatever the tick decides. Keys in the first `grace_ms` are ignored
//! (leftovers of the keys typed to end claude). Input and output are injected so tests drive it.

use std::io::{IsTerminal, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use baton_core::clock::{Ms, now_ms};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

/// Where the prompt's keys come from.
pub trait KeyInput {
    /// Called once before the prompt reads: a console goes into raw mode.
    fn begin(&mut self) {}

    /// Called once when the prompt is over: a console leaves raw mode.
    fn end(&mut self) {}

    /// What was typed, waiting at most `timeout`; None when nothing was.
    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>>;
}

/// The operator's console. When stdin is not a console no key is ever read: a blocked read of a pipe
/// could not be taken back, and would take input meant for the next `claude`.
#[derive(Debug, Default)]
pub struct ConsoleKeys {
    /// Whether this value switched raw mode on.
    raw: bool,
}

impl ConsoleKeys {
    /// The console of this process.
    pub fn new() -> ConsoleKeys {
        ConsoleKeys::default()
    }
}

/// A key press as the bytes a terminal would send for it.
fn key_bytes(code: KeyCode, modifiers: KeyModifiers) -> Vec<u8> {
    match code {
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() => {
            vec![(c.to_ascii_lowercase() as u8) & 0x1f]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x7f],
        _ => vec![0x1b, b'['],
    }
}

impl KeyInput for ConsoleKeys {
    fn begin(&mut self) {
        if std::io::stdin().is_terminal() {
            // Not a console after all: the prompt runs without keys.
            self.raw = crossterm::terminal::enable_raw_mode().is_ok();
        }
    }

    fn end(&mut self) {
        if self.raw {
            // The console may have gone away.
            let _ = crossterm::terminal::disable_raw_mode();
            self.raw = false;
        }
    }

    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        if !self.raw {
            thread::sleep(timeout);
            return None;
        }
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if !event::poll(left).unwrap_or(false) {
                return None;
            }
            match event::read() {
                Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                    return Some(key_bytes(key.code, key.modifiers));
                }
                Ok(Event::Paste(text)) => return Some(text.into_bytes()),
                Ok(_) => {}
                Err(_) => return None,
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }
}

/// Keys that arrive on a channel: what the tests type, or another thread forwards.
#[derive(Debug)]
pub struct ChannelKeys {
    /// Where the keys arrive.
    keys: Receiver<Vec<u8>>,
}

/// A channel of keys: what is sent on the sender is what the prompt reads.
pub fn channel_keys() -> (Sender<Vec<u8>>, ChannelKeys) {
    let (tx, keys) = mpsc::channel();
    (tx, ChannelKeys { keys })
}

impl KeyInput for ChannelKeys {
    fn read(&mut self, timeout: Duration) -> Option<Vec<u8>> {
        let started = Instant::now();
        match self.keys.recv_timeout(timeout) {
            Ok(bytes) => Some(bytes),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                // Nobody is left to type: wait the time out.
                thread::sleep(timeout.saturating_sub(started.elapsed()));
                None
            }
        }
    }
}

/// What a tick shows and decides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step<T> {
    /// The line to show.
    pub line: Option<String>,
    /// The prompt's answer, to end it now.
    pub result: Option<T>,
}

impl<T> Step<T> {
    /// A tick that shows `line` and waits on.
    pub fn line(line: impl Into<String>) -> Step<T> {
        Step {
            line: Some(line.into()),
            result: None,
        }
    }

    /// A tick that ends the prompt with `result`.
    pub fn done(result: T) -> Step<T> {
        Step {
            line: None,
            result: Some(result),
        }
    }
}

/// How a prompt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer<T> {
    /// Enter: now.
    Go,
    /// Any other key: stay.
    Stay,
    /// Ctrl+C: quit.
    Quit,
    /// The tick decided.
    Tick(T),
}

/// How a prompt runs.
pub struct PromptOptions<'a> {
    /// The time between two ticks, in milliseconds.
    pub tick_ms: u64,
    /// Keys typed this soon after the prompt started are ignored, in milliseconds.
    pub grace_ms: Ms,
    /// The clock.
    pub now: &'a dyn Fn() -> Ms,
}

impl Default for PromptOptions<'_> {
    fn default() -> Self {
        PromptOptions {
            tick_ms: 1000,
            grace_ms: 500,
            now: &now_ms,
        }
    }
}

impl std::fmt::Debug for PromptOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptOptions")
            .field("tick_ms", &self.tick_ms)
            .field("grace_ms", &self.grace_ms)
            .finish_non_exhaustive()
    }
}

/// What a chunk of typed keys means.
fn answer_of<T>(bytes: &[u8]) -> Answer<T> {
    if bytes.contains(&0x03) {
        Answer::Quit
    } else if !bytes.is_empty() && bytes.iter().all(|b| *b == b'\r' || *b == b'\n') {
        Answer::Go
    } else {
        Answer::Stay
    }
}

/// Runs the prompt: every tick `on_tick` is given the time and says what to show (redrawn on one
/// line) and whether the prompt is over; a key ends it too. A tick that fails shows its error.
pub fn key_prompt<T>(
    input: &mut dyn KeyInput,
    output: &mut dyn Write,
    options: &PromptOptions<'_>,
    mut on_tick: impl FnMut(Ms) -> Result<Step<T>, String>,
) -> Answer<T> {
    let started = (options.now)();
    input.begin();
    let answer = loop {
        // Asked to end (the terminal closed, a signal): as if the operator quit.
        if baton_core::sys::end_requested().is_some() {
            break Answer::Quit;
        }
        let step = on_tick((options.now)()).unwrap_or_else(|error| Step::line(format!("baton: {error}")));
        if let Some(line) = step.line.filter(|line| !line.is_empty()) {
            let _ = write!(output, "\r{line}\x1b[K");
            let _ = output.flush();
        }
        if let Some(result) = step.result {
            break Answer::Tick(result);
        }
        let deadline = Instant::now() + Duration::from_millis(options.tick_ms);
        let mut typed = None;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Some(bytes) = input.read(left) else {
                continue;
            };
            if (options.now)() - started < options.grace_ms {
                continue;
            }
            typed = Some(answer_of(&bytes));
            break;
        }
        if let Some(answer) = typed {
            break answer;
        }
    };
    input.end();
    let _ = output.write_all(b"\n");
    let _ = output.flush();
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn text(output: &[u8]) -> String {
        String::from_utf8_lossy(output).into_owned()
    }

    #[test]
    fn what_keys_mean() {
        assert_eq!(answer_of::<()>(b"\r"), Answer::Go);
        assert_eq!(answer_of::<()>(b"\r\n"), Answer::Go);
        assert_eq!(answer_of::<()>(b"x"), Answer::Stay);
        assert_eq!(answer_of::<()>(b"\rx"), Answer::Stay);
        assert_eq!(answer_of::<()>(b"x\x03"), Answer::Quit);
        assert_eq!(key_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL), [3]);
        assert_eq!(key_bytes(KeyCode::Char('C'), KeyModifiers::CONTROL), [3]);
        assert_eq!(key_bytes(KeyCode::Enter, KeyModifiers::NONE), b"\r");
        assert_eq!(key_bytes(KeyCode::Char('q'), KeyModifiers::NONE), b"q");
        assert_eq!(
            answer_of::<()>(&key_bytes(KeyCode::Up, KeyModifiers::NONE)),
            Answer::Stay
        );
    }

    #[test]
    fn the_tick_draws_the_line_and_ends_the_prompt() {
        let (_tx, mut keys) = channel_keys();
        let mut output = Vec::new();
        let ticks = Cell::new(0);
        let options = PromptOptions {
            tick_ms: 5,
            grace_ms: 0,
            now: &now_ms,
        };
        let answer = key_prompt(&mut keys, &mut output, &options, |_| {
            ticks.set(ticks.get() + 1);
            Ok(if ticks.get() < 3 {
                Step::line(format!("in {}", 3 - ticks.get()))
            } else {
                Step::done("go")
            })
        });
        assert_eq!(answer, Answer::Tick("go"));
        assert_eq!(text(&output), "\rin 2\x1b[K\rin 1\x1b[K\n");
    }

    #[test]
    fn a_key_ends_the_prompt_but_not_in_the_grace_time() {
        let run = |typed: &[u8], grace_ms: Ms| {
            let (tx, mut keys) = channel_keys();
            tx.send(typed.to_vec()).unwrap();
            let mut output = Vec::new();
            let ticks = Cell::new(0);
            let options = PromptOptions {
                tick_ms: 20,
                grace_ms,
                now: &now_ms,
            };
            key_prompt(&mut keys, &mut output, &options, |_| {
                ticks.set(ticks.get() + 1);
                Ok(if ticks.get() > 4 {
                    Step::done(())
                } else {
                    Step::line("waiting")
                })
            })
        };
        assert_eq!(run(b"\r", 0), Answer::Go);
        assert_eq!(run(b"x", 0), Answer::Stay);
        assert_eq!(run(b"\x03", 0), Answer::Quit);
        assert_eq!(run(b"\r", 60_000), Answer::Tick(()), "a leftover key is ignored");
    }

    #[test]
    fn a_tick_that_fails_shows_why_and_goes_on() {
        let (_tx, mut keys) = channel_keys();
        let mut output = Vec::new();
        let ticks = Cell::new(0);
        let options = PromptOptions {
            tick_ms: 1,
            grace_ms: 0,
            now: &now_ms,
        };
        let answer = key_prompt(&mut keys, &mut output, &options, |_| {
            ticks.set(ticks.get() + 1);
            if ticks.get() == 1 {
                Err("no config".to_owned())
            } else {
                Ok(Step::done(7))
            }
        });
        assert_eq!(answer, Answer::Tick(7));
        assert_eq!(text(&output), "\rbaton: no config\x1b[K\n");
    }
}
