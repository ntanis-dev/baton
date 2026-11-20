//! `baton install` and `baton uninstall` over a temporary home: what is planned, what is written,
//! what is kept, and that an install made by the Node version of Baton is replaced and removed.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use std::fs;
use std::path::{Path, PathBuf};

use baton_core::clock::now_ms;
use baton_core::env::Env;
use baton_core::paths::Paths;
use baton_install::ferry::FerryAccess;
use baton_install::install::{
    Applied, HOP_MARKER, Plan, apply_plan, hop_command_path, hop_command_text, install_state,
    is_baton_handler, plan_install, plan_uninstall, settings_path, status_line_command,
};
use baton_testkit::{temp_home, write, write_json};
use serde_json::{Value, json};

/// A baton executable in a folder with a space in its name.
const EXE: &str = "C:/Program Files/Baton/baton.exe";
/// The same program after it moved.
const MOVED: &str = "C:/tools/baton-moved/baton.exe";

/// No Ferry, whatever is on this machine (tests/ferry.rs has it).
fn nowhere() -> Option<PathBuf> {
    None
}

fn no_ferry() -> FerryAccess<'static> {
    FerryAccess {
        find: Some(&nowhere),
        run: None,
    }
}

fn read(file: &Path) -> String {
    fs::read_to_string(file).unwrap()
}

fn json_of(file: &Path) -> Value {
    serde_json::from_str(&read(file)).unwrap()
}

fn plan(paths: &Paths, exe: &str) -> Plan {
    plan_install(paths, Path::new(exe), &Env::new(), no_ferry()).unwrap()
}

fn install(paths: &Paths, exe: &str) -> (Plan, Applied) {
    let mut plan = plan(paths, exe);
    let applied = apply_plan(&mut plan, paths, now_ms(), &Env::new(), None).unwrap();
    (plan, applied)
}

fn uninstall(paths: &Paths) -> (Plan, Applied) {
    let mut plan = plan_uninstall(paths, &Env::new(), no_ferry()).unwrap();
    let applied = apply_plan(&mut plan, paths, now_ms(), &Env::new(), None).unwrap();
    (plan, applied)
}

/// Every handler of an event's groups.
fn handlers(settings: &Value, event: &str) -> Vec<Value> {
    settings["hooks"][event]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["hooks"].as_array().unwrap().clone())
        .collect()
}

const EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

#[test]
fn dry_run_the_plan_shows_the_changes_and_writes_nothing() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(&settings_path(paths), &json!({ "theme": "dark" }));
    let plan = plan(paths, EXE);
    assert_eq!(plan.files.len(), 2);
    assert!(
        plan.files[0]
            .diff
            .iter()
            .any(|l| l.starts_with('+') && l.contains("statusline"))
    );
    assert_eq!(json_of(&settings_path(paths)), json!({ "theme": "dark" }));
    assert!(!hop_command_path(paths).exists());
    assert!(!paths.install.exists());
}

#[test]
fn install_into_settings_without_a_status_line_or_hooks_keeping_the_other_keys() {
    let home = temp_home();
    let paths = &home.paths;
    let original = json!({ "theme": "dark", "permissions": { "allow": ["Bash(ls:*)"] } });
    write_json(&settings_path(paths), &original);
    let (_, applied) = install(paths, EXE);
    assert!(applied.changed);
    let s = json_of(&settings_path(paths));
    assert_eq!(s["theme"], "dark");
    assert_eq!(s["permissions"], json!({ "allow": ["Bash(ls:*)"] }));
    assert_eq!(
        s["statusLine"],
        json!({ "type": "command", "command": status_line_command(Path::new(EXE)) })
    );
    assert!(
        s["statusLine"]["command"].as_str().unwrap().starts_with("baton "),
        "a baton path with spaces runs as baton from PATH"
    );
    for event in EVENTS {
        let all = handlers(&s, event);
        assert_eq!(all.iter().filter(|h| is_baton_handler(h)).count(), 1, "{event}");
        assert_eq!(all[0]["command"], EXE);
        assert_eq!(all[0]["args"][0], "hook");
        assert_eq!(all[0]["timeout"], 10);
    }
    assert!(
        s["hooks"]["StopFailure"][0].get("matcher").is_none(),
        "every API error ends a turn"
    );
    let hop = read(&hop_command_path(paths));
    assert!(hop.contains(HOP_MARKER));
    assert!(hop.contains("${CLAUDE_SESSION_ID}"));
    assert!(hop.contains("--to='$ARGUMENTS'"), "a name with spaces stays one");
    let record = json_of(&paths.install);
    assert_eq!(record["previousStatusLine"], Value::Null);
    assert_eq!(record["exe"], EXE);
    assert_eq!(record["hooks"], json!(EVENTS));
    let backup = applied.backup.unwrap();
    assert_eq!(json_of(&backup.join("settings.json")), original);
    let state = install_state(paths);
    assert!(state.ok && state.status_line && state.hop_command && !state.node);
    assert!(state.hooks.iter().all(|hook| hook.installed && !hook.node));
    assert_eq!(
        state.hooks.iter().map(|hook| hook.event).collect::<Vec<_>>(),
        EVENTS
    );
    assert_eq!(state.record, Some(record));
}

#[test]
fn install_wraps_an_existing_status_line_and_keeps_existing_hooks_and_again_is_a_no_op() {
    let home = temp_home();
    let paths = &home.paths;
    let mine = json!({ "type": "command", "command": "~/.claude/statusline.sh", "padding": 2, "refreshInterval": 5 });
    let my_hook = json!({ "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo pre" }] });
    let my_stop = json!({ "hooks": [{ "type": "command", "command": "notify-send done" }] });
    write_json(
        &settings_path(paths),
        &json!({ "statusLine": mine, "hooks": { "PreToolUse": [my_hook], "Stop": [my_stop] } }),
    );
    let (first, _) = install(paths, EXE);
    assert!(first.notes.iter().any(|n| n.contains("status line")));
    let s = json_of(&settings_path(paths));
    assert_eq!(s["statusLine"]["padding"], 2);
    assert_eq!(s["statusLine"]["refreshInterval"], 5);
    assert_ne!(s["statusLine"]["command"], mine["command"]);
    assert_eq!(json_of(&paths.install)["previousStatusLine"], mine);
    assert_eq!(s["hooks"]["PreToolUse"], json!([my_hook]));
    assert_eq!(s["hooks"]["Stop"][0], my_stop);
    assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 2);
    let text = read(&settings_path(paths));
    let (second, applied) = install(paths, EXE);
    assert!(!applied.changed);
    assert!(second.files.is_empty());
    assert_eq!(read(&settings_path(paths)), text);
    assert_eq!(
        json_of(&paths.install)["previousStatusLine"],
        mine,
        "the wrapped status line is remembered across installs"
    );
}

#[test]
fn install_follows_baton_moving_its_entries_are_replaced_not_duplicated() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(&settings_path(paths), &json!({}));
    install(paths, "C:/tools/baton/baton.exe");
    let (moved, applied) = install(paths, MOVED);
    assert!(applied.changed);
    let action = |name: &str| moved.items.iter().find(|item| item.name == name).unwrap().action;
    assert_eq!(
        (action("hooks"), action("statusline"), action("hop")),
        ("update", "update", "update")
    );
    let s = json_of(&settings_path(paths));
    for event in EVENTS {
        let all = handlers(&s, event);
        assert_eq!(all.iter().filter(|h| is_baton_handler(h)).count(), 1);
        assert_eq!(all[0]["command"], MOVED);
    }
    assert!(
        s["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("baton-moved")
    );
    assert_eq!(read(&hop_command_path(paths)), hop_command_text(Path::new(MOVED)));
}

#[test]
fn formatting_is_kept_indentation_crlf_and_the_trailing_newline() {
    let home = temp_home();
    let paths = &home.paths;
    write(&settings_path(paths), "{\r\n    \"theme\": \"dark\"\r\n}\r\n");
    install(paths, EXE);
    let text = read(&settings_path(paths));
    assert!(
        text.starts_with("{\r\n    \"theme\": \"dark\",\r\n    \"hooks\": {\r\n        \"SessionStart\""),
        "{text}"
    );
    assert!(text.ends_with("}\r\n"));
    assert!(!text.replace("\r\n", "").contains('\n'));
}

#[test]
fn uninstall_restores_the_previous_status_line_and_removes_exactly_batons_entries() {
    let home = temp_home();
    let paths = &home.paths;
    let mine = json!({ "type": "command", "command": "~/.claude/statusline.sh" });
    let my_stop = json!({ "hooks": [{ "type": "command", "command": "notify-send done" }] });
    let original = json!({ "theme": "dark", "statusLine": mine, "hooks": { "Stop": [my_stop] } });
    write_json(&settings_path(paths), &original);
    let before = read(&settings_path(paths));
    install(paths, EXE);
    let mut plan = plan_uninstall(paths, &Env::new(), no_ferry()).unwrap();
    assert_eq!(plan.files.len(), 2);
    let applied = apply_plan(&mut plan, paths, now_ms(), &Env::new(), None).unwrap();
    assert!(applied.changed);
    assert_eq!(json_of(&settings_path(paths)), original);
    assert_eq!(read(&settings_path(paths)), before, "key for key, as it was");
    assert!(!hop_command_path(paths).exists());
    assert!(!paths.install.exists());
    assert!(applied.backup.unwrap().join("settings.json").exists());
    assert!(!uninstall(paths).1.changed);
}

#[test]
fn uninstall_without_a_previous_status_line_removes_batons_and_a_new_settings_json_is_left_empty() {
    let home = temp_home();
    let paths = &home.paths;
    install(paths, EXE);
    assert!(json_of(&settings_path(paths))["statusLine"].is_object());
    let (plan, _) = uninstall(paths);
    let statusline = plan.items.iter().find(|item| item.name == "statusline").unwrap();
    assert_eq!((statusline.action, statusline.restored), ("remove", Some(false)));
    assert_eq!(json_of(&settings_path(paths)), json!({}));
    let state = install_state(paths);
    assert!(state.ok && !state.status_line && !state.hop_command);
    assert!(state.hooks.iter().all(|hook| !hook.installed));
    assert_eq!(state.record, None);
}

#[test]
fn an_operators_own_hop_command_is_left_alone() {
    let home = temp_home();
    let paths = &home.paths;
    write(&hop_command_path(paths), "my own hop\n");
    let (plan, _) = install(paths, EXE);
    assert!(plan.notes.iter().any(|n| n.contains("your own command")));
    assert_eq!(
        plan.items.iter().find(|item| item.name == "hop").unwrap().action,
        "skip"
    );
    assert_eq!(read(&hop_command_path(paths)), "my own hop\n");
    uninstall(paths);
    assert_eq!(read(&hop_command_path(paths)), "my own hop\n");
}

#[test]
fn settings_json_that_is_not_json_is_refused() {
    let home = temp_home();
    let paths = &home.paths;
    write(&settings_path(paths), "{ nope");
    let error = plan_install(paths, Path::new(EXE), &Env::new(), no_ferry()).unwrap_err();
    assert!(error.to_string().contains("not valid JSON"), "{error}");
    assert!(error.to_string().ends_with("; fix it first."), "{error}");
    assert!(plan_uninstall(paths, &Env::new(), no_ferry()).is_err());
    write(&settings_path(paths), "[1, 2]");
    let error = plan_install(paths, Path::new(EXE), &Env::new(), no_ferry()).unwrap_err();
    assert!(
        error.to_string().contains("not valid JSON (not an object)"),
        "{error}"
    );
    write(&settings_path(paths), "{ nope");
    assert_eq!(read(&settings_path(paths)), "{ nope");
    assert!(!home.home.join(".baton").exists());
    let state = install_state(paths);
    assert!(!state.ok);
    assert_eq!(state.error.as_deref(), Some("settings.json is not valid JSON"));
}

// ---- An install made by the Node version of Baton --------------------------------------------------

const NODE: &str = "C:/nowhere/Program Files/nodejs/node.exe";
const CLI: &str = "E:/nowhere/baton/src/cli.js";

/// What the Node version of Baton left in a home: its hooks beside the operator's own, its status
/// line wrapping theirs, its /hop command and its record.
fn node_install(paths: &Paths, mine: &Value, my_stop: &Value) {
    let node_hook = |event: &str| json!({ "hooks": [{ "type": "command", "command": NODE, "args": [CLI, "hook", event], "timeout": 10 }] });
    write_json(
        &settings_path(paths),
        &json!({
            "theme": "dark",
            "statusLine": { "type": "command", "command": format!("node \"{CLI}\" statusline") },
            "hooks": {
                "Stop": [my_stop, node_hook("stop")],
                "SessionStart": [node_hook("session-start")],
                "UserPromptSubmit": [node_hook("prompt")],
                "StopFailure": [node_hook("stop-failure")],
                "SessionEnd": [node_hook("session-end")]
            }
        }),
    );
    write(
        &hop_command_path(paths),
        &format!(
            "---\ndescription: Move this conversation to another account at the end of this turn {HOP_MARKER}\nargument-hint: \"[account]\"\nallowed-tools: Bash(node:*)\ndisable-model-invocation: true\n---\n!`node \"{CLI}\" hop --session \"${{CLAUDE_SESSION_ID}}\" --from-command --to='$ARGUMENTS'`\n\nBaton was asked.\n"
        ),
    );
    write_json(
        &paths.install,
        &json!({
            "version": 1,
            "cli": CLI,
            "node": NODE,
            "settingsPath": settings_path(paths).to_string_lossy(),
            "commandPath": hop_command_path(paths).to_string_lossy(),
            "previousStatusLine": mine,
            "hooks": EVENTS,
            "installedAt": "2026-10-01T10:00:00.000Z",
            "backup": "C:/somewhere/backups/2026-10-01T10-00-00-000Z"
        }),
    );
}

#[test]
fn an_install_by_the_node_version_is_seen_and_replaced_in_place() {
    let home = temp_home();
    let paths = &home.paths;
    let mine = json!({ "type": "command", "command": "~/.claude/statusline.sh" });
    let my_stop = json!({ "hooks": [{ "type": "command", "command": "notify-send done" }] });
    node_install(paths, &mine, &my_stop);
    let state = install_state(paths);
    assert!(state.ok && state.status_line && state.hop_command && state.node);
    assert!(
        state
            .hooks
            .iter()
            .all(|hook| hook.installed && hook.node && !hook.program_exists)
    );

    let (plan, applied) = install(paths, EXE);
    assert!(applied.changed);
    let action = |name: &str| plan.items.iter().find(|item| item.name == name).unwrap().action;
    assert_eq!(
        (action("hooks"), action("statusline"), action("hop")),
        ("update", "update", "update")
    );
    assert!(
        plan.notes.iter().all(|n| n.contains("from PATH")),
        "nothing of the operator's is touched: {:?}",
        plan.notes
    );
    let s = json_of(&settings_path(paths));
    for event in EVENTS {
        let all = handlers(&s, event);
        let ours: Vec<&Value> = all.iter().filter(|h| is_baton_handler(h)).collect();
        assert_eq!(ours.len(), 1, "{event}");
        assert_eq!(ours[0]["command"], EXE, "{event}");
        assert_eq!(ours[0]["args"][0], "hook", "{event}");
    }
    assert_eq!(
        s["hooks"]["Stop"][0], my_stop,
        "the operator's own hook stays, and stays first"
    );
    assert_eq!(
        s["statusLine"],
        json!({ "type": "command", "command": "baton statusline" })
    );
    assert!(!s.to_string().contains("cli.js"));
    let hop = read(&hop_command_path(paths));
    assert_eq!(hop, hop_command_text(Path::new(EXE)));
    let record = json_of(&paths.install);
    assert_eq!(
        record["previousStatusLine"], mine,
        "the operator's status line is still remembered"
    );
    assert_eq!(record["exe"], EXE);
    assert!(record.get("cli").is_none() && record.get("node").is_none());
    let state = install_state(paths);
    assert!(state.status_line && !state.node);
    // Again: nothing to do.
    assert!(!install(paths, EXE).1.changed);
    // And uninstall gives the operator's own back.
    uninstall(paths);
    assert_eq!(
        json_of(&settings_path(paths)),
        json!({ "theme": "dark", "statusLine": mine, "hooks": { "Stop": [my_stop] } })
    );
}

#[test]
fn uninstall_removes_an_install_by_the_node_version() {
    let home = temp_home();
    let paths = &home.paths;
    let mine = json!({ "type": "command", "command": "~/.claude/statusline.sh" });
    let my_stop = json!({ "hooks": [{ "type": "command", "command": "notify-send done" }] });
    node_install(paths, &mine, &my_stop);
    let (plan, applied) = uninstall(paths);
    assert!(applied.changed);
    let item = |name: &str| plan.items.iter().find(|item| item.name == name).unwrap();
    assert_eq!(
        (
            item("hooks").action,
            item("statusline").action,
            item("hop").action
        ),
        ("remove", "remove", "remove")
    );
    assert_eq!(item("statusline").restored, Some(true));
    assert_eq!(
        json_of(&settings_path(paths)),
        json!({ "theme": "dark", "statusLine": mine, "hooks": { "Stop": [my_stop] } })
    );
    assert!(!hop_command_path(paths).exists());
    assert!(!paths.install.exists());
    let backup = applied.backup.unwrap();
    assert!(backup.join("settings.json").exists() && backup.join("hop.md").exists());
}

#[test]
fn a_status_line_changed_after_install_is_left_as_it_is() {
    let home = temp_home();
    let paths = &home.paths;
    let mine = json!({ "type": "command", "command": "~/.claude/statusline.sh" });
    write_json(&settings_path(paths), &json!({ "statusLine": mine }));
    install(paths, EXE);
    let mut settings = json_of(&settings_path(paths));
    settings["statusLine"] = json!({ "type": "command", "command": "another-tool" });
    write_json(&settings_path(paths), &settings);
    let (plan, _) = uninstall(paths);
    assert_eq!(
        plan.items
            .iter()
            .find(|item| item.name == "statusline")
            .unwrap()
            .action,
        "absent"
    );
    assert!(plan.notes.iter().any(|n| n.contains("changed after install")));
    assert_eq!(
        json_of(&settings_path(paths)),
        json!({ "statusLine": { "type": "command", "command": "another-tool" } })
    );
}

#[test]
fn a_baton_whose_path_a_shell_cannot_start_with_runs_from_path_and_the_plan_says_when_it_is_not_there() {
    let home = temp_home();
    let paths = &home.paths;
    let from_path = |plan: &Plan| {
        plan.notes
            .iter()
            .filter(|n| n.contains("run baton from PATH"))
            .count()
    };
    // A path any shell runs as it is: named in full, and nothing to say.
    let plain = plan(paths, "C:/tools/baton/baton.exe");
    assert_eq!(from_path(&plain), 0);
    assert!(
        read_after(&plain, "settings.json").contains("\"command\": \"C:/tools/baton/baton.exe statusline\"")
    );
    // A space in it: `baton` from PATH, and a note while PATH has no such baton.
    let spaced = plan(paths, EXE);
    assert_eq!(from_path(&spaced), 1);
    assert!(
        spaced
            .notes
            .iter()
            .any(|n| n.ends_with("add C:/Program Files/Baton to PATH.")),
        "{:?}",
        spaced.notes
    );
    assert!(read_after(&spaced, "settings.json").contains("\"command\": \"baton statusline\""));
    assert!(read_after(&spaced, "hop.md").contains("!`baton hop --session "));
    // The hooks never go through a shell: they name the executable whatever its path.
    assert!(read_after(&spaced, "settings.json").contains(&format!("\"command\": \"{EXE}\"")));
    // On PATH, there is nothing to say.
    let dir = home.home.join("my tools");
    let exe = dir.join(if cfg!(windows) { "baton.exe" } else { "baton" });
    write(&exe, "");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let on_path = Env::from_pairs([("PATH", dir.clone().into_os_string())]);
    let found = plan_install(paths, &exe, &on_path, no_ferry()).unwrap();
    assert_eq!(from_path(&found), 0, "{:?}", found.notes);
    // Another baton on PATH is not this one.
    let other = plan_install(paths, Path::new(EXE), &on_path, no_ferry()).unwrap();
    assert_eq!(from_path(&other), 1);
}

/// What a plan writes to the file with this backup name.
fn read_after(plan: &Plan, backup_name: &str) -> String {
    plan.files
        .iter()
        .find(|f| f.backup_name == backup_name)
        .and_then(|f| f.after.clone())
        .unwrap()
}

#[test]
fn settings_behind_a_symbolic_link_are_written_through_it() {
    let home = temp_home();
    let paths = &home.paths;
    let real = home.home.join("dotfiles").join("settings.json");
    write_json(&real, &json!({ "theme": "dark" }));
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&real, settings_path(paths)).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&real, settings_path(paths)).is_ok();
    if !linked {
        // File links are not allowed here (Windows without Developer Mode).
        return;
    }
    install(paths, EXE);
    assert!(
        fs::symlink_metadata(settings_path(paths))
            .unwrap()
            .file_type()
            .is_symlink(),
        "still a link"
    );
    assert!(json_of(&real)["statusLine"].is_object());
}
