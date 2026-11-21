//! Baton as a Ferry launcher.
//!
//! When Ferry is on this machine (its data folder exists, or `ferry` is on PATH), `baton install`
//! writes `<data>/launchers/baton.json`, so Ferry can open a project in Baton and resume or continue a
//! Claude Code conversation there; `baton uninstall` removes the file while it is still Baton's.
//! `<data>` is `FERRY_HOME`, else `~/.ferry` in Baton's own home (`BATON_HOME` keeps the tests away from
//! the real one). The file runs the baton executable directly (exec form, like the hooks): no shell.
//! Its format is Ferry's launcher schema, version 1.
//!
//! The file names Claude Code's own option for starting without asking for approval (`bypassArgs`), so
//! Ferry's `bypass-permissions` setting and the `Bypass` of a running session cover Baton as they do
//! Claude Code. The setting stays Ferry's and the operator's: Baton names the option and turns nothing
//! on. Ferry puts the option first on the command line, and Baton hands it to claude with the rest.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use baton_core::claude_path::find_on_path;
use baton_core::env::Env;
use baton_core::fsutil::{Modes, read_text};
use baton_core::json::{Get, Object, pretty};
use baton_core::paths::{Paths, forward, resolve, same_path};
use serde_json::{Value, json};

use crate::proc;

/// The id of Baton's launcher: the file is `<id>.json`.
pub const LAUNCHER_ID: &str = "baton";

/// What says a launcher file is the one `baton install` wrote.
pub const LAUNCHER_SOURCE: &str = "baton install";

/// Ferry's built-in Claude Code launcher, which Baton stands in for.
pub const CLAUDE_LAUNCHER: &str = "claude";

/// Claude Code's option that starts it without asking for approval: what Ferry adds for a launcher
/// its `bypass-permissions` setting names.
pub const BYPASS_ARG: &str = "--dangerously-skip-permissions";

/// Ferry keeps its data folder private: a folder Baton creates there is this user's only, and so is
/// the launcher file, as Ferry writes its own.
pub const PRIVATE: Modes = Modes {
    dir: Some(0o700),
    file: Some(0o600),
};

/// How long `ferry config` may take.
const FERRY_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether `path` is a folder.
fn is_dir(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Ferry's data folder: `FERRY_HOME`, else `~/.ferry`.
pub fn ferry_home(paths: &Paths, env: &Env) -> PathBuf {
    match env.some("FERRY_HOME") {
        Some(home) => resolve(home),
        None => paths.home.join(".ferry"),
    }
}

/// Baton's launcher file in Ferry's data folder.
pub fn launcher_path(data: &Path) -> PathBuf {
    data.join("launchers").join(format!("{LAUNCHER_ID}.json"))
}

/// What came of running Ferry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FerryRun {
    /// Its exit code; None when it could not be run.
    pub status: Option<i32>,
    /// What it printed.
    pub stdout: String,
    /// What it printed to its error stream, or why it could not be run.
    pub stderr: String,
}

/// Finds `ferry`: where it is, or None.
pub type FindFerry<'a> = &'a dyn Fn() -> Option<PathBuf>;

/// Runs Ferry: the program, its arguments and its environment.
pub type RunFerry<'a> = &'a dyn Fn(&Path, &[&str], &Env) -> FerryRun;

/// How Ferry is found and run: Baton's own way where a field is None, or a test's.
#[derive(Clone, Copy, Default)]
pub struct FerryAccess<'a> {
    /// Replaces the PATH lookup of `ferry`.
    pub find: Option<FindFerry<'a>>,
    /// Replaces running Ferry.
    pub run: Option<RunFerry<'a>>,
}

impl fmt::Debug for FerryAccess<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FerryAccess")
            .field("find", &self.find.map(|_| "a test's"))
            .field("run", &self.run.map(|_| "a test's"))
            .finish()
    }
}

/// Whether Ferry is on this machine, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ferry {
    /// Whether Ferry was found: its data folder exists, or `ferry` is on PATH.
    pub found: bool,
    /// Its data folder.
    pub data: PathBuf,
    /// Baton's launcher file there.
    pub file: PathBuf,
    /// Where `ferry` is on PATH, but only when the data folder does not exist (then it is what says
    /// Ferry is here).
    pub program: Option<PathBuf>,
    /// Where `ferry` is on PATH; None when it is not.
    pub cli: Option<PathBuf>,
}

/// Looks for Ferry. `find` replaces the PATH lookup (the tests pass their own).
pub fn detect_ferry(paths: &Paths, env: &Env, find: Option<FindFerry<'_>>) -> Ferry {
    let data = ferry_home(paths, env);
    let cli = match find {
        Some(find) => find(),
        None => find_on_path("ferry", env),
    };
    let here = is_dir(&data);
    let program = if here { None } else { cli.clone() };
    Ferry {
        found: program.is_some() || here,
        file: launcher_path(&data),
        data,
        program,
        cli,
    }
}

// ---- The agent Ferry opens -------------------------------------------------------------------------
// Ferry's `c` key and `ferry agent` open one agent launcher: its "agent" setting. Baton takes that
// place while it is registered, through Ferry's own CLI (`ferry config agent baton`), so the setting
// stays Ferry's to check and keep; `baton uninstall` puts the one before back. A launcher chosen on
// purpose (anything but Ferry's default or its built-in Claude Code) stays.

/// Runs Ferry and waits for it.
pub fn run_ferry_sync(program: &Path, args: &[&str], env: &Env) -> FerryRun {
    let mut command = Command::new(program);
    command.args(args);
    env.apply(&mut command);
    let done = proc::run(command, None, FERRY_TIMEOUT);
    FerryRun {
        status: done.status,
        stdout: done.stdout,
        stderr: done.error.unwrap_or(done.stderr),
    }
}

/// Runs Ferry the way `run` says, or Baton's own way.
fn run_ferry(program: &Path, args: &[&str], env: &Env, run: Option<RunFerry<'_>>) -> FerryRun {
    match run {
        Some(run) => run(program, args, env),
        None => run_ferry_sync(program, args, env),
    }
}

/// The agent Ferry opens with `c`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    /// A launcher's id, or `default` for the first installed.
    pub value: String,
    /// Whether it is Ferry's default.
    pub default: bool,
}

/// What `c` opens, from `ferry config agent --json`, or None when Ferry did not say.
pub fn ferry_agent(program: &Path, env: &Env, run: Option<RunFerry<'_>>) -> Option<Agent> {
    let result = run_ferry(program, &["config", "agent", "--json"], env, run);
    if result.status != Some(0) {
        return None;
    }
    let value: Value = serde_json::from_str(&result.stdout).ok()?;
    Some(Agent {
        value: value.text("value")?.to_owned(),
        default: value.truthy("default"),
    })
}

/// Whether Baton takes the agent's place: Ferry's default, or its built-in Claude Code.
pub fn takes_over(agent: &Agent) -> bool {
    agent.default || agent.value == CLAUDE_LAUNCHER
}

/// Sets the agent Ferry opens with `c` to the launcher `id`.
pub fn set_ferry_agent(program: &Path, id: &str, env: &Env, run: Option<RunFerry<'_>>) -> FerryRun {
    run_ferry(program, &["config", "agent", id], env, run)
}

// ---- The launcher file ---------------------------------------------------------------------------------

/// Baton's launcher for Ferry: the baton executable at `exe`, run directly.
pub fn launcher_def(exe: &Path) -> Value {
    json!({
        "schemaVersion": 1,
        "id": LAUNCHER_ID,
        "name": "Baton",
        "description": "Claude Code on your accounts, one after another",
        "program": forward(exe),
        "args": [],
        "resumeArgs": ["--resume", "{conversation}"],
        "continueArgs": ["--continue"],
        "bypassArgs": [BYPASS_ARG],
        "conversations": "claude-code",
        "terminal": true,
        "source": LAUNCHER_SOURCE,
    })
}

/// The launcher file's text.
pub fn launcher_text(exe: &Path) -> String {
    pretty(&launcher_def(exe)) + "\n"
}

/// A launcher file's JSON object, or None when the text is not one.
pub fn parse_launcher(text: &str) -> Option<Object> {
    match serde_json::from_str(text).ok()? {
        Value::Object(value) => Some(value),
        _ => None,
    }
}

/// Only a file Baton wrote is Baton's to remove.
pub fn is_baton_launcher(value: Option<&Object>) -> bool {
    value.is_some_and(|value| {
        value.get("id").and_then(Value::as_str) == Some(LAUNCHER_ID)
            && value.get("source").and_then(Value::as_str) == Some(LAUNCHER_SOURCE)
    })
}

/// How Baton stands with Ferry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registered {
    /// Ferry is not on this machine.
    Absent,
    /// Ferry is here and Baton is not registered.
    Missing,
    /// The launcher file is what `baton install` writes now.
    Current,
    /// The launcher file is Baton's, but not what `baton install` writes now.
    Stale,
    /// There is a `baton.json` that is not Baton's.
    Foreign,
}

/// The registration, for doctor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    /// Where Ferry is.
    pub ferry: Ferry,
    /// How Baton stands with it.
    pub state: Registered,
    /// For a stale one: whether the program it runs is not this baton executable.
    pub program: bool,
}

/// How Baton stands with Ferry: `exe` is the baton executable `baton install` would register.
pub fn registration_state(paths: &Paths, env: &Env, find: Option<FindFerry<'_>>, exe: &Path) -> Registration {
    let ferry = detect_ferry(paths, env, find);
    let state = |ferry: Ferry, state: Registered| Registration {
        ferry,
        state,
        program: false,
    };
    if !ferry.found {
        return state(ferry, Registered::Absent);
    }
    let Some(text) = read_text(&ferry.file) else {
        return state(ferry, Registered::Missing);
    };
    if text == launcher_text(exe) {
        return state(ferry, Registered::Current);
    }
    let Some(value) = parse_launcher(&text).filter(|value| is_baton_launcher(Some(value))) else {
        return state(ferry, Registered::Foreign);
    };
    let theirs = value.get("program").and_then(Value::as_str).unwrap_or_default();
    Registration {
        ferry,
        state: Registered::Stale,
        program: !same_path(theirs, forward(exe)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::paths::baton_paths;

    #[test]
    fn where_ferry_keeps_its_data() {
        let paths = baton_paths(resolve("home"));
        assert_eq!(ferry_home(&paths, &Env::new()), paths.home.join(".ferry"));
        let elsewhere = Env::from_pairs([("FERRY_HOME", "data")]);
        assert_eq!(ferry_home(&paths, &elsewhere), resolve("data"));
        assert_eq!(
            ferry_home(&paths, &Env::from_pairs([("FERRY_HOME", "")])),
            paths.home.join(".ferry")
        );
        assert_eq!(
            launcher_path(&paths.home),
            paths.home.join("launchers").join("baton.json")
        );
    }

    #[test]
    fn the_launcher_runs_the_baton_executable_itself() {
        let def = launcher_def(Path::new("C:/tools/baton/baton.exe"));
        assert_eq!(
            def,
            json!({
                "schemaVersion": 1,
                "id": "baton",
                "name": "Baton",
                "description": "Claude Code on your accounts, one after another",
                "program": "C:/tools/baton/baton.exe",
                "args": [],
                "resumeArgs": ["--resume", "{conversation}"],
                "continueArgs": ["--continue"],
                "bypassArgs": ["--dangerously-skip-permissions"],
                "conversations": "claude-code",
                "terminal": true,
                "source": "baton install"
            })
        );
        let text = launcher_text(Path::new("C:/tools/baton/baton.exe"));
        assert!(text.starts_with("{\n  \"schemaVersion\": 1,\n  \"id\": \"baton\",\n"));
        assert!(text.contains("\n  \"args\": [],\n"));
        assert!(text.ends_with("\n}\n"));
        assert_eq!(parse_launcher(&text), def.as_object().cloned());
    }

    #[test]
    fn whose_launcher_a_file_is() {
        assert!(is_baton_launcher(
            parse_launcher(&launcher_text(Path::new("baton"))).as_ref()
        ));
        for other in [
            r#"{"id":"baton","source":"by hand"}"#,
            r#"{"id":"other","source":"baton install"}"#,
            "{}",
        ] {
            assert!(!is_baton_launcher(parse_launcher(other).as_ref()), "{other}");
        }
        assert_eq!(parse_launcher("[1]"), None);
        assert_eq!(parse_launcher("null"), None);
        assert_eq!(parse_launcher("{ nope"), None);
        assert!(!is_baton_launcher(None));
    }

    #[test]
    fn what_ferry_says_c_opens() {
        let program = Path::new("ferry");
        let env = Env::new();
        let says = |status: Option<i32>, stdout: &'static str| {
            move |_: &Path, args: &[&str], _: &Env| {
                assert_eq!(args, ["config", "agent", "--json"]);
                FerryRun {
                    status,
                    stdout: stdout.to_owned(),
                    stderr: String::new(),
                }
            }
        };
        let ask = |run: &dyn Fn(&Path, &[&str], &Env) -> FerryRun| ferry_agent(program, &env, Some(run));
        assert_eq!(
            ask(&says(
                Some(0),
                r#"{"key":"agent","value":"default","default":true}"#
            )),
            Some(Agent {
                value: "default".into(),
                default: true
            })
        );
        assert_eq!(
            ask(&says(Some(0), r#"{"value":"gemini"}"#)),
            Some(Agent {
                value: "gemini".into(),
                default: false
            })
        );
        assert_eq!(ask(&says(Some(1), r#"{"value":"gemini"}"#)), None);
        assert_eq!(ask(&says(None, "")), None);
        assert_eq!(ask(&says(Some(0), "not json")), None);
        assert_eq!(ask(&says(Some(0), r#"{"value":7}"#)), None);
        assert!(takes_over(&Agent {
            value: "default".into(),
            default: true
        }));
        assert!(takes_over(&Agent {
            value: "claude".into(),
            default: false
        }));
        assert!(!takes_over(&Agent {
            value: "gemini".into(),
            default: false
        }));
    }

    #[test]
    fn a_ferry_that_cannot_be_run_says_why() {
        let missing = Path::new("baton-no-such-ferry-anywhere");
        let done = run_ferry_sync(missing, &["config", "agent", "--json"], &Env::from_process());
        assert_eq!(done.status, None);
        assert!(!done.stderr.is_empty());
        assert_eq!(ferry_agent(missing, &Env::from_process(), None), None);
    }
}
