//! Profiles in copy mode (no file links), as the command line shows them: `baton doctor` names a copy
//! conflict, `baton conflicts` lists and clears them, and a removed account is forgotten.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once"
)]

mod common;

use std::path::{Path, PathBuf};

use baton::doctor::{DoctorOptions, Report, run_doctor};
use baton::screens::Check;
use baton_core::clock::now_ms;
use baton_core::fsutil::read_json;
use baton_core::paths::{Paths, profile_dir};
use baton_install::ferry::FerryAccess;
use baton_profile::copies::{BASE, CopyEntry, copies_file, read_conflicts, sync_copies};
use baton_profile::profile::{LinkOptions, link_profile};
use baton_testkit::{TestHome, temp_home, test_env, write, write_json};
use common::baton;
use serde_json::{Value, json};

/// A home with two profiles, acct2 and acct3, linked without file links: settings.json is a copy in
/// each.
fn setup() -> TestHome {
    let home = temp_home();
    let paths = &home.paths;
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "dark" }),
    );
    write(&paths.claude_dir.join("CLAUDE.md"), "# one\n");
    for name in ["acct2", "acct3"] {
        link_profile(paths, name, &LinkOptions::file_links(false)).unwrap();
    }
    home
}

/// The settings of `~/.claude` (named [`BASE`]) or of a profile.
fn settings_of(paths: &Paths, name: &str) -> Value {
    let dir = if name == BASE {
        paths.claude_dir.clone()
    } else {
        profile_dir(paths, name)
    };
    read_json(&dir.join("settings.json")).unwrap()
}

/// A profile's settings.json, replaced.
fn write_settings(paths: &Paths, name: &str, value: Value) {
    write_json(&profile_dir(paths, name).join("settings.json"), &value);
}

/// Both profiles brought in line now.
fn sync(paths: &Paths) -> Vec<CopyEntry> {
    sync_copies(paths, &["acct2", "acct3"], now_ms()).unwrap()
}

/// What a sync did with an account's settings.json.
fn action(report: &[CopyEntry], account: &str) -> &'static str {
    report
        .iter()
        .find(|r| r.account == account && r.name == "settings.json")
        .unwrap()
        .action
}

/// What doctor finds in a test home, without asking Claude Code who is signed in and without Ferry.
fn doctor(home: &TestHome) -> Report {
    let no_ferry = || -> Option<PathBuf> { None };
    run_doctor(&DoctorOptions {
        paths: &home.paths,
        env: &test_env(&home.home),
        exe: Path::new("baton"),
        auth: false,
        ferry: FerryAccess {
            find: Some(&no_ferry),
            run: None,
        },
        now: now_ms(),
    })
}

/// One of doctor's checks, by name.
fn check<'a>(report: &'a Report, name: &str) -> &'a Check {
    report.checks.iter().find(|check| check.name == name).unwrap()
}

#[test]
fn both_changed_the_shared_bases_version_is_kept_and_doctor_names_the_conflict() {
    let home = setup();
    let paths = &home.paths;
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "one" }),
    );
    write_settings(paths, "acct2", json!({ "theme": "two" }));
    let report = sync(paths);
    assert_eq!(action(&report, "acct2"), "conflict");
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
    let found = doctor(&home);
    let copies = check(&found, "copy conflicts");
    assert_eq!(copies.ok, Some(false));
    assert!(
        copies.detail.contains("settings.json changed in acct2"),
        "{}",
        copies.detail
    );
}

#[test]
fn two_profiles_changed_the_same_file_and_baton_conflicts_shows_it_and_clears_it() {
    let home = setup();
    let paths = &home.paths;
    write_settings(paths, "acct2", json!({ "theme": "two" }));
    write_settings(paths, "acct3", json!({ "theme": "three" }));
    let report = sync(paths);
    assert_eq!(action(&report, "acct2"), "copied-back");
    assert_eq!(action(&report, "acct3"), "conflict");
    assert_eq!(settings_of(paths, BASE), json!({ "theme": "two" }));
    assert_eq!(settings_of(paths, "acct3"), json!({ "theme": "two" }));
    assert_eq!(read_conflicts(paths)[0].kept, "acct2");
    let shown = baton(&home, &["conflicts"]);
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    assert!(
        shown.stdout.contains("settings.json in acct3"),
        "{}",
        shown.stdout
    );
    let kept = shown.stdout.lines().any(|line| {
        line.split_once("Kept").is_some_and(|(_, rest)| {
            rest.starts_with(' ') && rest.trim_start().starts_with("acct2's version")
        })
    });
    assert!(kept, "{}", shown.stdout);
    baton(&home, &["conflicts", "--clear"]);
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
    let report = link_profile(paths, "acct9", &LinkOptions::file_links(false)).unwrap();
    assert_eq!(
        report.iter().find(|r| r.name == "settings.json").unwrap().action,
        "refreshed"
    );
    assert_eq!(settings_of(paths, "acct9"), json!({ "theme": "dark" }));
    assert!(paths.backups.exists());
    baton(&home, &["account", "add", "acct3"]);
    baton(&home, &["account", "add", "acct4"]);
    assert_eq!(baton(&home, &["account", "remove", "acct4"]).code, 0);
    let record = read_json(&copies_file(paths)).unwrap();
    assert_eq!(record["profiles"].get("acct4"), None);
}

#[test]
#[cfg_attr(not(windows), ignore = "Windows only")]
fn doctor_explains_that_developer_mode_turns_the_copies_into_real_links() {
    let home = temp_home();
    let found = doctor(&home);
    let links = check(&found, "file links");
    if links.detail.starts_with("allowed") {
        return;
    }
    let explained = links
        .detail
        .split_once("Developer Mode ")
        .is_some_and(|(_, rest)| rest.contains(" makes them real links and shares history.jsonl"));
    assert!(explained, "{}", links.detail);
}
