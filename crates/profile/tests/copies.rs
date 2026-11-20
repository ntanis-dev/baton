//! Profiles in copy mode (no file links): settings.json is a copy in each, kept in line both ways.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once, as the tests do"
)]

mod common;

use std::fs;
use std::path::Path;

use baton_core::clock::now_ms;
use baton_core::config::By;
use baton_core::fsutil::read_json;
use baton_core::paths::{Paths, profile_dir};
use baton_profile::accounts::{AddOptions, add_account, remove_account};
use baton_profile::copies::{BASE, CopyEntry, clear_conflicts, copies_file, read_conflicts, sync_copies};
use baton_profile::profile::{LinkOptions, inspect_profile, link_profile};
use common::{TestHome, temp_home, write, write_json};
use serde_json::{Value, json};

/// Linking as it is with full sync off: what these tests are about is the same either way, and the
/// settings' own tests are in `tests/sync.rs`.
fn without_sync(file_links: bool) -> LinkOptions {
    LinkOptions {
        full_sync: Some(false),
        ..LinkOptions::file_links(file_links)
    }
}

/// A home with two profiles, acct2 and acct3, linked without file links.
fn setup() -> TestHome {
    let home = temp_home();
    let paths = &home.paths;
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "dark" }),
    );
    write(&paths.claude_dir.join("CLAUDE.md"), "# one\n");
    for name in ["acct2", "acct3"] {
        link_profile(paths, name, &without_sync(false)).unwrap();
    }
    home
}

/// The settings of `~/.claude` (named [`BASE`]) or of a profile.
fn settings_of(paths: &Paths, name: &str) -> Value {
    let file = if name == BASE {
        paths.claude_dir.join("settings.json")
    } else {
        profile_dir(paths, name).join("settings.json")
    };
    read_json(&file).unwrap()
}

/// A profile's settings.json, replaced.
fn write_settings(paths: &Paths, name: &str, value: Value) {
    write_json(&profile_dir(paths, name).join("settings.json"), &value);
}

/// The report's entry for settings.json of an account.
fn entry<'a>(report: &'a [CopyEntry], account: &str) -> &'a CopyEntry {
    report
        .iter()
        .find(|r| r.account == account && r.name == "settings.json")
        .unwrap()
}

/// Both profiles brought in line now.
fn sync(paths: &Paths) -> Vec<CopyEntry> {
    sync_copies(paths, &["acct2", "acct3"], now_ms()).unwrap()
}

#[test]
fn a_change_made_in_a_profile_is_copied_back_with_a_backup_and_on_to_the_other_profiles() {
    let home = setup();
    let paths = &home.paths;
    // /config under acct2 rewrites that profile's settings.json.
    write_settings(paths, "acct2", json!({ "theme": "light", "model": "opus" }));
    let status = inspect_profile(paths, "acct2", None);
    assert_eq!(
        status.iter().find(|i| i.name == "settings.json").unwrap().status,
        "copy-changed"
    );
    let report = sync(paths);
    let back = entry(&report, "acct2");
    assert_eq!(back.action, "copied-back");
    assert_eq!(
        settings_of(paths, BASE),
        json!({ "theme": "light", "model": "opus" })
    );
    assert_eq!(
        settings_of(paths, "acct3"),
        json!({ "theme": "light", "model": "opus" })
    );
    assert_eq!(entry(&report, "acct3").action, "refreshed");
    let detail = back.detail.as_deref().unwrap();
    let saved = detail.split_once("is in ").unwrap().1;
    assert!(
        detail.starts_with("~/.claude's previous settings.json is in "),
        "{detail}"
    );
    assert_eq!(
        read_json(Path::new(saved)),
        Some(json!({ "theme": "dark" })),
        "~/.claude's previous version is kept"
    );
    assert!(read_conflicts(paths).is_empty());
    // Stable afterwards.
    assert!(
        sync(paths)
            .iter()
            .all(|r| ["ok", "no-source"].contains(&r.action))
    );
}

#[test]
fn a_change_in_the_shared_base_alone_refreshes_the_profiles() {
    let home = setup();
    let paths = &home.paths;
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "light" }),
    );
    let report = link_profile(paths, "acct2", &without_sync(false)).unwrap();
    assert_eq!(
        report.iter().find(|r| r.name == "settings.json").unwrap().action,
        "refreshed"
    );
    assert_eq!(settings_of(paths, "acct2"), json!({ "theme": "light" }));
    assert_eq!(
        settings_of(paths, "acct3"),
        json!({ "theme": "light" }),
        "every profile at once"
    );
}

#[test]
fn both_changed_the_shared_bases_version_is_kept_the_profiles_is_backed_up_and_the_conflict_recorded() {
    let home = setup();
    let paths = &home.paths;
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "one" }),
    );
    write_settings(paths, "acct2", json!({ "theme": "two" }));
    let report = sync(paths);
    let c = entry(&report, "acct2");
    assert_eq!(c.action, "conflict");
    assert!(
        c.detail
            .as_deref()
            .unwrap()
            .starts_with("changed in the profile and in ~/.claude: kept ~/.claude's; this profile's is in "),
        "{c:?}"
    );
    assert_eq!(settings_of(paths, BASE), json!({ "theme": "one" }));
    assert_eq!(settings_of(paths, "acct2"), json!({ "theme": "one" }));
    let conflicts = read_conflicts(paths);
    let conflict = &conflicts[0];
    assert_eq!(
        (
            conflict.account.as_str(),
            conflict.item.as_str(),
            conflict.kept.as_str()
        ),
        ("acct2", "settings.json", "~/.claude")
    );
    assert_eq!(
        read_json(Path::new(conflict.backup.as_deref().unwrap())),
        Some(json!({ "theme": "two" }))
    );
    assert!(conflict.at.is_some());
}

#[test]
fn two_profiles_changed_the_same_file_the_first_in_order_is_copied_back_the_other_is_a_conflict() {
    let home = setup();
    let paths = &home.paths;
    write_settings(paths, "acct2", json!({ "theme": "two" }));
    write_settings(paths, "acct3", json!({ "theme": "three" }));
    let report = sync(paths);
    assert_eq!(entry(&report, "acct2").action, "copied-back");
    let other = entry(&report, "acct3");
    assert_eq!(other.action, "conflict");
    assert!(
        other
            .detail
            .as_deref()
            .unwrap()
            .starts_with("kept acct2's change; this profile's is in ")
    );
    assert_eq!(settings_of(paths, BASE), json!({ "theme": "two" }));
    assert_eq!(settings_of(paths, "acct3"), json!({ "theme": "two" }));
    assert_eq!(read_conflicts(paths)[0].kept, "acct2");
    clear_conflicts(paths).unwrap();
    assert!(read_conflicts(paths).is_empty());
}

#[test]
fn two_profiles_that_made_the_same_change_are_no_conflict() {
    let home = setup();
    let paths = &home.paths;
    write_settings(paths, "acct2", json!({ "theme": "same" }));
    write_settings(paths, "acct3", json!({ "theme": "same" }));
    let report = sync(paths);
    assert_eq!(entry(&report, "acct2").action, "copied-back");
    assert_eq!(entry(&report, "acct3").action, "ok");
    assert_eq!(settings_of(paths, BASE), json!({ "theme": "same" }));
    assert!(read_conflicts(paths).is_empty());
}

#[test]
fn an_old_copy_without_a_record_is_replaced_and_kept_as_a_backup_and_removed_accounts_are_forgotten() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "dark" }),
    );
    write_settings(paths, "acct9", json!({ "theme": "old" }));
    let report = link_profile(paths, "acct9", &without_sync(false)).unwrap();
    assert_eq!(
        report.iter().find(|r| r.name == "settings.json").unwrap().action,
        "refreshed"
    );
    assert_eq!(settings_of(paths, "acct9"), json!({ "theme": "dark" }));
    assert!(paths.backups.exists());
    let stamped = fs::read_dir(&paths.backups)
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    assert_eq!(
        read_json(&stamped.join("replaced-acct9-settings.json")),
        Some(json!({ "theme": "old" }))
    );
    let copy_mode = AddOptions {
        file_links: Some(false),
        ..AddOptions::default()
    };
    add_account(paths, "acct3", &copy_mode).unwrap();
    add_account(paths, "acct4", &copy_mode).unwrap();
    let recorded = || read_json(&copies_file(paths)).unwrap()["profiles"].clone();
    assert!(recorded().get("acct4").is_some());
    remove_account(paths, "acct4", By::Name).unwrap();
    assert_eq!(recorded().get("acct4"), None);
    assert!(recorded().get("acct3").is_some());
    assert!(recorded().get("acct9").is_some());
}

#[test]
fn the_record_is_written_as_it_always_was() {
    let home = setup();
    let paths = &home.paths;
    let record = read_json(&copies_file(paths)).unwrap();
    assert_eq!(
        record.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["version", "profiles", "source"]
    );
    assert_eq!(record["version"], json!(1));
    assert_eq!(
        record["profiles"].as_object().unwrap().keys().collect::<Vec<_>>(),
        ["acct2", "acct3"]
    );
    let own = record["profiles"]["acct2"].as_object().unwrap();
    assert_eq!(own.keys().collect::<Vec<_>>(), ["settings.json", "CLAUDE.md"]);
    assert_eq!(own["settings.json"], record["source"]["settings.json"]);
    assert_eq!(own["CLAUDE.md"].as_str().unwrap().len(), 64);
    // A profile that is gone, or a name that could not be one, is left out of a sync.
    let report = sync_copies(paths, &["acct2", "ghost", "../x"], now_ms()).unwrap();
    assert!(
        report
            .iter()
            .all(|r| ["acct2", "acct3"].contains(&r.account.as_str())),
        "{report:?}"
    );
    // keybindings.json is not in ~/.claude: said once per profile, nothing copied.
    assert_eq!(report.iter().filter(|r| r.action == "no-source").count(), 2);
    assert!(!profile_dir(paths, "acct2").join("keybindings.json").exists());
}
