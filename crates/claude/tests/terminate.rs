//! Ending claude: at once, by keys with the fall back to its process tree, by a signal, and the keys
//! themselves typed into a real console.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::cell::{Cell, RefCell};
use std::process::{Child, Command, Stdio};

use baton_claude::terminate::{EndOptions, TERMINAL_RESET, end_claude, restore_terminal_on, wait_exit};
use baton_core::config::EndMethod;
use baton_testkit::fixtures::{FAKE_USAGE_CLAUDE, bin};

/// A process that runs until it is ended: the fake usage claude, told to hang.
fn sleeper() -> Child {
    Command::new(bin(FAKE_USAGE_CLAUDE))
        .env("FAKE_USAGE_MODE", "hang")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// Ends a process from the outside, the way its own exit would look to `end_claude`.
fn kill_pid(pid: u32) {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &pid.to_string(), "/F"]);
        command
    } else {
        let mut command = Command::new("kill");
        command.args(["-9", &pid.to_string()]);
        command
    };
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
}

#[test]
fn kill_ends_the_process_at_once() {
    let mut child = sleeper();
    let ended = end_claude(
        &mut child,
        &EndOptions {
            method: EndMethod::Kill,
            ..EndOptions::default()
        },
    );
    assert_eq!((ended.method, ended.forced), ("kill", true));
    assert!(child.try_wait().unwrap().is_some());
    // A claude that has exited already is left alone.
    let again = end_claude(&mut child, &EndOptions::default());
    assert_eq!((again.method, again.forced), ("none", false));
}

#[test]
fn keys_that_do_not_end_claude_fall_back_to_ending_its_process_tree_then_restore_the_terminal() {
    let mut child = sleeper();
    let typed: RefCell<Vec<Vec<u32>>> = RefCell::new(Vec::new());
    let restored = Cell::new(0);
    let type_keys = |codes: &[u32]| {
        typed.borrow_mut().push(codes.to_vec());
        "ok".to_owned()
    };
    let restore = || restored.set(restored.get() + 1);
    let ended = end_claude(
        &mut child,
        &EndOptions {
            method: EndMethod::Keys,
            type_keys: Some(&type_keys),
            rounds: 2,
            round_wait_ms: 100,
            restore: Some(&restore),
            ..EndOptions::default()
        },
    );
    assert_eq!(*typed.borrow(), [[3, 3], [3, 3]]);
    assert_eq!((ended.method, ended.forced, restored.get()), ("keys", true, 1));
    assert_eq!(ended.detail, ["ok", "ok"]);
    assert!(wait_exit(&mut child, 1000));
}

#[test]
fn keys_stop_as_soon_as_claude_has_exited() {
    let mut child = sleeper();
    let pid = child.id();
    let rounds = Cell::new(0);
    let type_keys = |_codes: &[u32]| {
        rounds.set(rounds.get() + 1);
        kill_pid(pid);
        "ok".to_owned()
    };
    let ended = end_claude(
        &mut child,
        &EndOptions {
            method: EndMethod::Keys,
            type_keys: Some(&type_keys),
            round_wait_ms: 2000,
            ..EndOptions::default()
        },
    );
    assert_eq!(
        (ended.method, ended.forced, ended.rounds, rounds.get()),
        ("keys", false, Some(1), 1)
    );
}

#[cfg(unix)]
#[test]
fn sigterm_lets_claude_shut_down_by_itself() {
    use std::os::unix::process::ExitStatusExt;

    let mut child = sleeper();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let ended = end_claude(
        &mut child,
        &EndOptions {
            method: EndMethod::Signal,
            ..EndOptions::default()
        },
    );
    assert_eq!((ended.method, ended.forced), ("signal", false));
    assert_eq!(
        child.wait().unwrap().signal(),
        Some(15),
        "ended by SIGTERM, not by a kill"
    );
}

#[test]
fn restoring_the_terminal_writes_the_reset_sequence_only_to_a_terminal() {
    let mut out = Vec::new();
    restore_terminal_on(&mut out, false, false);
    assert!(out.is_empty());
    restore_terminal_on(&mut out, true, false);
    assert_eq!(String::from_utf8(out).unwrap(), TERMINAL_RESET);
}

#[cfg(windows)]
#[test]
fn two_ctrl_c_typed_into_the_console_arrive_as_keys_at_a_raw_mode_reader() {
    use baton_claude::terminate::{KeyTiming, type_console_keys};
    use baton_core::fsutil::read_json_lines;
    use baton_testkit::fixtures::RAW_READER;
    use baton_testkit::until;
    use std::time::Instant;

    let console = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(r"\\.\CONIN$");
    if console.is_err() {
        eprintln!("skipped: needs a Windows console");
        return;
    }
    drop(console);
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("reader.jsonl");
    let mut reader = Command::new(bin(RAW_READER))
        .arg(&log)
        .arg("15000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let first = until(|| read_json_lines(&log).into_iter().next());
    if let Some(error) = first.get("error") {
        let _ = reader.kill();
        let _ = reader.wait();
        eprintln!("skipped: the console cannot be read here ({error})");
        return;
    }
    let started = Instant::now();
    let timing = KeyTiming { delay_ms: 200 };
    assert_eq!(type_console_keys(&[3, 3], timing), "ok");
    assert_eq!(reader.wait().unwrap().code(), Some(0));
    eprintln!("keys typed and read in {} ms", started.elapsed().as_millis());
    let chunks: Vec<String> = read_json_lines(&log)
        .iter()
        .filter_map(|entry| {
            entry
                .get("data")
                .and_then(|data| data.as_str())
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(chunks, ["\x03", "\x03"], "two separate key presses");
}
