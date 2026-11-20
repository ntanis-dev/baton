//! Running `baton` inside a test: its arguments, a test home's environment, and what it printed.
#![allow(dead_code)]

use baton::cli::{Io, run};
use baton_core::env::Env;
use baton_testkit::{TestHome, test_env};
use serde_json::Value;

/// What a run of `baton` came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// Its exit code.
    pub code: i32,
    /// What it printed.
    pub stdout: String,
    /// What it complained about.
    pub stderr: String,
}

impl Run {
    /// What it printed, as JSON (for `--json`).
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|error| panic!("not JSON ({error}): {}", self.stdout))
    }
}

/// Runs `baton` with these arguments in a test home, its output piped.
pub fn baton(home: &TestHome, args: &[&str]) -> Run {
    baton_env(test_env(&home.home), args)
}

/// Runs `baton` with these arguments and this environment, its output piped.
pub fn baton_env(env: Env, args: &[&str]) -> Run {
    baton_io(env, args, |_| {})
}

/// Runs `baton` with these arguments and this environment; `setup` changes the outside world it sees
/// (a terminal for stdout, text on stdin, another clock, a stand-in for Ferry). Ferry is not found
/// unless `setup` says where it is.
pub fn baton_io(env: Env, args: &[&str], setup: impl FnOnce(&mut Io<'_>)) -> Run {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let argv: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let code = {
        let mut io = Io::piped(env, &mut stdout, &mut stderr);
        // Never the Ferry of this machine: a test that wants one passes its own stand-in.
        io.find_ferry = Some(Box::new(|| None));
        setup(&mut io);
        run(&argv, &mut io)
    };
    Run {
        code,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
}
