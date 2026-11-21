//! Baton's registration with Ferry: the launcher file `baton install` writes and `baton uninstall`
//! removes, and the agent Ferry opens with `c`.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};

use baton_core::clock::now_ms;
use baton_core::env::Env;
use baton_core::paths::Paths;
use baton_install::ferry::{
    Agent, FerryAccess, FerryRun, Registered, launcher_def, launcher_text, registration_state,
};
use baton_install::install::{Applied, InstallError, Item, Plan, apply_plan, plan_install, plan_uninstall};
use baton_testkit::{TestHome, temp_home, write, write_json};
use regex::Regex;
use serde_json::{Value, json};

/// The baton executable under test.
const EXE: &str = "C:/Program Files/Baton/baton.exe";
/// A `ferry` on PATH.
const FERRY_EXE: &str = "C:/tools/ferry/ferry.exe";

fn read(file: &Path) -> String {
    fs::read_to_string(file).unwrap()
}

fn json_of(file: &Path) -> Value {
    serde_json::from_str(&read(file)).unwrap()
}

/// `ferry` is never on PATH unless a test says so.
fn no_path() -> Option<PathBuf> {
    None
}

fn on_path() -> Option<PathBuf> {
    Some(PathBuf::from(FERRY_EXE))
}

const NO_PATH: FerryAccess<'static> = FerryAccess {
    find: Some(&no_path),
    run: None,
};

/// A test home with Ferry's data folder (`~/.ferry`): the home, the folder and Baton's launcher file.
fn ferry_home() -> (TestHome, PathBuf, PathBuf) {
    let home = temp_home();
    let data = home.home.join(".ferry");
    fs::create_dir_all(&data).unwrap();
    let file = data.join("launchers").join("baton.json");
    (home, data, file)
}

fn plan_with(paths: &Paths, exe: &str, env: &Env, access: FerryAccess<'_>) -> Plan {
    plan_install(paths, Path::new(exe), env, access).unwrap()
}

fn plan(paths: &Paths) -> Plan {
    plan_with(paths, EXE, &Env::new(), NO_PATH)
}

fn install_with(paths: &Paths, exe: &str, env: &Env, access: FerryAccess<'_>) -> (Plan, Applied) {
    let mut plan = plan_with(paths, exe, env, access);
    let applied = apply_plan(&mut plan, paths, now_ms(), &Env::new(), access.run).unwrap();
    (plan, applied)
}

fn install(paths: &Paths) -> (Plan, Applied) {
    install_with(paths, EXE, &Env::new(), NO_PATH)
}

fn uninstall_with(paths: &Paths, env: &Env, access: FerryAccess<'_>) -> (Plan, Applied) {
    let mut plan = plan_uninstall(paths, env, access).unwrap();
    let applied = apply_plan(&mut plan, paths, now_ms(), &Env::new(), access.run).unwrap();
    (plan, applied)
}

fn uninstall(paths: &Paths) -> (Plan, Applied) {
    uninstall_with(paths, &Env::new(), NO_PATH)
}

fn items<'a>(plan: &'a Plan, name: &str) -> Vec<&'a Item> {
    plan.items.iter().filter(|item| item.name == name).collect()
}

/// The actions and files of a plan's Ferry items.
fn ferry_items(plan: &Plan) -> Vec<(&'static str, Option<PathBuf>)> {
    items(plan, "ferry")
        .into_iter()
        .map(|item| (item.action, item.path.clone()))
        .collect()
}

/// Ferry's launcher format, schema version 1: what is wrong with a file (nothing when it is valid).
fn launcher_problems(def: &Value, file_name: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let strings = |v: &Value| {
        v.as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    };
    let unescaped = |text: &str| text.replace("{{", "").replace("}}", "");
    let placeholder = Regex::new(r"\{([^{}]*)\}").unwrap();
    let mut placeholders = |text: &str, allowed: &[&str]| {
        let rest = unescaped(text);
        for found in placeholder.captures_iter(&rest) {
            if !allowed.contains(&&found[1]) {
                problems.push(format!("placeholder {{{}}}", &found[1]));
            }
        }
        if placeholder.replace_all(&rest, "").contains(['{', '}']) {
            problems.push(format!("a stray brace in {text}"));
        }
    };
    let list = |key: &str| -> Vec<String> {
        def[key]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    for arg in list("args")
        .iter()
        .chain(&list("continueArgs"))
        .chain(&list("bypassArgs"))
    {
        placeholders(arg, &["path", "name"]);
    }
    for arg in &list("resumeArgs") {
        placeholders(arg, &["path", "name", "conversation"]);
    }
    if def["schemaVersion"] != 1 {
        problems.push("schemaVersion".to_owned());
    }
    let id = def["id"].as_str().unwrap_or_default();
    if !Regex::new("^[a-z0-9][a-z0-9-]{0,31}$").unwrap().is_match(id) || format!("{id}.json") != file_name {
        problems.push("id".to_owned());
    }
    let name = def["name"].as_str().unwrap_or_default();
    if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        problems.push("name".to_owned());
    }
    if let Some(description) = def.get("description")
        && !description
            .as_str()
            .is_some_and(|d| d.chars().count() <= 120 && !d.contains(['\r', '\n']))
    {
        problems.push("description".to_owned());
    }
    if def["program"].as_str().is_none_or(str::is_empty) {
        problems.push("program".to_owned());
    }
    for key in ["args", "resumeArgs", "continueArgs", "bypassArgs"] {
        if def.get(key).is_some_and(|value| !strings(value)) {
            problems.push(key.to_owned());
        }
    }
    if def.get("resumeArgs").is_some()
        && !list("resumeArgs")
            .iter()
            .any(|a| unescaped(a).contains("{conversation}"))
    {
        problems.push("resumeArgs without {conversation}".to_owned());
    }
    if (def.get("resumeArgs").is_some() || def.get("continueArgs").is_some())
        && def.get("conversations").is_none()
    {
        problems.push("resumeArgs or continueArgs without conversations".to_owned());
    }
    if def.get("conversations").is_some_and(|c| c != "claude-code") {
        problems.push("conversations".to_owned());
    }
    if def.get("terminal").is_some_and(|t| !t.is_boolean()) {
        problems.push("terminal".to_owned());
    }
    if def
        .get("source")
        .is_some_and(|s| s.as_str().is_none_or(|s| s.len() > 80))
    {
        problems.push("source".to_owned());
    }
    problems
}

#[test]
fn without_ferry_the_plan_says_so_in_one_item_and_nothing_is_written_for_it() {
    let home = temp_home();
    let paths = &home.paths;
    let mut planned = plan(paths);
    assert_eq!(ferry_items(&planned), [("missing", None)]);
    assert!(
        planned
            .files
            .iter()
            .all(|f| !f.path.to_string_lossy().contains(".ferry"))
    );
    apply_plan(&mut planned, paths, now_ms(), &Env::new(), None).unwrap();
    assert!(!home.home.join(".ferry").exists());
    assert!(json_of(&paths.install).get("launcher").is_none());
    // A FERRY_HOME that does not exist is not Ferry either.
    let elsewhere = Env::from_pairs([("FERRY_HOME", home.home.join("no").into_os_string())]);
    assert_eq!(
        ferry_items(&plan_with(paths, EXE, &elsewhere, NO_PATH)),
        [("missing", None)]
    );
    // Uninstall has nothing to say about it.
    assert!(ferry_items(&uninstall(paths).0).is_empty());
}

#[test]
fn dry_run_batons_registration_is_planned_and_nothing_is_written() {
    let (home, _, file) = ferry_home();
    let paths = &home.paths;
    let planned = plan(paths);
    assert_eq!(ferry_items(&planned), [("add", Some(file.clone()))]);
    let launcher = planned.files.iter().find(|f| f.path == file).unwrap();
    assert_eq!(launcher.before, None);
    assert!(launcher.ferry);
    assert_eq!(
        serde_json::from_str::<Value>(launcher.after.as_deref().unwrap()).unwrap(),
        launcher_def(Path::new(EXE))
    );
    assert!(
        launcher
            .diff
            .iter()
            .any(|line| line == "+   \"source\": \"baton install\""),
        "{:?}",
        launcher.diff
    );
    assert!(!home.home.join(".ferry").join("launchers").exists());
    assert!(!paths.install.exists());
}

#[test]
fn install_writes_the_launcher_file_in_ferrys_format_the_baton_executable_exec_form() {
    let (home, _, file) = ferry_home();
    let paths = &home.paths;
    let (_, applied) = install(paths);
    assert!(applied.changed);
    let def = json_of(&file);
    assert_eq!(
        def,
        json!({
            "schemaVersion": 1,
            "id": "baton",
            "name": "Baton",
            "description": "Claude Code on your accounts, one after another",
            "program": EXE,
            "args": [],
            "resumeArgs": ["--resume", "{conversation}"],
            "continueArgs": ["--continue"],
            "bypassArgs": ["--dangerously-skip-permissions"],
            "conversations": "claude-code",
            "terminal": true,
            "source": "baton install"
        })
    );
    assert_eq!(launcher_problems(&def, "baton.json"), Vec::<String>::new());
    assert_eq!(read(&file), launcher_text(Path::new(EXE)));
    assert_eq!(json_of(&paths.install)["launcher"], json!(file.to_string_lossy()));
    // A path in the system's own spelling is written with forward slashes, and stays absolute.
    let own = std::env::current_exe().unwrap();
    let program = launcher_def(&own)["program"].as_str().unwrap().to_owned();
    assert!(!program.contains('\\'));
    assert!(Path::new(&program).is_absolute());
}

#[test]
fn ferry_home_or_ferry_on_path_finds_ferry_and_launchers_is_created() {
    let home = temp_home();
    let data = home.root.path().join("ferry-data");
    fs::create_dir_all(&data).unwrap();
    let env = Env::from_pairs([("FERRY_HOME", data.clone().into_os_string())]);
    install_with(&home.paths, EXE, &env, NO_PATH);
    assert!(data.join("launchers").join("baton.json").exists());
    assert!(!home.home.join(".ferry").exists());

    // On PATH only: ~/.ferry, made with its launchers/ folder.
    let other = temp_home();
    let found = || Some(other.root.path().join("bin").join("ferry.exe"));
    let mute = |_: &Path, _: &[&str], _: &Env| FerryRun {
        status: Some(1),
        ..FerryRun::default()
    };
    install_with(
        &other.paths,
        EXE,
        &Env::new(),
        FerryAccess {
            find: Some(&found),
            run: Some(&mute),
        },
    );
    assert!(
        other
            .home
            .join(".ferry")
            .join("launchers")
            .join("baton.json")
            .exists()
    );

    // The PATH lookup itself, from the environment it is given.
    let third = temp_home();
    let bin = third.root.path().join("bin");
    let program = bin.join(if cfg!(windows) { "ferry.exe" } else { "ferry" });
    write(&program, "");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let with_ferry = Env::from_pairs([("PATH", bin.clone().into_os_string()), ("PATHEXT", ".EXE".into())]);
    let found = plan_install(&third.paths, Path::new(EXE), &with_ferry, FerryAccess::default()).unwrap();
    assert_eq!(ferry_items(&found)[0].0, "add");
    // It is there but cannot answer (an empty file): the agent is left for the user.
    assert_eq!(
        items(&found, "agent")[0].why,
        Some("ferry config agent did not answer")
    );
    let without = Env::from_pairs([
        ("PATH", third.root.path().join("empty").into_os_string()),
        ("PATHEXT", ".EXE".into()),
    ]);
    let empty = plan_install(&third.paths, Path::new(EXE), &without, FerryAccess::default()).unwrap();
    assert_eq!(ferry_items(&empty)[0].0, "missing");
}

#[test]
fn install_again_changes_nothing_and_baton_moving_updates_the_file_after_a_backup() {
    let (home, _, file) = ferry_home();
    let paths = &home.paths;
    install(paths);
    let text = read(&file);
    let (again, applied) = install(paths);
    assert!(!applied.changed);
    assert!(again.files.is_empty());
    assert_eq!(ferry_items(&again), [("same", Some(file.clone()))]);
    assert_eq!(read(&file), text);
    let moved = "C:/tools/baton-moved/baton.exe";
    let (after, applied) = install_with(paths, moved, &Env::new(), NO_PATH);
    assert_eq!(ferry_items(&after)[0].0, "update");
    assert_eq!(read(&applied.backup.unwrap().join("ferry-baton.json")), text);
    assert_eq!(json_of(&file)["program"], moved);
}

#[test]
fn a_baton_json_that_is_not_batons_is_backed_up_then_replaced() {
    let (home, _, file) = ferry_home();
    let paths = &home.paths;
    write_json(
        &file,
        &json!({ "schemaVersion": 1, "id": "baton", "name": "Something else", "program": "something", "source": "by hand" }),
    );
    let before = read(&file);
    let (planned, applied) = install(paths);
    assert!(planned.notes.iter().any(|n| n.contains("not Baton's")));
    assert_eq!(read(&applied.backup.unwrap().join("ferry-baton.json")), before);
    assert_eq!(json_of(&file)["source"], "baton install");
}

#[test]
fn uninstall_removes_batons_launcher_only_after_a_backup_and_the_other_launchers_stay() {
    let (home, data, file) = ferry_home();
    let paths = &home.paths;
    let theirs = data.join("launchers").join("tool.json");
    write_json(
        &theirs,
        &json!({ "schemaVersion": 1, "id": "tool", "name": "Tool", "program": "t" }),
    );
    install(paths);
    let text = read(&file);
    let (planned, applied) = uninstall(paths);
    assert_eq!(ferry_items(&planned), [("remove", Some(file.clone()))]);
    assert!(!file.exists());
    assert!(theirs.exists());
    assert_eq!(read(&applied.backup.unwrap().join("ferry-baton.json")), text);
    let (again, applied) = uninstall(paths);
    assert!(!applied.changed);
    assert_eq!(ferry_items(&again), [("absent", None)]);
}

#[test]
fn uninstall_leaves_a_baton_json_whose_id_or_source_is_not_batons() {
    let (home, _, file) = ferry_home();
    let paths = &home.paths;
    install(paths);
    for (key, value) in [("source", "by hand"), ("id", "other")] {
        let mut mine = launcher_def(Path::new(EXE));
        mine[key] = json!(value);
        write_json(&file, &mine);
        let (planned, _) = uninstall(paths);
        assert_eq!(ferry_items(&planned), [("skip", Some(file.clone()))]);
        assert!(planned.notes.iter().any(|n| n.contains("not Baton's")));
        assert_eq!(json_of(&file), mine);
    }
}

#[test]
fn uninstall_finds_the_launcher_where_install_wrote_it_after_ferry_home_changed() {
    let home = temp_home();
    let paths = &home.paths;
    let data = home.root.path().join("ferry-data");
    fs::create_dir_all(&data).unwrap();
    let env = Env::from_pairs([("FERRY_HOME", data.clone().into_os_string())]);
    install_with(paths, EXE, &env, NO_PATH);
    let file = data.join("launchers").join("baton.json");
    assert!(file.exists());
    let (planned, _) = uninstall(paths);
    assert_eq!(ferry_items(&planned), [("remove", Some(file.clone()))]);
    assert!(!file.exists());
}

#[test]
fn a_launcher_that_cannot_be_written_is_a_note_the_rest_is_installed_and_the_status_line_survives() {
    let (home, data, file) = ferry_home();
    let paths = &home.paths;
    let settings = home.home.join(".claude").join("settings.json");
    write_json(
        &settings,
        &json!({ "statusLine": { "command": "my-own-status" } }),
    );
    // Ferry's launchers/ is a file: Baton cannot write its launcher there.
    write(&data.join("launchers"), "not a folder");
    let (planned, applied) = install(paths);
    assert!(applied.changed);
    assert_eq!(ferry_items(&planned), [("failed", Some(file.clone()))]);
    let note = planned
        .notes
        .iter()
        .find(|n| n.contains("Could not write"))
        .unwrap();
    assert!(note.contains("baton.json:"), "{note}");
    assert!(
        note.ends_with(
            ". The rest is done; fix it and run baton install again to register Baton with Ferry."
        ),
        "{note}"
    );
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
    let (again, _) = install(paths);
    assert_eq!(ferry_items(&again), [("add", Some(file.clone()))]);
    assert!(file.exists());
    assert_eq!(
        json_of(&paths.install)["previousStatusLine"],
        json!({ "command": "my-own-status" })
    );
    uninstall(paths);
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
    let mut planned = plan(paths);
    let InstallError(message) = apply_plan(&mut planned, paths, now_ms(), &Env::new(), None).unwrap_err();
    assert!(message.starts_with("Could not write "), "{message}");
    assert!(message.contains("hop.md: "), "{message}");
    assert_eq!(
        json_of(&paths.install)["previousStatusLine"],
        json!({ "command": "my-own-status" })
    );
    fs::remove_file(home.home.join(".claude").join("commands")).unwrap();
    assert!(install(paths).1.changed);
    uninstall(paths);
    assert_eq!(
        json_of(&settings),
        json!({ "statusLine": { "command": "my-own-status" } })
    );
}

#[cfg(unix)]
#[test]
fn ferrys_folders_that_install_creates_are_private_and_so_is_the_launcher() {
    use std::os::unix::fs::PermissionsExt;
    let home = temp_home();
    let found = || Some(home.root.path().join("bin").join("ferry"));
    let mute = |_: &Path, _: &[&str], _: &Env| FerryRun {
        status: Some(1),
        ..FerryRun::default()
    };
    install_with(
        &home.paths,
        EXE,
        &Env::new(),
        FerryAccess {
            find: Some(&found),
            run: Some(&mute),
        },
    );
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let data = home.home.join(".ferry");
    assert_eq!(mode(&data), 0o700);
    assert_eq!(mode(&data.join("launchers")), 0o700);
    assert_eq!(mode(&data.join("launchers").join("baton.json")), 0o600);
}

#[test]
fn how_baton_stands_with_ferry_not_installed_not_registered_current_stale_not_batons() {
    let home = temp_home();
    let paths = &home.paths;
    let state = |exe: &str| registration_state(paths, &Env::new(), NO_PATH.find, Path::new(exe));
    assert_eq!(state(EXE).state, Registered::Absent);

    let data = home.home.join(".ferry");
    let file = data.join("launchers").join("baton.json");
    fs::create_dir_all(&data).unwrap();
    let missing = state(EXE);
    assert_eq!(missing.state, Registered::Missing);
    assert_eq!(missing.ferry.file, file);

    install(paths);
    let current = state(EXE);
    assert_eq!((current.state, current.program), (Registered::Current, false));

    // The baton executable is elsewhere now.
    let moved = state("C:/tools/baton-moved/baton.exe");
    assert_eq!((moved.state, moved.program), (Registered::Stale, true));
    // The same program, older words.
    let mut older = launcher_def(Path::new(EXE));
    older["description"] = json!("older words");
    write_json(&file, &older);
    let changed = state(EXE);
    assert_eq!((changed.state, changed.program), (Registered::Stale, false));

    write_json(&file, &json!({ "id": "baton", "source": "by hand" }));
    assert_eq!(state(EXE).state, Registered::Foreign);
    write(&file, "not json at all");
    assert_eq!(state(EXE).state, Registered::Foreign);
}

// ---- The agent Ferry opens ---------------------------------------------------------------------------

/// A `ferry` on PATH, faked: answers `config agent --json` with what it was told, takes
/// `config agent <id>`, and keeps every run.
struct FakeFerry {
    agent: RefCell<Agent>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeFerry {
    fn new(value: &str, default: bool) -> FakeFerry {
        FakeFerry {
            agent: RefCell::new(Agent {
                value: value.to_owned(),
                default,
            }),
            calls: RefCell::new(Vec::new()),
        }
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

/// The action and what goes with it of a plan's agent items: (action, previous, current, why).
type AgentItem = (&'static str, Option<String>, Option<String>, Option<&'static str>);

fn agent_items(plan: &Plan) -> Vec<AgentItem> {
    items(plan, "agent")
        .into_iter()
        .map(|item| (item.action, item.previous.clone(), item.current.clone(), item.why))
        .collect()
}

#[test]
fn install_makes_baton_the_agent_ferry_opens_when_that_was_ferrys_default_and_uninstall_puts_the_default_back()
 {
    let (home, _, _) = ferry_home();
    let paths = &home.paths;
    let ferry = FakeFerry::new("default", true);
    let run = |program: &Path, args: &[&str], _: &Env| ferry.run(program, args);
    let with = FerryAccess {
        find: Some(&on_path),
        run: Some(&run),
    };
    // Planning only asks; a dry run says what will be.
    let dry = plan_with(paths, EXE, &Env::new(), with);
    assert_eq!(
        agent_items(&dry),
        [("set", Some("default".to_owned()), None, None)]
    );
    assert_eq!(*ferry.calls.borrow(), [[FERRY_EXE, "config", "agent", "--json"]]);
    assert_eq!(
        dry.agent.as_ref().map(|a| (a.program.clone(), a.id.as_str())),
        Some((PathBuf::from(FERRY_EXE), "baton"))
    );
    assert_eq!(ferry.current().value, "default");

    let (_, applied) = install_with(paths, EXE, &Env::new(), with);
    assert!(applied.changed);
    assert_eq!(ferry.last_call(), [FERRY_EXE, "config", "agent", "baton"]);
    assert_eq!(ferry.current().value, "baton");
    assert_eq!(json_of(&paths.install)["previousAgent"], "default");
    // Again: nothing to do, and the record keeps what was before Baton.
    let (again, applied) = install_with(paths, EXE, &Env::new(), with);
    assert!(!applied.changed);
    assert_eq!(agent_items(&again), [("same", None, None, None)]);
    assert_eq!(json_of(&paths.install)["previousAgent"], "default");

    let (un, _) = uninstall_with(paths, &Env::new(), with);
    assert_eq!(
        agent_items(&un),
        [("restore", Some("default".to_owned()), None, None)]
    );
    assert_eq!(ferry.last_call(), [FERRY_EXE, "config", "agent", "default"]);
    assert!(ferry.current().default);
}

#[test]
fn ferrys_built_in_claude_code_gives_way_to_baton_and_comes_back_and_a_launcher_chosen_on_purpose_stays() {
    let (home, _, _) = ferry_home();
    let paths = &home.paths;
    let claude = FakeFerry::new("claude", false);
    let run = |program: &Path, args: &[&str], _: &Env| claude.run(program, args);
    let as_claude = FerryAccess {
        find: Some(&on_path),
        run: Some(&run),
    };
    install_with(paths, EXE, &Env::new(), as_claude);
    assert_eq!(claude.current().value, "baton");
    assert_eq!(json_of(&paths.install)["previousAgent"], "claude");
    uninstall_with(paths, &Env::new(), as_claude);
    assert_eq!(
        claude.current(),
        Agent {
            value: "claude".to_owned(),
            default: false
        }
    );

    let gemini = FakeFerry::new("gemini", false);
    let run = |program: &Path, args: &[&str], _: &Env| gemini.run(program, args);
    let as_gemini = FerryAccess {
        find: Some(&on_path),
        run: Some(&run),
    };
    let (planned, _) = install_with(paths, EXE, &Env::new(), as_gemini);
    assert_eq!(
        agent_items(&planned),
        [("keep", None, Some("gemini".to_owned()), None)]
    );
    assert!(
        planned
            .notes
            .iter()
            .any(|n| n.contains("ferry config agent baton"))
    );
    assert_eq!(gemini.current().value, "gemini");
    assert!(json_of(&paths.install).get("previousAgent").is_none());
    // Nothing of Baton's to put back.
    assert!(agent_items(&uninstall_with(paths, &Env::new(), as_gemini).0).is_empty());
}

#[test]
fn the_agent_ferry_opens_is_changed_by_someone_else_and_uninstall_leaves_it() {
    let (home, _, _) = ferry_home();
    let paths = &home.paths;
    let ferry = FakeFerry::new("default", true);
    let run = |program: &Path, args: &[&str], _: &Env| ferry.run(program, args);
    let with = FerryAccess {
        find: Some(&on_path),
        run: Some(&run),
    };
    install_with(paths, EXE, &Env::new(), with);
    ferry.run(Path::new(FERRY_EXE), &["config", "agent", "gemini"]);
    let (planned, _) = uninstall_with(paths, &Env::new(), with);
    assert_eq!(agent_items(&planned), [("absent", None, None, None)]);
    assert_eq!(ferry.current().value, "gemini");
}

#[test]
fn without_ferry_on_path_or_when_it_does_not_answer_the_agent_is_left_for_the_user() {
    let (home, _, _) = ferry_home();
    let paths = &home.paths;
    assert_eq!(
        agent_items(&plan(paths)),
        [("skip", None, None, Some("ferry is not on PATH"))]
    );
    let mute = |_: &Path, _: &[&str], _: &Env| FerryRun {
        status: Some(1),
        stdout: String::new(),
        stderr: "no".to_owned(),
    };
    let muted = plan_with(
        paths,
        EXE,
        &Env::new(),
        FerryAccess {
            find: Some(&on_path),
            run: Some(&mute),
        },
    );
    assert_eq!(
        agent_items(&muted),
        [("skip", None, None, Some("ferry config agent did not answer"))]
    );
    // A `ferry config agent baton` that fails is a note, and the rest is done.
    let ferry = FakeFerry::new("default", true);
    let failing = |program: &Path, args: &[&str], _: &Env| {
        if args.get(2) == Some(&"baton") {
            FerryRun {
                status: Some(1),
                stdout: String::new(),
                stderr: "the store is locked\n".to_owned(),
            }
        } else {
            ferry.run(program, args)
        }
    };
    let (planned, applied) = install_with(
        paths,
        EXE,
        &Env::new(),
        FerryAccess {
            find: Some(&on_path),
            run: Some(&failing),
        },
    );
    assert!(applied.changed);
    assert_eq!(
        agent_items(&planned),
        [("failed", Some("default".to_owned()), None, None)]
    );
    assert!(planned.notes.iter().any(|n| {
        n == "ferry config agent baton failed: the store is locked. The rest is done; run it yourself to make Baton the agent Ferry opens."
    }));
    assert!(paths.install.exists());
    // Uninstall without ferry on PATH says how to put the one before back.
    let (un, _) = uninstall(paths);
    assert_eq!(
        agent_items(&un),
        [("skip", None, None, Some("ferry is not on PATH"))]
    );
    assert!(
        un.notes
            .iter()
            .any(|n| n.contains("ferry config agent default puts the one before back"))
    );
}
