//! What the command line tests of install, the hooks, doctor and the terminal share: a `baton` that
//! never looks for Ferry on this machine, the real program run as a process, and ways to read a screen
//! without minding how wide its columns are.
#![allow(dead_code, reason = "each test binary uses a different part of these helpers")]
#![allow(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use baton_core::env::Env;
use baton_testkit::{TestHome, fixtures, test_env};
use serde_json::Value;

use crate::common::{Run, baton_io};

/// The baton executable these tests install: named baton, so that what it writes is known as
/// Baton's, and with a path any shell runs as it is.
pub const EXE: &str = if cfg!(windows) {
    "C:/tools/baton/baton.exe"
} else {
    "/opt/baton/baton"
};

/// The environment of a test home, with the glyphs every terminal here would get.
pub fn env_of(home: &TestHome) -> Env {
    test_env(&home.home).with("BATON_GLYPHS", "rich")
}

/// Runs `baton` in a test home, its output piped: no Ferry on PATH, and [`EXE`] as its own path.
pub fn baton(home: &TestHome, args: &[&str]) -> Run {
    baton_with(env_of(home), args)
}

/// [`baton`] with this environment.
pub fn baton_with(env: Env, args: &[&str]) -> Run {
    baton_io(env, args, |io| {
        io.exe = PathBuf::from(EXE);
        io.find_ferry = Some(Box::new(|| None));
    })
}

/// Runs the real `baton` program as a process, with `input` on its stdin.
pub fn spawn(env: &Env, args: &[&str], input: &str) -> Run {
    let mut command = Command::new(fixtures::baton());
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    env.apply(&mut command);
    let mut child = command.spawn().unwrap();
    if let Some(mut stdin) = child.stdin.take() {
        // A command that does not read its input may be gone already.
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child.wait_with_output().unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// A screen with every run of spaces as one space and every rule as one stroke, so that a test reads
/// its words and not its columns.
pub fn squash(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if (ch == ' ' || ch == '\u{2500}') && out.ends_with(ch) {
            continue;
        }
        out.push(ch);
    }
    out
}

/// A screen without any white space at all: what a long path reads as, wherever its line was broken.
pub fn glued(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// The word that follows each `label` on a screen (words are what white space separates).
pub fn after<'a>(text: &'a str, label: &str) -> Vec<&'a str> {
    let words: Vec<&str> = text.split_whitespace().collect();
    words
        .windows(2)
        .filter(|pair| pair[0] == label)
        .map(|pair| pair[1])
        .collect()
}

/// A file's JSON.
pub fn json_of(file: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
}
