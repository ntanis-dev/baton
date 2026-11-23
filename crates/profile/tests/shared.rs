//! The files of `~/.claude` every profile shares, where profiles hold copies of them (no file links):
//! settings setting by setting, `CLAUDE.md` line by line, the prompt history as everyone's lines.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once, as the tests do"
)]

mod common;

use std::fs;
use std::path::PathBuf;

use baton_core::config::{By, Config, load_config};
use baton_core::fsutil::read_json;
use baton_core::paths::{Paths, profile_dir};
use baton_profile::accounts::remove_account;
use baton_profile::profile::{LinkOptions, can_symlink_files, inspect_profile, link_profile};
use baton_profile::shared::{
    SharedReport, every_question, shared_questions, sync_everything, sync_shared, take_everything,
};
use baton_profile::sync::{Apply, HOME, SyncReport};
use common::{TestHome, temp_home, write, write_json};
use serde_json::{Value, json};

const T0: i64 = 1_780_000_000_000;

/// Linking with full sync off and no file links: the shared files are copies either way, and the
/// profiles' `.claude.json` has tests of its own.
fn copy_mode() -> LinkOptions {
    LinkOptions {
        full_sync: Some(false),
        ..LinkOptions::file_links(false)
    }
}

/// A home with three accounts, of which acct2 and acct3 are linked: `~/.claude` has settings, a
/// `CLAUDE.md` and a prompt history, and each profile a copy of them.
fn setup() -> TestHome {
    let home = temp_home();
    let paths = &home.paths;
    write_json(
        &paths.config,
        &json!({ "accounts": [
            { "id": "acct2", "name": "Two" }, { "id": "acct3", "name": "Three" }, { "id": "acct4", "name": "Four" }
        ], "policy": { "fullSync": false } }),
    );
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "dark", "permissions": { "allow": ["Read"] }, "env": { "A": "1" } }),
    );
    write(
        &paths.claude_dir.join("CLAUDE.md"),
        "# Notes\n\none\ntwo\nthree\n",
    );
    write(
        &paths.claude_dir.join("history.jsonl"),
        &history(&[(1, "s1", "first"), (2, "s1", "second")]),
    );
    for name in ["acct2", "acct3"] {
        link_profile(paths, name, &copy_mode()).unwrap();
    }
    home
}

/// The file `name` of `~/.claude` ([`HOME`]) or of a profile.
fn file_of(paths: &Paths, who: &str, name: &str) -> PathBuf {
    if who == HOME {
        paths.claude_dir.join(name)
    } else {
        profile_dir(paths, who).join(name)
    }
}

fn settings(paths: &Paths, who: &str) -> Value {
    read_json(&file_of(paths, who, "settings.json")).unwrap()
}

fn text(paths: &Paths, who: &str, name: &str) -> String {
    fs::read_to_string(file_of(paths, who, name)).unwrap()
}

/// Lines of a prompt history as Claude Code writes them: when, in which conversation, what.
fn history(lines: &[(i64, &str, &str)]) -> String {
    lines
        .iter()
        .map(|(at, session, display)| {
            json!({ "display": display, "pastedContents": {}, "timestamp": at, "sessionId": session })
                .to_string()
                + "\n"
        })
        .collect()
}

/// What a history's lines say, in order.
fn said(paths: &Paths, who: &str) -> Vec<String> {
    text(paths, who, "history.jsonl")
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|entry| entry["display"].as_str().map(str::to_owned))
                .unwrap_or_else(|| line.to_owned())
        })
        .collect()
}

fn config(paths: &Paths) -> Config {
    load_config(paths)
}

/// Every holder brought in step now.
fn sync_all(paths: &Paths) -> SharedReport {
    sync_shared(paths, &config(paths), Apply::All, T0, Some(false)).unwrap()
}

fn of<'a>(report: &'a SharedReport, file: &str) -> &'a SyncReport {
    report.of(file).unwrap()
}

fn action(report: &[baton_profile::profile::LinkEntry], name: &str) -> &'static str {
    report.iter().find(|entry| entry.name == name).unwrap().action
}

#[test]
fn a_new_profile_gets_a_copy_of_every_shared_file() {
    let home = setup();
    let paths = &home.paths;
    for who in ["acct2", "acct3"] {
        assert_eq!(settings(paths, who), settings(paths, HOME), "{who}");
        assert_eq!(text(paths, who, "CLAUDE.md"), "# Notes\n\none\ntwo\nthree\n");
        assert_eq!(said(paths, who), ["first", "second"]);
    }
    let report = link_profile(paths, "acct4", &copy_mode()).unwrap();
    for name in ["settings.json", "CLAUDE.md", "history.jsonl"] {
        assert_eq!(action(&report, name), "copied", "{name}");
    }
    assert_eq!(action(&report, "keybindings.json"), "no-source");
    let again = link_profile(paths, "acct4", &copy_mode()).unwrap();
    assert_eq!(action(&again, "settings.json"), "ok");
    let status = |name: &str| {
        inspect_profile(paths, "acct4", Some(false))
            .into_iter()
            .find(|item| item.name == name)
            .unwrap()
            .status
    };
    assert_eq!(status("settings.json"), "copy-fresh");
    assert_eq!(status("history.jsonl"), "copy-fresh");
    assert_eq!(status("keybindings.json"), "no-source");
    assert!(shared_questions(paths).is_empty());
}

#[test]
fn a_setting_changed_under_an_account_reaches_the_others_whatever_else_changed_meanwhile() {
    let home = setup();
    let paths = &home.paths;
    // /config and /permissions under acct2 rewrite that profile's settings.json...
    write_json(
        &file_of(paths, "acct2", "settings.json"),
        &json!({ "theme": "light", "permissions": { "allow": ["Read", "Edit"] }, "env": { "A": "1" } }),
    );
    // ...and meanwhile `baton install` (or the operator) changed ~/.claude's: another key, another
    // entry. With whole files this was one against the other; setting by setting nothing is lost.
    write_json(
        &file_of(paths, HOME, "settings.json"),
        &json!({
            "theme": "dark", "permissions": { "allow": ["Read"] }, "env": { "A": "1", "B": "2" },
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "baton hook stop" }] }] }
        }),
    );
    // The conversation moves to acct3: it and ~/.claude's own are written, acct2 when its turn comes.
    let report = link_profile(paths, "acct3", &copy_mode()).unwrap();
    assert_eq!(action(&report, "settings.json"), "synced");
    let everything = json!({
        "theme": "light", "permissions": { "allow": ["Read", "Edit"] }, "env": { "A": "1", "B": "2" },
        "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "baton hook stop" }] }] }
    });
    assert_eq!(settings(paths, "acct3"), everything);
    assert_eq!(settings(paths, HOME), everything);
    assert_eq!(settings(paths, "acct2")["hooks"], Value::Null, "not its turn yet");
    link_profile(paths, "acct2", &copy_mode()).unwrap();
    assert_eq!(settings(paths, "acct2"), everything);
    assert!(shared_questions(paths).is_empty());
    // What ~/.claude's file held before the sync changed it is kept.
    let kept: Vec<PathBuf> = fs::read_dir(&paths.backups)
        .unwrap()
        .flatten()
        .map(|folder| folder.path().join("sync-settings.json"))
        .filter(|file| file.exists())
        .collect();
    assert_eq!(kept.len(), 1);
    assert_eq!(read_json(&kept[0]).unwrap()["theme"], "dark");
    // What is taken out under one account is taken out everywhere.
    write_json(
        &file_of(paths, "acct3", "settings.json"),
        &json!({
            "theme": "light", "permissions": { "allow": ["Read"] }, "env": { "B": "2" },
            "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "baton hook stop" }] }] }
        }),
    );
    let report = sync_all(paths);
    assert!(of(&report, "settings.json").questions.is_empty());
    for who in [HOME, "acct2", "acct3"] {
        let own = settings(paths, who);
        assert_eq!(own["permissions"]["allow"], json!(["Read"]), "{who}");
        assert_eq!(own["env"], json!({ "B": "2" }), "{who}");
    }
}

#[test]
fn the_same_setting_changed_in_two_ways_is_a_question_and_nobody_is_written_over() {
    let home = setup();
    let paths = &home.paths;
    let with_theme =
        |theme: &str| json!({ "theme": theme, "permissions": { "allow": ["Read"] }, "env": { "A": "1" } });
    write_json(&file_of(paths, "acct2", "settings.json"), &with_theme("light"));
    write_json(&file_of(paths, "acct3", "settings.json"), &with_theme("blue"));
    let report = sync_all(paths);
    let questions = &of(&report, "settings.json").questions;
    assert_eq!(questions.len(), 1);
    assert_eq!(questions[0].label, "settings.json · theme");
    assert_eq!(settings(paths, "acct2")["theme"], "light");
    assert_eq!(settings(paths, "acct3")["theme"], "blue");
    // The first to change it reached ~/.claude's; the question stays until it is answered.
    assert_eq!(settings(paths, HOME)["theme"], "light");
    assert_eq!(shared_questions(paths), *questions);
    assert_eq!(every_question(paths, &config(paths)), *questions);
    let linked = link_profile(paths, "acct3", &copy_mode()).unwrap();
    assert_eq!(action(&linked, "settings.json"), "conflict");
    let answered = take_everything(
        paths,
        &config(paths),
        &[questions[0].leaf.clone()],
        "acct3",
        T0 + 1,
    )
    .unwrap();
    assert!(answered.questions.is_empty());
    for who in [HOME, "acct2", "acct3"] {
        assert_eq!(settings(paths, who)["theme"], "blue", "{who}");
    }
    assert!(shared_questions(paths).is_empty());
}

#[test]
fn keybindings_are_kept_in_step_too_and_a_copy_that_cannot_be_read_is_left_alone() {
    let home = setup();
    let paths = &home.paths;
    // Made under acct2, where ~/.claude has none yet: it becomes everyone's.
    let bindings = json!({ "bindings": [{ "context": "Chat", "bindings": { "ctrl+k": "chat:clear" } }] });
    write_json(&file_of(paths, "acct2", "keybindings.json"), &bindings);
    fs::write(file_of(paths, "acct3", "settings.json"), "{ half").unwrap();
    let report = sync_all(paths);
    assert_eq!(
        read_json(&file_of(paths, HOME, "keybindings.json")).unwrap(),
        bindings
    );
    assert_eq!(
        read_json(&file_of(paths, "acct3", "keybindings.json")).unwrap(),
        bindings
    );
    assert_eq!(of(&report, "settings.json").unreadable, ["acct3"]);
    assert_eq!(text(paths, "acct3", "settings.json"), "{ half");
    let linked = link_profile(paths, "acct3", &copy_mode()).unwrap();
    assert_eq!(action(&linked, "settings.json"), "skipped");
}

#[test]
fn claude_md_has_what_each_account_changed_and_the_same_lines_changed_twice_are_a_question() {
    let home = setup();
    let paths = &home.paths;
    write(
        &file_of(paths, "acct2", "CLAUDE.md"),
        "# Notes\n\nONE\ntwo\nthree\n",
    );
    write(
        &file_of(paths, "acct3", "CLAUDE.md"),
        "# Notes\n\none\ntwo\nthree\nfour\n",
    );
    let report = sync_all(paths);
    assert!(of(&report, "CLAUDE.md").questions.is_empty());
    for who in [HOME, "acct2", "acct3"] {
        assert_eq!(
            text(paths, who, "CLAUDE.md"),
            "# Notes\n\nONE\ntwo\nthree\nfour\n",
            "{who}"
        );
    }
    assert!(
        sync_all(paths).of("CLAUDE.md").unwrap().written.is_empty(),
        "in step"
    );
    // The same line, two ways.
    write(
        &file_of(paths, "acct2", "CLAUDE.md"),
        "# Notes\n\nONE\ntwo\n3\nfour\n",
    );
    write(
        &file_of(paths, HOME, "CLAUDE.md"),
        "# Notes\n\nONE\ntwo\nTHREE\nfour\n",
    );
    let report = sync_all(paths);
    let questions = &of(&report, "CLAUDE.md").questions;
    assert_eq!(questions.len(), 1);
    assert_eq!(
        (questions[0].leaf.as_str(), questions[0].label.as_str()),
        ("CLAUDE.md", "CLAUDE.md")
    );
    let holders: Vec<&str> = questions[0].values.iter().map(|held| held.who.as_str()).collect();
    assert_eq!(holders, [HOME, "acct2", "acct3"]);
    assert_ne!(questions[0].values[0].value, questions[0].values[1].value);
    assert!(
        text(paths, "acct2", "CLAUDE.md").contains("\n3\n"),
        "nobody is written over"
    );
    assert!(text(paths, HOME, "CLAUDE.md").contains("THREE"));
    take_everything(paths, &config(paths), &["CLAUDE.md".to_owned()], "acct2", T0 + 1).unwrap();
    for who in [HOME, "acct2", "acct3"] {
        assert_eq!(
            text(paths, who, "CLAUDE.md"),
            "# Notes\n\nONE\ntwo\n3\nfour\n",
            "{who}"
        );
    }
    assert!(shared_questions(paths).is_empty());
    // A file that is gone under one account comes back; one that was emptied is empty everywhere.
    fs::remove_file(file_of(paths, "acct3", "CLAUDE.md")).unwrap();
    sync_all(paths);
    assert_eq!(
        text(paths, "acct3", "CLAUDE.md"),
        "# Notes\n\nONE\ntwo\n3\nfour\n"
    );
    write(&file_of(paths, "acct3", "CLAUDE.md"), "");
    sync_all(paths);
    assert_eq!(text(paths, HOME, "CLAUDE.md"), "");
}

#[test]
fn the_prompt_history_is_every_accounts_lines_each_once_in_the_order_they_were_typed() {
    let home = setup();
    let paths = &home.paths;
    let more = |who: &str, lines: &[(i64, &str, &str)]| {
        let file = file_of(paths, who, "history.jsonl");
        let mut all = fs::read_to_string(&file).unwrap();
        all.push_str(&history(lines));
        write(&file, &all);
    };
    more("acct2", &[(3, "s2", "under two"), (6, "s2", "two again")]);
    more("acct3", &[(4, "s3", "under three")]);
    more(HOME, &[(5, "s0", "plain claude")]);
    let report = sync_all(paths);
    let all = [
        "first",
        "second",
        "under two",
        "under three",
        "plain claude",
        "two again",
    ];
    for who in [HOME, "acct2", "acct3"] {
        assert_eq!(said(paths, who), all, "{who}");
    }
    assert_eq!(of(&report, "history.jsonl").written.len(), 3);
    assert!(
        sync_all(paths).of("history.jsonl").unwrap().written.is_empty(),
        "in step"
    );
    // The same prompt in the same conversation at the same moment is one line; the same words later
    // are another. A line that is not Claude Code's stays where it was.
    more("acct2", &[(6, "s2", "two again"), (7, "s2", "two again")]);
    let mut odd = fs::read_to_string(file_of(paths, "acct3", "history.jsonl")).unwrap();
    odd.push_str("not json\n");
    write(&file_of(paths, "acct3", "history.jsonl"), &odd);
    sync_all(paths);
    let mut expected: Vec<&str> = all.to_vec();
    expected.extend(["not json", "two again"]);
    assert_eq!(said(paths, HOME), expected);
    assert_eq!(said(paths, "acct2"), expected);
    // Only the account that is about to be used, and ~/.claude's own, are written at a hop.
    more("acct3", &[(9, "s3", "late")]);
    sync_shared(paths, &config(paths), Apply::One("acct2"), T0, Some(false)).unwrap();
    assert_eq!(said(paths, "acct2").last().unwrap(), "late");
    assert_eq!(said(paths, HOME).last().unwrap(), "late");
}

#[test]
fn baton_sync_covers_the_shared_files_and_a_removed_account_is_forgotten() {
    let home = setup();
    let paths = &home.paths;
    write_json(
        &file_of(paths, "acct2", "settings.json"),
        &json!({ "theme": "light", "permissions": { "allow": ["Read"] }, "env": { "A": "1" } }),
    );
    // Full sync is off here: the shared files are kept in step all the same.
    let report = sync_everything(paths, &config(paths), Apply::All, T0).unwrap();
    assert!(
        report
            .brought
            .iter()
            .any(|brought| brought.label == "settings.json · theme" && brought.from == "acct2")
    );
    assert_eq!(
        report
            .written
            .iter()
            .map(|written| written.account.as_str())
            .collect::<Vec<_>>(),
        ["acct3"],
        "~/.claude's own goes without saying"
    );
    assert_eq!(settings(paths, "acct3")["theme"], "light");
    // Two that differ, then one of them is removed: its side of the question goes with it.
    write_json(
        &file_of(paths, "acct2", "settings.json"),
        &json!({ "theme": "a" }),
    );
    write_json(
        &file_of(paths, "acct3", "settings.json"),
        &json!({ "theme": "b" }),
    );
    assert!(!sync_all(paths).of("settings.json").unwrap().questions.is_empty());
    remove_account(paths, "acct3", By::Id).unwrap();
    let left = shared_questions(paths);
    assert!(
        left.iter()
            .all(|question| question.values.iter().all(|held| held.who != "acct3"))
    );
}

#[test]
fn a_copy_becomes_a_link_once_links_are_allowed_and_what_was_changed_in_it_is_kept() {
    let home = setup();
    let paths = &home.paths;
    if !can_symlink_files(&paths.tmp) {
        return; // Windows without Developer Mode: there is nothing to turn into a link.
    }
    write_json(
        &file_of(paths, "acct2", "settings.json"),
        &json!({ "theme": "light", "permissions": { "allow": ["Read"] }, "env": { "A": "1" } }),
    );
    write(
        &file_of(paths, "acct2", "CLAUDE.md"),
        "# Notes\n\none\ntwo\nthree\nmine\n",
    );
    let links = LinkOptions {
        full_sync: Some(false),
        ..LinkOptions::file_links(true)
    };
    let report = link_profile(paths, "acct2", &links).unwrap();
    for name in ["settings.json", "CLAUDE.md", "history.jsonl"] {
        assert_eq!(action(&report, name), "linked", "{name}");
        assert!(
            fs::symlink_metadata(file_of(paths, "acct2", name))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    assert_eq!(
        settings(paths, HOME)["theme"],
        "light",
        "its change went to ~/.claude's first"
    );
    assert!(text(paths, HOME, "CLAUDE.md").ends_with("mine\n"));
    // One that waits for an answer stays a copy until it has one.
    write_json(
        &file_of(paths, "acct3", "settings.json"),
        &json!({ "theme": "blue" }),
    );
    write_json(
        &file_of(paths, HOME, "settings.json"),
        &json!({ "theme": "green" }),
    );
    let report = link_profile(paths, "acct3", &links).unwrap();
    assert_eq!(action(&report, "settings.json"), "conflict");
    assert_eq!(settings(paths, "acct3")["theme"], "blue");
}
