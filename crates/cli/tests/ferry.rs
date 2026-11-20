//! Baton and Ferry from the command line: what `baton install` and `baton uninstall` say and do about
//! the launcher file and the agent Ferry opens, and what `baton doctor` reports of both. Ferry itself
//! is never run and never looked for on this machine: every test says where it is and answers for it.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

mod common;
mod support;

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use baton::doctor::{DoctorOptions, run_doctor};
use baton::screens::Check;
use baton_core::clock::now_ms;
use baton_core::env::Env;
use baton_core::paths::Paths;
use baton_install::ferry::{
    Agent, FerryAccess, FerryRun, Registered, launcher_def, launcher_text, registration_state,
};
use baton_install::install::{apply_plan, plan_install};
use baton_testkit::{TestHome, temp_home, write, write_json};
use common::{Run, baton_io};
use serde_json::json;
use support::{EXE, after, baton, env_of, glued, json_of, squash};

/// A `ferry` on PATH.
const FERRY_EXE: &str = "C:/tools/ferry/ferry.exe";
/// What the screens call Baton's launcher file in a test home.
const LAUNCHER: &str = "~/.ferry/launchers/baton.json";

/// A test home with Ferry's data folder (`~/.ferry`): the home, the folder and Baton's launcher file.
fn ferry_home() -> (TestHome, PathBuf, PathBuf) {
    let home = temp_home();
    let data = home.home.join(".ferry");
    fs::create_dir_all(&data).unwrap();
    let file = data.join("launchers").join("baton.json");
    (home, data, file)
}

/// `ferry` is never on PATH unless a test says so.
fn no_path() -> Option<PathBuf> {
    None
}

fn on_path() -> Option<PathBuf> {
    Some(PathBuf::from(FERRY_EXE))
}

/// Registers this baton with Ferry the way `baton install` does, without the command line.
fn install(paths: &Paths, exe: &str) {
    let access = FerryAccess {
        find: Some(&no_path),
        run: None,
    };
    let mut plan = plan_install(paths, Path::new(exe), &Env::new(), access).unwrap();
    apply_plan(&mut plan, paths, now_ms(), &Env::new(), None).unwrap();
}

#[test]
fn without_ferry_the_plan_says_so_in_one_line() {
    let home = temp_home();
    let dry = baton(&home, &["install", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    // One line: a mark, then the words.
    let said = dry
        .stdout
        .lines()
        .filter(|line| line.contains("Ferry"))
        .collect::<Vec<_>>();
    assert_eq!(said.len(), 1, "{}", dry.stdout);
    let mut line = said[0].trim_start().chars();
    assert!(line.next().is_some_and(|mark| !mark.is_whitespace()));
    assert_eq!(
        line.as_str(),
        " Ferry not found; run baton install again after installing it"
    );
    // Installed, it writes nothing for Ferry, and uninstall has nothing to say about it.
    let done = baton(&home, &["install"]);
    assert_eq!(done.code, 0, "{}", done.stderr);
    assert!(!home.home.join(".ferry").exists());
    assert!(json_of(&home.paths.install).get("launcher").is_none());
    let removed = baton(&home, &["uninstall"]);
    assert!(!removed.stdout.contains("Ferry"), "{}", removed.stdout);
}

#[test]
fn dry_run_batons_registration_is_planned_and_nothing_is_written() {
    let (home, _, _) = ferry_home();
    let dry = baton(&home, &["install", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    assert!(
        squash(&dry.stdout).contains(&format!("Ferry will register {LAUNCHER}")),
        "{}",
        dry.stdout
    );
    assert!(after(&dry.stdout, "create").contains(&LAUNCHER), "{}", dry.stdout);
    let diff = baton(&home, &["install", "--dry-run", "--diff"]);
    assert!(
        squash(&diff.stdout).contains("+ \"source\": \"baton install\""),
        "{}",
        diff.stdout
    );
    assert!(!home.home.join(".ferry").join("launchers").exists());
    assert!(!home.paths.install.exists());
}

#[test]
fn install_and_uninstall_from_the_command_line_register_and_remove_baton() {
    let (home, _, file) = ferry_home();
    let done = baton(&home, &["install"]);
    assert_eq!(done.code, 0, "{}", done.stderr);
    assert!(
        squash(&done.stdout).contains(&format!("Ferry registered {LAUNCHER}")),
        "{}",
        done.stdout
    );
    assert_eq!(json_of(&file), launcher_def(Path::new(EXE)));
    assert_eq!(json_of(&file)["program"], EXE);
    let again = baton(&home, &["install"]);
    assert!(
        squash(&again.stdout).contains(&format!("Ferry in place {LAUNCHER}")),
        "{}",
        again.stdout
    );
    let removed = baton(&home, &["uninstall"]);
    assert!(
        squash(&removed.stdout).contains(&format!("Ferry removed {LAUNCHER}")),
        "{}",
        removed.stdout
    );
    assert!(!file.exists());
    let nothing = baton(&home, &["uninstall"]);
    assert!(
        squash(&nothing.stdout).contains("Ferry not registered"),
        "{}",
        nothing.stdout
    );
}

#[test]
fn a_launcher_that_cannot_be_written_is_a_note_the_rest_is_installed_and_the_status_line_survives() {
    let (home, data, _) = ferry_home();
    let paths = &home.paths;
    let settings = home.home.join(".claude").join("settings.json");
    write_json(
        &settings,
        &json!({ "statusLine": { "command": "my-own-status" } }),
    );
    // Ferry's launchers/ is a file: Baton cannot write its launcher there.
    write(&data.join("launchers"), "not a folder");
    let run = baton(&home, &["install"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(squash(&run.stdout).contains("Ferry failed"), "{}", run.stdout);
    let said = glued(&run.stdout);
    let note = said
        .find("Couldnotwrite")
        .unwrap_or_else(|| panic!("no note: {}", run.stdout));
    assert!(said[note..].contains("baton.json:"), "{}", run.stdout);
    assert_eq!(run.stderr, "");
    assert!(
        json_of(&settings)["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("statusline")
    );
    assert_eq!(
        json_of(&paths.install)["previousStatusLine"],
        json!({ "command": "my-own-status" })
    );
    // Fixed, it registers, and uninstall still puts the operator's status line back.
    fs::remove_file(data.join("launchers")).unwrap();
    let again = baton(&home, &["install"]);
    assert!(
        squash(&again.stdout).contains("Ferry registered"),
        "{}",
        again.stdout
    );
    assert_eq!(
        json_of(&paths.install)["previousStatusLine"],
        json!({ "command": "my-own-status" })
    );
    assert_eq!(baton(&home, &["uninstall"]).code, 0);
    assert_eq!(
        json_of(&settings),
        json!({ "statusLine": { "command": "my-own-status" } })
    );
}

#[test]
fn a_file_baton_cannot_write_stops_install_with_a_message_and_the_record_keeps_the_status_line() {
    let home = temp_home();
    let paths = &home.paths;
    let settings = home.home.join(".claude").join("settings.json");
    write_json(
        &settings,
        &json!({ "statusLine": { "command": "my-own-status" } }),
    );
    // commands/ is a file: hop.md cannot be written, after settings.json was.
    write(&home.home.join(".claude").join("commands"), "not a folder");
    let run = baton(&home, &["install"]);
    assert_eq!(run.code, 2, "{}", run.stdout);
    assert_eq!(run.stdout, "");
    let said = run
        .stderr
        .find("Could not write")
        .unwrap_or_else(|| panic!("no message: {}", run.stderr));
    assert!(run.stderr[said..].contains("hop.md:"), "{}", run.stderr);
    assert_eq!(
        run.stderr.trim_end().lines().count(),
        1,
        "one line, no trace: {}",
        run.stderr
    );
    assert_eq!(
        json_of(&paths.install)["previousStatusLine"],
        json!({ "command": "my-own-status" })
    );
    fs::remove_file(home.home.join(".claude").join("commands")).unwrap();
    assert_eq!(baton(&home, &["install"]).code, 0);
    assert_eq!(baton(&home, &["uninstall"]).code, 0);
    assert_eq!(
        json_of(&settings),
        json!({ "statusLine": { "command": "my-own-status" } })
    );
}

// ---- Doctor ----------------------------------------------------------------------------------------

/// A config with no accounts and a Claude Code that is not there: doctor then runs nothing.
fn no_claude(home: &TestHome) {
    let nowhere = home.home.join("no-claude-here");
    write_json(
        &home.paths.config,
        &json!({ "version": 1, "accounts": [], "claudePath": nowhere.to_string_lossy() }),
    );
}

/// The check called `name` of a doctor run without sign-ins, for [`EXE`].
fn check(home: &TestHome, access: FerryAccess<'_>, name: &str) -> Option<Check> {
    let report = run_doctor(&DoctorOptions {
        paths: &home.paths,
        env: &env_of(home),
        exe: Path::new(EXE),
        auth: false,
        ferry: access,
        now: now_ms(),
    });
    report.checks.into_iter().find(|check| check.name == name)
}

#[test]
fn doctor_not_installed_which_is_no_failure_not_registered_current_stale_not_batons() {
    let home = temp_home();
    let paths = &home.paths;
    no_claude(&home);
    let access = FerryAccess {
        find: Some(&no_path),
        run: None,
    };
    let ferry = || check(&home, access, "Ferry").unwrap();
    assert_eq!(
        ferry(),
        Check {
            name: "Ferry".to_owned(),
            ok: None,
            detail: "not installed".to_owned()
        }
    );
    assert_eq!(
        check(&home, access, "Ferry's agent"),
        None,
        "nothing to say of an agent without Ferry"
    );

    let data = home.home.join(".ferry");
    let file = data.join("launchers").join("baton.json");
    fs::create_dir_all(&data).unwrap();
    assert_eq!(
        (ferry().ok, ferry().detail.as_str()),
        (Some(false), "not registered: baton install")
    );

    install(paths, EXE);
    assert_eq!(
        (ferry().ok, ferry().detail),
        (Some(true), format!("registered and current ({})", file.display()))
    );

    let stale = |why: &str| {
        let found = ferry();
        assert_eq!(found.ok, Some(false), "{why}");
        assert_eq!(
            found.detail,
            format!("registered but stale ({why}): run baton install again")
        );
    };
    // Registered by a baton that is elsewhere now.
    install(
        paths,
        if cfg!(windows) {
            "C:/tools/elsewhere/baton.exe"
        } else {
            "/opt/elsewhere/baton"
        },
    );
    stale("baton's path changed");
    // Registered by the Node version: node and Baton's cli.js.
    write_json(
        &file,
        &json!({
            "schemaVersion": 1,
            "id": "baton",
            "name": "Baton",
            "description": "Claude Code on your accounts, one after another",
            "program": "C:/Program Files/nodejs/node.exe",
            "args": ["E:/GitHub/baton/src/cli.js"],
            "resumeArgs": ["--resume", "{conversation}"],
            "continueArgs": ["--continue"],
            "conversations": "claude-code",
            "terminal": true,
            "source": "baton install"
        }),
    );
    stale("written by the Node version of Baton");
    // This baton, in older words.
    let mut older = launcher_def(Path::new(EXE));
    older["description"] = json!("older words");
    write_json(&file, &older);
    stale("contents changed");
    assert_eq!(
        registration_state(paths, &Env::new(), Some(&no_path), Path::new(EXE)).state,
        Registered::Stale
    );

    write_json(&file, &json!({ "id": "baton", "source": "by hand" }));
    assert_eq!(
        (ferry().ok, ferry().detail),
        (
            Some(false),
            format!("{} is not Baton's: baton install replaces it", file.display())
        )
    );
    // And what baton install writes makes it current again.
    write(&file, &launcher_text(Path::new(EXE)));
    assert_eq!(ferry().ok, Some(true));
}

/// A `ferry` on PATH, faked: answers `config agent --json` with what it was told, takes
/// `config agent <id>`, and keeps every run.
struct FakeFerry {
    agent: RefCell<Agent>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeFerry {
    fn new(value: &str, default: bool) -> Rc<FakeFerry> {
        Rc::new(FakeFerry {
            agent: RefCell::new(Agent {
                value: value.to_owned(),
                default,
            }),
            calls: RefCell::new(Vec::new()),
        })
    }

    fn run(&self, program: &Path, args: &[&str]) -> FerryRun {
        let mut call = vec![program.to_string_lossy().into_owned()];
        call.extend(args.iter().map(|arg| (*arg).to_owned()));
        self.calls.borrow_mut().push(call);
        match args {
            ["config", "agent", "--json"] => {
                let agent = self.agent.borrow();
                FerryRun {
                    status: Some(0),
                    stdout: json!({ "key": "agent", "value": agent.value, "default": agent.default })
                        .to_string()
                        + "\n",
                    stderr: String::new(),
                }
            }
            ["config", "agent", value] => {
                *self.agent.borrow_mut() = Agent {
                    value: (*value).to_owned(),
                    default: *value == "default",
                };
                FerryRun {
                    status: Some(0),
                    ..FerryRun::default()
                }
            }
            _ => FerryRun {
                status: Some(2),
                stdout: String::new(),
                stderr: "unknown".to_owned(),
            },
        }
    }

    fn current(&self) -> Agent {
        self.agent.borrow().clone()
    }

    fn last_call(&self) -> Vec<String> {
        self.calls.borrow().last().cloned().unwrap_or_default()
    }
}

/// Runs `baton` in a test home with this `ferry` on PATH.
fn baton_with_ferry(home: &TestHome, ferry: &Rc<FakeFerry>, args: &[&str]) -> Run {
    let ferry = Rc::clone(ferry);
    baton_io(env_of(home), args, move |io| {
        io.exe = PathBuf::from(EXE);
        io.find_ferry = Some(Box::new(on_path));
        io.run_ferry = Some(Box::new(move |program, args, _| ferry.run(program, args)));
    })
}

#[test]
fn doctor_whether_c_opens_baton_in_ferry() {
    let (home, _, _) = ferry_home();
    no_claude(&home);
    let agent = |ferry: &Rc<FakeFerry>| {
        let run = |program: &Path, args: &[&str], _: &Env| ferry.run(program, args);
        check(
            &home,
            FerryAccess {
                find: Some(&on_path),
                run: Some(&run),
            },
            "Ferry's agent",
        )
        .unwrap()
    };
    assert_eq!(
        agent(&FakeFerry::new("baton", false)),
        Check {
            name: "Ferry's agent".to_owned(),
            ok: Some(true),
            detail: "Baton: c opens it".to_owned()
        }
    );
    assert_eq!(
        agent(&FakeFerry::new("default", true)),
        Check {
            name: "Ferry's agent".to_owned(),
            ok: None,
            detail: "default: ferry config agent baton makes it Baton".to_owned()
        }
    );
    let gemini = FakeFerry::new("gemini", false);
    assert_eq!(
        agent(&gemini).detail,
        "gemini: ferry config agent baton makes it Baton"
    );
    assert_eq!(
        *gemini.calls.borrow(),
        [[FERRY_EXE, "config", "agent", "--json"]],
        "doctor only asks"
    );
    let unseen = check(
        &home,
        FerryAccess {
            find: Some(&no_path),
            run: None,
        },
        "Ferry's agent",
    )
    .unwrap();
    assert_eq!(
        (unseen.ok, unseen.detail.as_str()),
        (None, "not checked: ferry is not on PATH")
    );
    let mute = |_: &Path, _: &[&str], _: &Env| FerryRun {
        status: Some(1),
        ..FerryRun::default()
    };
    let unanswered = check(
        &home,
        FerryAccess {
            find: Some(&on_path),
            run: Some(&mute),
        },
        "Ferry's agent",
    )
    .unwrap();
    assert_eq!(
        unanswered.detail,
        "not checked: ferry config agent did not answer"
    );
}

#[test]
fn doctor_from_the_command_line_reports_ferry_and_its_agent() {
    let (home, _, file) = ferry_home();
    no_claude(&home);
    let ferry = FakeFerry::new("default", true);
    let before = baton_with_ferry(&home, &ferry, &["doctor", "--json"]).json();
    let of = |report: &serde_json::Value, name: &str| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["name"] == name)
            .cloned()
            .unwrap()
    };
    assert_eq!(
        of(&before, "Ferry"),
        json!({ "name": "Ferry", "ok": false, "detail": "not registered: baton install" })
    );
    assert_eq!(
        of(&before, "Ferry's agent"),
        json!({ "name": "Ferry's agent", "ok": null, "detail": "default: ferry config agent baton makes it Baton" })
    );
    assert_eq!(baton_with_ferry(&home, &ferry, &["install"]).code, 0);
    let after = baton_with_ferry(&home, &ferry, &["doctor", "--json"]).json();
    assert_eq!(
        of(&after, "Ferry"),
        json!({ "name": "Ferry", "ok": true, "detail": format!("registered and current ({})", file.display()) })
    );
    assert_eq!(
        of(&after, "Ferry's agent"),
        json!({ "name": "Ferry's agent", "ok": true, "detail": "Baton: c opens it" })
    );
}

// ---- The agent Ferry opens ---------------------------------------------------------------------------

#[test]
fn install_makes_baton_the_agent_ferry_opens_a_dry_run_only_asks_and_uninstall_puts_the_default_back() {
    let (home, _, _) = ferry_home();
    let paths = &home.paths;
    let ferry = FakeFerry::new("default", true);
    // Planning only asks; a dry run says what will be.
    let shown = baton_with_ferry(&home, &ferry, &["install", "--dry-run"]);
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    assert!(
        squash(&shown.stdout).contains("Ferry's agent will be Baton was default"),
        "{}",
        shown.stdout
    );
    assert_eq!(*ferry.calls.borrow(), [[FERRY_EXE, "config", "agent", "--json"]]);
    assert_eq!(ferry.current().value, "default");
    assert!(!paths.install.exists());

    let done = baton_with_ferry(&home, &ferry, &["install"]);
    assert_eq!(done.code, 0, "{}", done.stderr);
    assert!(
        squash(&done.stdout).contains("Ferry's agent now Baton was default"),
        "{}",
        done.stdout
    );
    assert_eq!(ferry.last_call(), [FERRY_EXE, "config", "agent", "baton"]);
    assert_eq!(ferry.current().value, "baton");
    assert_eq!(json_of(&paths.install)["previousAgent"], "default");
    // Again: nothing to do, and the record keeps what was before Baton.
    let again = baton_with_ferry(&home, &ferry, &["install"]);
    assert!(again.stdout.contains("Already installed"), "{}", again.stdout);
    assert!(
        squash(&again.stdout).contains("Ferry's agent Baton c opens Baton"),
        "{}",
        again.stdout
    );
    assert_eq!(json_of(&paths.install)["previousAgent"], "default");

    let removed = baton_with_ferry(&home, &ferry, &["uninstall"]);
    assert_eq!(removed.code, 0, "{}", removed.stderr);
    assert!(
        squash(&removed.stdout).contains("Ferry's agent put back default"),
        "{}",
        removed.stdout
    );
    assert_eq!(ferry.last_call(), [FERRY_EXE, "config", "agent", "default"]);
    assert!(ferry.current().default);
}

#[test]
fn a_launcher_chosen_on_purpose_stays_and_a_ferry_that_refuses_is_a_note() {
    let (home, _, _) = ferry_home();
    let gemini = FakeFerry::new("gemini", false);
    let kept = baton_with_ferry(&home, &gemini, &["install"]);
    assert_eq!(kept.code, 0, "{}", kept.stderr);
    assert!(
        squash(&kept.stdout).contains("Ferry's agent left alone gemini, as you chose"),
        "{}",
        kept.stdout
    );
    assert!(
        glued(&kept.stdout).contains("ferryconfigagentbatonmakesitBaton"),
        "{}",
        kept.stdout
    );
    assert_eq!(gemini.current().value, "gemini");
    assert!(json_of(&home.paths.install).get("previousAgent").is_none());

    // A `ferry config agent baton` that fails is a note, and the rest is done.
    let (other, _, _) = ferry_home();
    let failing = baton_io(env_of(&other), &["install"], |io| {
        io.exe = PathBuf::from(EXE);
        io.find_ferry = Some(Box::new(on_path));
        io.run_ferry = Some(Box::new(|_, args, _| match args {
            ["config", "agent", "--json"] => FerryRun {
                status: Some(0),
                stdout: json!({ "value": "default", "default": true }).to_string(),
                stderr: String::new(),
            },
            _ => FerryRun {
                status: Some(1),
                stdout: String::new(),
                stderr: "the store is locked\n".to_owned(),
            },
        }));
    });
    assert_eq!(failing.code, 0, "{}", failing.stderr);
    assert!(
        squash(&failing.stdout).contains("Ferry's agent failed"),
        "{}",
        failing.stdout
    );
    assert!(
        glued(&failing.stdout).contains("ferryconfigagentbatonfailed:thestoreislocked."),
        "{}",
        failing.stdout
    );
    assert!(other.paths.install.exists());
}
