//! Running a helper program to its end: its input written, its output kept, and a limit on how long it
//! may take. What Baton runs this way (the operator's own status line command, `ferry config`) is small
//! and quick; a program that hangs is ended.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use baton_core::clock::Ms;
use baton_core::text::span;

/// What came of running a program.
#[derive(Debug, Default)]
pub(crate) struct Finished {
    /// Its exit code; None when it could not be started, was ended for taking too long, or was ended
    /// by a signal.
    pub(crate) status: Option<i32>,
    /// What it printed.
    pub(crate) stdout: String,
    /// What it printed to its error stream.
    pub(crate) stderr: String,
    /// Why it has no exit code, when Baton knows.
    pub(crate) error: Option<String>,
}

/// Reads a stream to its end on a thread of its own and sends what it read.
fn drain(stream: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (send, receive) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut bytes);
        }
        let _ = send.send(bytes);
    });
    receive
}

/// Runs `command` with `input` on its standard input (none: no input at all) and waits for it, at
/// most `timeout`. The program gets no window of its own (Windows).
pub(crate) fn run(mut command: Command, input: Option<&str>, timeout: Duration) -> Finished {
    command.stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: a console program started from a windowless one opens no console.
        command.creation_flags(0x0800_0000);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Finished {
                error: Some(error.to_string()),
                ..Finished::default()
            };
        }
    };
    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
        let input = input.to_owned();
        // On a thread: a program that prints a lot before it reads would otherwise block both sides.
        thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let left = || deadline.saturating_duration_since(Instant::now());
    let mut timed_out = false;
    // Both streams end when the program does; then its exit code is there to take.
    let out = stdout.recv_timeout(left());
    let err = stderr.recv_timeout(left());
    let mut status = None;
    if out.is_ok() && err.is_ok() {
        loop {
            match child.try_wait() {
                Ok(Some(done)) => {
                    status = done.code();
                    break;
                }
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
                _ => {
                    timed_out = true;
                    break;
                }
            }
        }
    } else {
        timed_out = true;
    }
    if timed_out {
        let _ = child.kill();
        let _ = child.wait();
    }
    let text = |first: Result<Vec<u8>, mpsc::RecvTimeoutError>, rest: &mpsc::Receiver<Vec<u8>>| {
        let bytes = first
            .or_else(|_| rest.recv_timeout(Duration::from_millis(200)))
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Finished {
        status: if timed_out { None } else { status },
        stdout: text(out, &stdout),
        stderr: text(err, &stderr),
        error: timed_out.then(|| format!("timed out after {}", span(Some(timeout.as_millis() as Ms)))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command line run by the system's own shell.
    fn shell(line: &str) -> Command {
        let mut command = if cfg!(windows) {
            Command::new("cmd")
        } else {
            Command::new("sh")
        };
        command
            .args(if cfg!(windows) { ["/D", "/C"] } else { ["-c", "--"] })
            .arg(line);
        command
    }

    #[test]
    fn output_and_exit_code_are_kept() {
        let done = run(
            shell("echo out& echo err 1>&2& exit 3"),
            None,
            Duration::from_secs(20),
        );
        assert_eq!(done.status, Some(3));
        assert_eq!(done.stdout.trim(), "out");
        assert_eq!(done.stderr.trim(), "err");
        assert!(done.error.is_none());
    }

    #[test]
    fn input_reaches_the_program() {
        let reader = if cfg!(windows) { "findstr x" } else { "cat" };
        let done = run(shell(reader), Some("x marks\n"), Duration::from_secs(20));
        assert_eq!(done.status, Some(0));
        assert_eq!(done.stdout.trim(), "x marks");
    }

    #[test]
    fn a_program_that_is_not_there_is_an_error_not_a_panic() {
        let done = run(
            Command::new("baton-no-such-program-anywhere"),
            None,
            Duration::from_secs(5),
        );
        assert_eq!(done.status, None);
        assert!(done.error.is_some());
    }

    #[test]
    fn a_program_that_hangs_is_ended() {
        let wait = if cfg!(windows) {
            "ping -n 30 127.0.0.1 > nul"
        } else {
            "sleep 30"
        };
        let started = Instant::now();
        let done = run(shell(wait), None, Duration::from_millis(300));
        assert_eq!(done.status, None);
        assert!(done.error.is_some_and(|error| error.contains("timed out")));
        assert!(started.elapsed() < Duration::from_secs(15));
    }
}
