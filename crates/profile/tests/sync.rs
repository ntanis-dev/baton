//! Full sync where it happens: when a profile is linked (an account added, `baton` starting, a
//! conversation moving to another account).

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once, as the tests do"
)]

mod common;

use std::fs;
use std::path::Path;

use baton_core::fsutil::read_json;
use baton_core::json::Object;
use baton_core::paths::{Paths, global_config_file, profile_dir};
use baton_profile::accounts::{AddOptions, add_account, remove_account, set_policy};
use baton_profile::profile::{LinkEntry, LinkOptions, link_profile};
use baton_profile::sync::{read_questions, sync_file};
use common::{temp_home, write, write_json};
use serde_json::{Value, json};

/// Links a profile the way `baton` does before it starts Claude Code on it.
fn link(paths: &Paths, id: &str) -> Vec<LinkEntry> {
    link_profile(paths, id, &LinkOptions::default()).unwrap()
}

/// What linking said of one item.
fn said<'a>(report: &'a [LinkEntry], name: &str) -> (&'a str, Option<&'a str>) {
    let entry = report.iter().find(|entry| entry.name == name).unwrap();
    (entry.action, entry.detail.as_deref())
}

/// Adds an account with nothing special about it.
fn add(paths: &Paths, name: &str) {
    add_account(paths, name, &AddOptions::default()).unwrap();
}

/// An account's `.claude.json`.
fn settings(paths: &Paths, id: &str) -> Value {
    read_json(&global_config_file(paths, id)).unwrap()
}

/// Changes an account's `.claude.json` the way Claude Code would while it runs there.
fn claude_writes(paths: &Paths, id: &str, change: impl FnOnce(&mut Value)) {
    let mut value = read_json(&global_config_file(paths, id)).unwrap_or_else(|| json!({}));
    change(&mut value);
    write_json(&global_config_file(paths, id), &value);
}

/// Whether `path` is a link.
fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

#[test]
fn a_new_account_starts_with_everything_the_first_one_was_given() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "model": "opus" }),
    );
    write_json(
        &paths.claude_json,
        &json!({ "hasCompletedOnboarding": true, "userID": "plain" }),
    );
    add(paths, "first");
    // The first account is set up while Claude Code runs on it: a theme, a server, a trusted folder,
    // an agent, a skill.
    claude_writes(paths, "first", |own| {
        own["userID"] = json!("user-first");
        own["oauthAccount"] = json!({ "emailAddress": "first@example.invalid" });
        own["theme"] = json!("dark");
        own["mcpServers"] = json!({ "files": { "command": "files" } });
        own["projects"] = json!({ "E:/Work": { "hasTrustDialogAccepted": true, "allowedTools": ["Read"] } });
    });
    let first = profile_dir(paths, "first");
    write(&first.join("agents").join("reviewer.md"), "review");
    write(&first.join("skills").join("ship").join("SKILL.md"), "ship");
    assert_eq!(
        fs::read_to_string(paths.claude_dir.join("agents").join("reviewer.md")).unwrap(),
        "review",
        "a shared folder is shared from the start, so what is made in it is everyone's"
    );

    add(paths, "second");
    let second = profile_dir(paths, "second");
    assert_eq!(
        fs::read_to_string(second.join("agents").join("reviewer.md")).unwrap(),
        "review"
    );
    assert_eq!(
        fs::read_to_string(second.join("skills").join("ship").join("SKILL.md")).unwrap(),
        "ship"
    );
    let own = settings(paths, "second");
    assert_eq!(own["theme"], "dark");
    assert_eq!(own["mcpServers"], json!({ "files": { "command": "files" } }));
    assert_eq!(own["projects"]["E:/Work"]["hasTrustDialogAccepted"], true);
    assert_eq!(own["hasCompletedOnboarding"], true);
    assert!(
        own.get("userID").is_none() && own.get("oauthAccount").is_none(),
        "never who the other one is"
    );
    assert_eq!(settings(paths, "first")["userID"], "user-first");
    assert_eq!(
        read_json(&paths.claude_json).unwrap(),
        json!({ "hasCompletedOnboarding": true, "userID": "plain" })
    );
}

#[test]
fn what_is_added_on_one_account_is_there_when_the_conversation_moves_on() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(&paths.claude_json, &json!({ "theme": "dark" }));
    for name in ["one", "two", "three"] {
        add(paths, name);
    }
    // The conversation runs on "one"; a server is added there, and a folder trusted.
    assert_eq!(said(&link(paths, "one"), ".claude.json"), ("ok", Some("in step")));
    claude_writes(paths, "one", |own| {
        own["mcpServers"] = json!({ "github": { "command": "gh" } });
        own["projects"] = json!({ "/work": { "hasTrustDialogAccepted": true } });
    });
    // It moves to "two": that profile is linked before Claude Code starts on it.
    let report = link(paths, "two");
    assert_eq!(said(&report, ".claude.json"), ("synced", Some("2 settings")));
    assert_eq!(
        settings(paths, "two")["mcpServers"],
        json!({ "github": { "command": "gh" } })
    );
    // On "two" the theme changes; the conversation moves on to "three", which has it all.
    claude_writes(paths, "two", |own| own["theme"] = json!("light"));
    link(paths, "three");
    let third = settings(paths, "three");
    assert_eq!(third["theme"], "light");
    assert_eq!(third["mcpServers"], json!({ "github": { "command": "gh" } }));
    assert_eq!(third["projects"]["/work"]["hasTrustDialogAccepted"], true);
    // And back on "one", the theme from "two".
    link(paths, "one");
    assert_eq!(settings(paths, "one")["theme"], "light");
    assert!(read_questions(paths).is_empty());
}

#[test]
fn what_an_account_kept_for_itself_becomes_everyones_unless_the_same_name_is_taken() {
    let home = temp_home();
    let paths = &home.paths;
    let profile = profile_dir(paths, "old");
    // A profile from before full sync: folders and a file of its own.
    write(&profile.join("agents").join("mine.md"), "mine");
    write(&profile.join("rules").join("style.md"), "profile's");
    write(&profile.join("CLAUDE.md"), "remember this");
    write(&paths.claude_dir.join("rules").join("style.md"), "shared");
    write_json(&paths.claude_dir.join("settings.json"), &json!({}));
    let report = link(paths, "old");
    assert_eq!(said(&report, "agents"), ("adopted", None));
    assert!(is_link(&profile.join("agents")));
    assert_eq!(
        fs::read_to_string(paths.claude_dir.join("agents").join("mine.md")).unwrap(),
        "mine"
    );
    // Both have a style.md: neither is written over.
    assert_eq!(
        said(&report, "rules"),
        ("conflict", Some("a folder of its own exists in the profile"))
    );
    assert_eq!(
        fs::read_to_string(profile.join("rules").join("style.md")).unwrap(),
        "profile's"
    );
    assert_eq!(
        fs::read_to_string(paths.claude_dir.join("rules").join("style.md")).unwrap(),
        "shared"
    );
    // The file only this profile had is ~/.claude's now, and shared like the others.
    assert_eq!(
        fs::read_to_string(paths.claude_dir.join("CLAUDE.md")).unwrap(),
        "remember this"
    );
    assert!(
        ["linked", "copied", "ok"].contains(&said(&report, "CLAUDE.md").0),
        "{report:?}"
    );
    // The prompt history is appended by every running claude: it is never taken from a profile.
    write(&profile.join("history.jsonl"), "{}\n");
    link(paths, "old");
    assert!(!paths.claude_dir.join("history.jsonl").exists());
}

#[test]
fn a_profile_whose_settings_cannot_be_read_is_left_exactly_as_it_is() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(&paths.claude_json, &json!({ "theme": "dark" }));
    add(paths, "one");
    write(&global_config_file(paths, "one"), "{ \"oauthAccount\": ");
    let report = link(paths, "one");
    assert_eq!(
        said(&report, ".claude.json"),
        (
            "skipped",
            Some("the profile's own is not valid JSON; left as it is")
        )
    );
    assert_eq!(
        fs::read_to_string(global_config_file(paths, "one")).unwrap(),
        "{ \"oauthAccount\": "
    );
}

#[test]
fn with_full_sync_off_everything_is_as_it_was() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(&paths.claude_json, &json!({ "theme": "dark" }));
    let mut off = Object::new();
    off.insert("fullSync".to_owned(), json!("off"));
    set_policy(paths, &off).unwrap();
    add(paths, "one");
    add(paths, "two");
    claude_writes(paths, "one", |own| own["theme"] = json!("light"));
    write(&profile_dir(paths, "one").join("agents").join("mine.md"), "mine");
    let report = link(paths, "two");
    // No shared folder is made, the settings come from ~/.claude.json alone, and nothing is recorded.
    assert_eq!(said(&report, "agents"), ("no-source", None));
    assert_eq!(said(&report, ".claude.json"), ("ok", Some("seeded")));
    assert_eq!(settings(paths, "two")["theme"], "dark");
    assert!(!paths.claude_dir.join("agents").exists());
    assert!(!sync_file(paths).exists());
    // The next start puts the home's theme back over the account's own, as it always did.
    link(paths, "one");
    assert_eq!(settings(paths, "one")["theme"], "dark");
}

#[test]
fn a_removed_account_no_longer_takes_part() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(&paths.claude_json, &json!({ "theme": "dark" }));
    add(paths, "one");
    add(paths, "two");
    let seen = |paths: &Paths| read_json(&sync_file(paths)).unwrap()["seen"].clone();
    assert!(seen(paths).get("two").is_some());
    remove_account(paths, "two", baton_core::config::By::Name).unwrap();
    assert!(seen(paths).get("two").is_none());
    // Its folder stays, with whatever is in it; it is just not written any more.
    claude_writes(paths, "one", |own| own["theme"] = json!("light"));
    link(paths, "one");
    assert_eq!(settings(paths, "two")["theme"], "dark");
}
