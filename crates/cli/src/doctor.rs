//! `baton doctor`: Claude Code on PATH and its version, every profile's links, which profiles are signed
//! in (from `claude auth status --json`, of which only loggedIn and authMethod are kept: no email, no
//! organization), whether the status line, hooks and /hop are installed, and Baton's registration with
//! Ferry.

use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use baton_claude::auth::auth_status;
use baton_core::claude_path::{Launcher, find_on_path, resolve_claude};
use baton_core::clock::Ms;
use baton_core::config::{config_problem, load_config, name_of};
use baton_core::env::Env;
use baton_core::format::NO_POLLER;
use baton_core::paths::{Paths, profile_dir, profile_env, same_path};
use baton_core::poll::{poll_holder, read_poll_state};
use baton_install::ferry::{
    FerryAccess, LAUNCHER_ID, Registered, Registration, detect_ferry, ferry_agent, registration_state,
};
use baton_install::hooks::status_line_shell;
use baton_install::install::{install_state, runs_from_path};
use baton_profile::copies::{BASE, read_conflicts};
use baton_profile::profile::{can_symlink_files, inspect_profile};
use baton_profile::sync::{full_sync, read_questions};
use serde_json::{Value, json};

use crate::screens::Check;

/// What `baton doctor` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Whether nothing failed.
    pub ok: bool,
    /// Every check, in the order it was made.
    pub checks: Vec<Check>,
}

impl Report {
    /// The report as `--json` prints it.
    pub fn to_json(&self) -> Value {
        let checks: Vec<Value> = self
            .checks
            .iter()
            .map(|c| json!({ "name": c.name, "ok": c.ok, "detail": c.detail }))
            .collect();
        json!({ "ok": self.ok, "checks": checks })
    }
}

/// What doctor is run with.
#[derive(Clone, Copy, Debug)]
pub struct DoctorOptions<'a> {
    /// Baton's paths.
    pub paths: &'a Paths,
    /// The environment.
    pub env: &'a Env,
    /// The baton executable, for the Ferry registration and the installed commands.
    pub exe: &'a Path,
    /// Whether to ask Claude Code which profiles are signed in.
    pub auth: bool,
    /// How Ferry is found and run.
    pub ferry: FerryAccess<'a>,
    /// The time now.
    pub now: Ms,
}

/// How long Claude Code gets to say its version.
const VERSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs `claude --version` and returns whether it exited with 0 and what it printed.
fn claude_version(launcher: &Launcher, env: &Env) -> (bool, String) {
    let mut command = launcher.command(&["--version".to_owned()]);
    env.apply(&mut command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return (false, String::new());
    };
    let reader = child.stdout.take().map(|mut out| {
        thread::spawn(move || {
            let mut text = String::new();
            let _ = out.read_to_string(&mut text);
            text
        })
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < VERSION_TIMEOUT => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text = reader.and_then(|reader| reader.join().ok()).unwrap_or_default();
    (status.is_some_and(|status| status.success()), text)
}

/// The registration, in doctor's words: ok is None when Ferry is not here.
fn ferry_check(registration: &Registration) -> (Option<bool>, String) {
    let file = registration.ferry.file.display();
    match registration.state {
        Registered::Absent => (None, "not installed".to_owned()),
        Registered::Missing => (Some(false), "not registered: baton install".to_owned()),
        Registered::Current => (Some(true), format!("registered and current ({file})")),
        Registered::Foreign => (
            Some(false),
            format!("{file} is not Baton's: baton install replaces it"),
        ),
        Registered::Stale => {
            let why = if registration.program {
                "baton's path changed"
            } else {
                "contents changed"
            };
            (
                Some(false),
                format!("registered but stale ({why}): run baton install again"),
            )
        }
    }
}

/// Checks everything and reports it.
pub fn run_doctor(options: &DoctorOptions<'_>) -> Report {
    let DoctorOptions {
        paths,
        env,
        exe,
        auth,
        ferry,
        now,
    } = *options;
    let mut checks: Vec<Check> = Vec::new();
    let mut add = |name: &str, ok: Option<bool>, detail: String| {
        checks.push(Check {
            name: name.to_owned(),
            ok,
            detail,
        });
    };
    add(
        "baton",
        Some(true),
        format!("{} ({})", env!("CARGO_PKG_VERSION"), exe.display()),
    );
    // A config that cannot be read shows as no accounts everywhere else: here is why.
    if let Some(problem) = config_problem(paths) {
        add("config.json", Some(false), problem);
    }
    let config = load_config(paths);
    let mut launcher = None;
    match resolve_claude(config.claude_path.as_deref(), env, Some(&paths.home)) {
        Ok(found) => {
            // Never without a profile: an empty folder of Baton's own.
            let mut version_env = env.clone();
            version_env.remove_any_case("CLAUDE_CONFIG_DIR");
            version_env.set(
                "CLAUDE_CONFIG_DIR",
                paths.tmp.join("claude-version").into_os_string(),
            );
            let (ok, said) = claude_version(&found, &version_env);
            let said = said.trim();
            let version = if said.is_empty() {
                "(no version output)"
            } else {
                said
            };
            add("claude", Some(ok), format!("{} {version}", found.path.display()));
            launcher = Some(found);
        }
        Err(error) => add("claude", Some(false), error.0),
    }
    let file_links = can_symlink_files(&paths.tmp);
    add(
        "file links",
        None,
        if file_links {
            "allowed (shared files are symbolic links)".to_owned()
        } else {
            "not allowed (Developer Mode off): settings.json, CLAUDE.md and keybindings.json are two-way copies synchronised at every launch and history.jsonl is not shared; turning on Windows Developer Mode (Settings > System > For developers) makes them real links and shares history.jsonl".to_owned()
        },
    );
    let conflicts = read_conflicts(paths);
    let label = |id: &str| name_of(&config, id);
    if conflicts.is_empty() {
        add("copy conflicts", None, "none".to_owned());
    } else {
        let last: Vec<String> = conflicts[conflicts.len().saturating_sub(5)..]
            .iter()
            .map(|c| {
                let kept = if c.kept == BASE {
                    BASE.to_owned()
                } else {
                    label(&c.kept)
                };
                format!(
                    "{} changed in {} and elsewhere (kept {kept}'s; {}'s is in {})",
                    c.item,
                    label(&c.account),
                    label(&c.account),
                    c.backup.as_deref().unwrap_or_default()
                )
            })
            .collect();
        add(
            "copy conflicts",
            Some(false),
            format!(
                "{}: {} · baton conflicts --clear once handled",
                conflicts.len(),
                last.join("; ")
            ),
        );
    }
    let differ = if full_sync(&config) {
        read_questions(paths).len()
    } else {
        0
    };
    add(
        "full sync",
        if differ > 0 { Some(false) } else { None },
        if !full_sync(&config) {
            "off: each account keeps its own settings (baton policy --full-sync on)".to_owned()
        } else if differ > 0 {
            format!(
                "the accounts differ on {differ} setting{}: baton sync",
                if differ == 1 { "" } else { "s" }
            )
        } else {
            "on: what is set on one account is set on all".to_owned()
        },
    );
    if cfg!(windows) {
        add(
            "status line shell",
            None,
            status_line_shell(env).command.display().to_string(),
        );
    }
    let poll = read_poll_state(paths);
    add(
        "usage poll",
        None,
        match poll_holder(paths, now) {
            Some(holder) => format!(
                "by {} (pid {}); last poll {}",
                if holder.kind == "menu" {
                    "the menu"
                } else {
                    "baton"
                },
                holder.pid,
                poll.last_run_at.as_deref().unwrap_or("none yet")
            ),
            None => NO_POLLER.to_owned(),
        },
    );
    if config.accounts.is_empty() {
        add(
            "accounts",
            Some(false),
            "none yet: baton account add <name>, then baton login <name>".to_owned(),
        );
    }
    for account in &config.accounts {
        let name = format!("account {}", account.name);
        if !profile_dir(paths, &account.id).exists() {
            add(
                &format!("{name} profile"),
                Some(false),
                "missing: run baton account add again or baton login".to_owned(),
            );
        } else {
            let items = inspect_profile(paths, &account.id, Some(file_links));
            let bad: Vec<String> = items
                .iter()
                .filter(|i| {
                    ["missing", "wrong-link", "conflict", "copy-stale", "unsafe-link"].contains(&i.status)
                })
                .map(|i| format!("{} {}", i.name, i.status))
                .collect();
            let count = |status: &str| items.iter().filter(|i| i.status == status).count();
            let detail = if bad.is_empty() {
                // Left out on purpose (the file-links line above says why).
                let not_shared: String = items
                    .iter()
                    .filter(|i| i.status == "not-shared")
                    .map(|i| format!(", {} not shared", i.name))
                    .collect();
                format!(
                    "{} linked, {} copied{not_shared}",
                    count("linked"),
                    count("copy-fresh")
                )
            } else {
                format!(
                    "{} (the next baton session refreshes them; conflicts need a look)",
                    bad.join(", ")
                )
            };
            add(&format!("{name} links"), Some(bad.is_empty()), detail);
        }
        if let Some(launcher) = launcher.as_ref().filter(|_| auth) {
            let status = auth_status(launcher, &profile_env(paths, &account.id, env));
            let detail = if !status.ok {
                format!("unknown: {}", status.error.as_deref().unwrap_or_default())
            } else if status.logged_in {
                format!("signed in ({})", status.auth_method.as_deref().unwrap_or("null"))
            } else {
                format!("not signed in: baton login {}", account.name)
            };
            add(
                &format!("{name} sign-in"),
                Some(status.ok && status.logged_in),
                detail,
            );
        }
    }
    let install = install_state(paths);
    if !install.ok {
        add("install", Some(false), install.error.clone().unwrap_or_default());
    } else {
        add(
            "status line",
            Some(install.status_line),
            if install.status_line {
                "installed"
            } else {
                "not installed: baton install"
            }
            .to_owned(),
        );
        let events = |keep: &dyn Fn(&baton_install::install::HookState) -> bool| {
            install
                .hooks
                .iter()
                .filter(|h| keep(h))
                .map(|h| h.event)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let missing = events(&|h| !h.installed);
        let broken = events(&|h| h.installed && !h.program_exists);
        let (ok, detail) = if !missing.is_empty() {
            (false, format!("missing {missing}: baton install"))
        } else if !broken.is_empty() {
            (false, format!("paths gone for {broken}: baton install again"))
        } else {
            (true, events(&|_| true))
        };
        add("hooks", Some(ok), detail);
        add(
            "/hop command",
            Some(install.hop_command),
            if install.hop_command {
                "installed"
            } else {
                "not installed (baton hop still works)"
            }
            .to_owned(),
        );
        // The status line and /hop name `baton` alone when its own path cannot start a command line.
        if install.status_line && runs_from_path(exe) {
            let found = find_on_path("baton", env);
            let ours = found.as_deref().is_some_and(|found| same_path(found, exe));
            add(
                "baton on PATH",
                Some(ours),
                match (&found, ours) {
                    (_, true) => "the status line and /hop run it from PATH".to_owned(),
                    (Some(other), false) => format!(
                        "{} is another baton: the status line and /hop run it instead of {}",
                        other.display(),
                        exe.display()
                    ),
                    (None, false) => format!(
                        "not on PATH: the status line and /hop cannot run; add {} to PATH",
                        exe.parent().unwrap_or(exe).display()
                    ),
                },
            );
        }
    }
    let (ok, detail) = ferry_check(&registration_state(paths, env, ferry.find, exe));
    add("Ferry", ok, detail);
    // Which agent `c` opens there: Baton, or another by choice (no failure).
    let found = detect_ferry(paths, env, ferry.find);
    if found.found {
        let agent = found
            .cli
            .as_deref()
            .and_then(|cli| ferry_agent(cli, env, ferry.run));
        let baton = agent.as_ref().is_some_and(|agent| agent.value == LAUNCHER_ID);
        let detail = match (&found.cli, &agent) {
            (None, _) => "not checked: ferry is not on PATH".to_owned(),
            (_, None) => "not checked: ferry config agent did not answer".to_owned(),
            (_, Some(_)) if baton => "Baton: c opens it".to_owned(),
            (_, Some(agent)) => format!("{}: ferry config agent baton makes it Baton", agent.value),
        };
        add("Ferry's agent", baton.then_some(true), detail);
    }
    Report {
        ok: checks.iter().all(|check| check.ok != Some(false)),
        checks,
    }
}
