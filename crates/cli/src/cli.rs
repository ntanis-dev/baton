//! baton: one command for the operator, the hooks and the status line.
//!
//! Commands live in one table (`commands`): each has a group, its usage, a summary, its options and a
//! `run` that returns an [`Outcome`] (its data is printed for `--json`). This module finds the command,
//! reads its arguments, runs it and prints the answer.

use std::io::Write;
use std::path::{Path, PathBuf};

use baton_core::clock::{Ms, now_ms};
use baton_core::env::Env;
use baton_core::json::pretty;
use baton_core::lines::Line;
use baton_core::paths::{Paths, paths_for};
use baton_core::term::{Terminal, document, error_text, render, terminal_for};
use baton_install::ferry::{FerryAccess, FerryRun};
use serde_json::Value;
use thiserror::Error;

use crate::args::{ArgError, Parsed, Spec, parse_args};
use crate::commands::{commands, help_outcome};

/// Why a command did not run to its end.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CliError {
    /// The command was asked for in a way it cannot answer: said plainly, exit code 2.
    #[error("{0}")]
    Usage(String),
    /// Something broke: exit code 1.
    #[error("{0}")]
    Failed(String),
}

impl From<ArgError> for CliError {
    fn from(error: ArgError) -> CliError {
        CliError::Usage(error.0)
    }
}

/// A usage error with this message.
pub fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}

/// A screen of `screens`, built when it is printed (not for `--json`).
pub type Screen = Box<dyn FnOnce(&Terminal) -> Vec<Line>>;

/// A command's answer.
#[derive(Default)]
pub struct Outcome {
    /// What `--json` prints.
    pub data: Option<Value>,
    /// Plain words, when the command has no screen.
    pub text: Option<String>,
    /// The text meant for scripts: printed instead of the screen when stdout is not a terminal.
    pub pipe: Option<String>,
    /// The screen, drawn for the terminal (plain text when stdout is not one).
    pub screen: Option<Screen>,
    /// The exit code.
    pub code: i32,
    /// Whether the command printed what it had to say itself.
    pub quiet: bool,
}

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outcome")
            .field("data", &self.data)
            .field("text", &self.text)
            .field("pipe", &self.pipe)
            .field("screen", &self.screen.is_some())
            .field("code", &self.code)
            .field("quiet", &self.quiet)
            .finish()
    }
}

impl Outcome {
    /// An answer that was already printed, with this exit code.
    pub fn quiet(code: i32) -> Outcome {
        Outcome {
            code,
            quiet: true,
            ..Outcome::default()
        }
    }

    /// An answer with data for `--json` and a screen.
    pub fn shown(data: Value, screen: Screen) -> Outcome {
        Outcome {
            data: Some(data),
            screen: Some(screen),
            ..Outcome::default()
        }
    }

    /// The same answer with this exit code.
    pub fn code(mut self, code: i32) -> Outcome {
        self.code = code;
        self
    }
}

/// Where a stream goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    /// Whether it is a terminal.
    pub tty: bool,
    /// How wide the terminal is.
    pub columns: Option<usize>,
}

/// Replaces the PATH lookup of Ferry.
pub type FindFerryFn = Box<dyn Fn() -> Option<PathBuf>>;

/// Replaces running Ferry: the program, its arguments and its environment.
pub type RunFerryFn = Box<dyn Fn(&Path, &[&str], &Env) -> FerryRun>;

/// Everything a command reaches the outside world through. The tests pass their own.
pub struct Io<'a> {
    /// The environment.
    pub env: Env,
    /// Where answers go.
    pub stdout: &'a mut dyn Write,
    /// Where complaints go.
    pub stderr: &'a mut dyn Write,
    /// What stdout is.
    pub out: Stream,
    /// What stderr is.
    pub err: Stream,
    /// Whether stdin is a terminal.
    pub stdin_tty: bool,
    /// What a hook or the status line reads; None reads the process's own stdin.
    pub stdin: Option<String>,
    /// The baton executable, as `baton install` writes it into settings.json and Ferry's launcher.
    pub exe: PathBuf,
    /// The clock.
    pub now: fn() -> Ms,
    /// Replaces the PATH lookup of Ferry (the tests).
    pub find_ferry: Option<FindFerryFn>,
    /// Replaces running Ferry (the tests).
    pub run_ferry: Option<RunFerryFn>,
}

impl std::fmt::Debug for Io<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Io")
            .field("out", &self.out)
            .field("err", &self.err)
            .field("stdin_tty", &self.stdin_tty)
            .field("exe", &self.exe)
            .finish_non_exhaustive()
    }
}

impl<'a> Io<'a> {
    /// An `Io` over these streams that are not terminals, with this environment.
    pub fn piped(env: Env, stdout: &'a mut dyn Write, stderr: &'a mut dyn Write) -> Io<'a> {
        Io {
            env,
            stdout,
            stderr,
            out: Stream::default(),
            err: Stream::default(),
            stdin_tty: false,
            stdin: Some(String::new()),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("baton")),
            now: now_ms,
            find_ferry: None,
            run_ferry: None,
        }
    }

    /// How Ferry is found and run: Baton's own way, or a test's.
    pub fn ferry(&self) -> FerryAccess<'_> {
        FerryAccess {
            find: self.find_ferry.as_deref(),
            run: self.run_ferry.as_deref(),
        }
    }

    /// What arrives on stdin, up to 4 MiB; nothing when stdin is a terminal.
    pub fn read_stdin(&mut self) -> String {
        if let Some(text) = &self.stdin {
            return text.clone();
        }
        if self.stdin_tty {
            return String::new();
        }
        let mut bytes = Vec::new();
        let limit = 4 << 20;
        let _ = std::io::Read::read_to_end(
            &mut std::io::Read::take(std::io::stdin().lock(), limit),
            &mut bytes,
        );
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// What a command's `run` gets.
#[derive(Debug)]
pub struct Ctx<'a, 'io> {
    /// The command's options and positionals (empty for a command that takes its arguments raw).
    pub values: Parsed,
    /// The arguments as they were typed, after the command's name.
    pub rest: Vec<String>,
    /// Baton's paths.
    pub paths: Paths,
    /// The outside world.
    pub io: &'a mut Io<'io>,
}

/// Rewrites a command's arguments before they are read.
pub type Prepare = fn(&[String]) -> Vec<String>;

/// A command of the table.
#[derive(Clone, Debug)]
pub struct Command {
    /// What is typed after `baton`.
    pub name: &'static str,
    /// The group it is listed under: session, accounts, usage, setup, internal.
    pub group: &'static str,
    /// How it is called, one line per form.
    pub usage: Vec<String>,
    /// What it does.
    pub summary: &'static str,
    /// An example.
    pub example: Option<&'static str>,
    /// Its options.
    pub options: Vec<Spec>,
    /// Whether its arguments are passed on untouched (Claude Code's own).
    pub raw: bool,
    /// Whether it is another name for a command already listed.
    pub alias: bool,
    /// Rewrites the arguments before they are read.
    pub prepare: Option<Prepare>,
    /// Runs it.
    pub run: fn(&mut Ctx<'_, '_>) -> Result<Outcome, CliError>,
}

/// Prints a command's answer: `--json` its data; otherwise its screen drawn for the terminal (plain
/// text when stdout is not one) or, where a command has one, the text meant for scripts (`pipe`).
fn present(outcome: Outcome, json: bool, io: &mut Io<'_>) {
    if json {
        let _ = writeln!(io.stdout, "{}", pretty(&outcome.data.unwrap_or(Value::Null)));
        return;
    }
    let mut text = outcome.text;
    if outcome.screen.is_some() || outcome.pipe.is_some() {
        let term = terminal_for(io.out.tty, io.out.columns, &io.env);
        match (outcome.pipe, outcome.screen) {
            (Some(pipe), _) if !term.tty => text = Some(pipe),
            (_, Some(screen)) => text = Some(render(&document(&screen(&term), &term), &term)),
            _ => {}
        }
    }
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        let _ = writeln!(io.stdout, "{text}");
    }
}

/// An error for stderr, in the menu's look when stderr is a terminal.
fn complain(message: &str, io: &mut Io<'_>) {
    let term = terminal_for(io.err.tty, io.err.columns, &io.env);
    let _ = writeln!(io.stderr, "{}", error_text(message, &term));
}

/// Says that a command failed, and returns its exit code: on stderr for a person, and for whoever
/// asked with `--json` as JSON on stdout (`{ "error", "code" }`), so that what reads the answer reads
/// the failure the same way.
fn fail(message: &str, code: i32, json: bool, io: &mut Io<'_>) -> i32 {
    if json {
        let _ = writeln!(
            io.stdout,
            "{}",
            pretty(&serde_json::json!({ "error": message, "code": code }))
        );
    } else {
        complain(message, io);
    }
    code
}

/// How many arguments that are not options a command takes at most; None where it says itself what
/// is wrong with them (a part it does not know, a list of any length), or never fails (a hook).
fn most_positionals(command: &str, values: &Parsed) -> Option<usize> {
    let part = values.positional(0);
    Some(match command {
        "hop" | "login" | "usage" | "help" => 1,
        "view" | "status" | "pick" | "stats" | "policy" | "install" | "uninstall" | "doctor" | "version" => 0,
        "sync" if part == Some("take") => 3,
        "account" => match part {
            None | Some("list") => 1,
            Some("add" | "remove" | "set") => 2,
            Some("rename" | "move") => 3,
            _ => return None,
        },
        _ => return None,
    })
}

/// Runs `baton` with these arguments (without the program's name) and returns its exit code.
pub fn run(argv: &[String], io: &mut Io<'_>) -> i32 {
    let paths = paths_for(&io.env);
    let first = argv.first().map(String::as_str);
    let mut rest: Vec<String> = argv.iter().skip(1).cloned().collect();
    // `baton` alone, or with claude arguments (`baton --model opus`), starts a session.
    let name = match first {
        Some("-h" | "--help") => "help",
        Some("--version" | "-v") => "version",
        Some(name) if !name.starts_with('-') => name,
        _ => {
            rest = argv.to_vec();
            "start"
        }
    };
    let table = commands();
    // Asked for JSON, also when the arguments cannot be read.
    let asked_json = argv
        .iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json");
    let Some(command) = table.iter().find(|command| command.name == name) else {
        return fail(
            &format!("Unknown command {name}. Try baton help."),
            2,
            asked_json,
            io,
        );
    };
    let mut values = Parsed::default();
    let mut json = false;
    if !command.raw {
        let args = match command.prepare {
            Some(prepare) => prepare(&rest),
            None => rest.clone(),
        };
        let mut specs = command.options.clone();
        specs.push(Spec::flag("json"));
        specs.push(Spec::flag("help").short('h'));
        values = match parse_args(&args, &specs) {
            Ok(values) => values,
            Err(error) => return fail(&error.0, 2, asked_json, io),
        };
        if values.flag("help") {
            present(help_outcome(Some(name)), false, io);
            return 0;
        }
        json = values.flag("json");
        // An argument too many is a mistake, not something to pass over.
        if let Some(most) = most_positionals(name, &values)
            && let Some(extra) = values.positional(most)
        {
            let message = format!("Unexpected argument {extra}. Try baton help {name}.");
            return fail(&message, 2, json, io);
        }
    }
    let mut ctx = Ctx {
        values,
        rest,
        paths,
        io,
    };
    match (command.run)(&mut ctx) {
        Ok(outcome) => {
            let code = outcome.code;
            if !outcome.quiet {
                present(outcome, json, io);
            }
            code
        }
        Err(CliError::Usage(message)) => fail(&message, 2, json, io),
        Err(CliError::Failed(message)) => fail(&message, 1, json, io),
    }
}

/// Runs `baton` for this process: its arguments, environment and streams.
pub fn main() -> i32 {
    use std::io::IsTerminal;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    if let Some(refusal) = baton_core::paths::dev_refusal(cfg!(debug_assertions), &Env::from_process()) {
        let _ = writeln!(stderr, "{refusal}");
        return 1;
    }
    // What a Baton from before files were made private left open is closed now.
    baton_core::fsutil::keep_private(
        &baton_core::paths::baton_paths(baton_core::paths::resolve_home(&Env::from_process())).root,
    );
    let out_tty = stdout.is_terminal();
    let err_tty = stderr.is_terminal();
    let columns = crate::console::columns();
    let mut io = Io {
        env: Env::from_process(),
        out: Stream {
            tty: out_tty,
            columns: if out_tty { columns } else { None },
        },
        err: Stream {
            tty: err_tty,
            columns: if err_tty { columns } else { None },
        },
        stdin_tty: std::io::stdin().is_terminal(),
        stdin: None,
        exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("baton")),
        now: now_ms,
        find_ferry: None,
        run_ferry: None,
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    run(&argv, &mut io)
}
