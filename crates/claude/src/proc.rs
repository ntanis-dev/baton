//! Running a helper program to its end without a window, with its output kept, a time limit, and a
//! way to call it off.

use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use baton_core::fsutil::lower_priority;
use baton_core::sys::{ProcessTree, own_group};

/// How often a running child is looked at.
pub(crate) const POLL: Duration = Duration::from_millis(10);

/// Keeps a console program from opening a window of its own (Windows): with no inherited stdio it
/// gets a hidden console instead.
pub(crate) fn hide_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// CREATE_NO_WINDOW.
        const NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

/// Whether a child has exited.
pub(crate) fn exited(child: &mut Child) -> bool {
    !matches!(child.try_wait(), Ok(None))
}

/// What a helper program left behind.
#[derive(Debug, Default)]
pub(crate) struct Captured {
    /// Its exit code; None when it was ended by a signal or its end could not be read.
    pub(crate) code: Option<i32>,
    /// What it wrote to stdout.
    pub(crate) stdout: String,
    /// What it wrote to stderr.
    pub(crate) stderr: String,
    /// Whether it was ended because it ran too long.
    pub(crate) timed_out: bool,
    /// Whether it was ended because it was called off.
    pub(crate) stopped: bool,
}

/// Reads a pipe to its end on a thread of its own; the text arrives on the channel.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Some(mut pipe) = pipe {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            let _ = tx.send(String::from_utf8_lossy(&bytes).into_owned());
        });
    }
    rx
}

/// Runs `command` to its end with nothing on stdin and its output captured. It is ended after
/// `timeout`, or as soon as `stop` is set. An error only when it could not be started.
pub(crate) fn run_captured(
    command: &mut Command,
    timeout: Duration,
    stop: Option<&AtomicBool>,
    low: bool,
) -> io::Result<Captured> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(command);
    // In a group of its own: it is ended with whatever it started, and never outlives this process.
    own_group(command);
    let mut child = command.spawn()?;
    let tree = ProcessTree::background(child.id());
    if low {
        lower_priority(child.id());
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let mut out = Captured::default();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                out.code = status.code();
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        out.stopped = stop.is_some_and(|stop| stop.load(Ordering::SeqCst));
        out.timed_out = !out.stopped && started.elapsed() >= timeout;
        if out.stopped || out.timed_out {
            tree.kill();
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        thread::sleep(POLL);
    }
    // A grandchild may still hold the pipes open: what arrived by now is what there is.
    let grace = Duration::from_secs(2);
    out.stdout = stdout.recv_timeout(grace).unwrap_or_default();
    out.stderr = stderr.recv_timeout(grace).unwrap_or_default();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that prints `text` and exits with `code`.
    fn shell(script_windows: &str, script_unix: &str) -> Command {
        if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/C", script_windows]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", script_unix]);
            command
        }
    }

    #[test]
    fn output_and_exit_code_are_kept() {
        let mut command = shell(
            "echo out& echo err 1>&2& exit 3",
            "echo out; echo err 1>&2; exit 3",
        );
        let done = run_captured(&mut command, Duration::from_secs(20), None, false).unwrap();
        assert_eq!(done.code, Some(3));
        assert_eq!(done.stdout.trim(), "out");
        assert_eq!(done.stderr.trim(), "err");
        assert!(!done.timed_out && !done.stopped);
    }

    #[test]
    fn a_program_that_runs_too_long_or_is_called_off_is_ended() {
        let mut slow = shell("ping -n 30 127.0.0.1 >nul", "sleep 30");
        let started = Instant::now();
        let done = run_captured(&mut slow, Duration::from_millis(300), None, true).unwrap();
        assert!(done.timed_out);
        assert!(started.elapsed() < Duration::from_secs(15));
        let stop = AtomicBool::new(true);
        let mut slow = shell("ping -n 30 127.0.0.1 >nul", "sleep 30");
        let done = run_captured(&mut slow, Duration::from_secs(30), Some(&stop), false).unwrap();
        assert!(done.stopped && !done.timed_out);
    }

    #[test]
    fn a_program_that_is_not_there_is_an_error() {
        let mut missing = Command::new("baton-no-such-program-anywhere");
        assert!(run_captured(&mut missing, Duration::from_secs(1), None, false).is_err());
    }
}
