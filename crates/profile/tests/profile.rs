//! A profile shares the folders and files of `~/.claude` and nothing of its sign-in.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once, as the tests do"
)]

mod common;

use std::fs;
use std::path::Path;

use baton_core::fsutil::read_json;
use baton_core::paths::{Paths, profile_dir, resolve};
use baton_profile::profile::{
    Inspected, LinkEntry, LinkOptions, can_symlink_files, carry_trust, inspect_profile, link_dir,
    link_profile, reset_symlink_cache, seed_global_config, unlink_profile,
};
use common::{temp_home, write, write_json};
use serde_json::{Value, json};

/// Linking as it is with full sync off: what these tests are about is the same either way, and the
/// settings' own tests are in `tests/sync.rs`.
fn without_sync(file_links: bool) -> LinkOptions {
    LinkOptions {
        full_sync: Some(false),
        ..LinkOptions::file_links(file_links)
    }
}

/// Text that stands for a credential or an identity: it may never reach a profile.
const SENTINEL: &str = "credential-sentinel-7c1f9a3e";

/// A `~/.claude` and `~/.claude.json` such as a signed-in Claude Code leaves.
fn shared_base(paths: &Paths) {
    write(
        &paths.claude_dir.join("projects").join("E--work").join("s1.jsonl"),
        "{}\n",
    );
    for dir in ["skills/mine", "plugins", "file-history", "sessions"] {
        fs::create_dir_all(paths.claude_dir.join(dir)).unwrap();
    }
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "dark" }),
    );
    write(&paths.claude_dir.join("CLAUDE.md"), "# rules\n");
    write(&paths.claude_dir.join("history.jsonl"), "{}\n");
    write_json(
        &paths.claude_dir.join(".credentials.json"),
        &json!({ "token": SENTINEL }),
    );
    write_json(
        &paths.claude_json,
        &json!({
            "hasCompletedOnboarding": true,
            "lastOnboardingVersion": "2.1.302",
            "oauthAccount": { "emailAddress": "operator@example.invalid", "accountUuid": SENTINEL },
            "userID": SENTINEL,
            "machineID": SENTINEL,
            "primaryApiKey": SENTINEL,
            "customApiKeyResponses": { "approved": [SENTINEL] },
            "cachedUsageUtilization": { "x": 1 },
            "mcpServers": { "tools": { "command": "node", "args": ["server.js"] } },
            "projects": {
                "E:/work": {
                    "hasTrustDialogAccepted": true,
                    "allowedTools": ["Bash"],
                    "lastCost": 9,
                    "lastSessionId": SENTINEL,
                    "mcpServers": { "local": { "command": "x" } }
                },
                "E:/untrusted": { "lastCost": 1 }
            }
        }),
    );
}

/// Linking without file links.
fn copy_mode() -> LinkOptions {
    without_sync(false)
}

/// Whether `path` is a link to `target`.
fn is_link_to(path: &Path, target: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
        && fs::read_link(path).is_ok_and(|to| {
            resolve(to).to_string_lossy().to_lowercase() == resolve(target).to_string_lossy().to_lowercase()
        })
}

/// Fails when a file under `dir` holds the sentinel. Links point into `~/.claude` and are checked by
/// name instead.
fn no_sentinel(dir: &Path) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let kind = entry.file_type().unwrap();
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            no_sentinel(&path);
        } else {
            let text = String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned();
            assert!(
                !text.contains(SENTINEL),
                "{} holds credential or identity data",
                path.display()
            );
        }
    }
}

/// The action reported for an item.
fn action(report: &[LinkEntry], name: &str) -> &'static str {
    report
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no entry for {name}"))
        .action
}

/// The JSON of a profile's `.claude.json`.
fn global_config(paths: &Paths, id: &str) -> Value {
    read_json(&profile_dir(paths, id).join(".claude.json")).unwrap()
}

#[test]
fn a_profile_links_the_shared_bases_folders_and_copies_its_files_when_file_links_are_not_allowed() {
    let home = temp_home();
    let paths = &home.paths;
    shared_base(paths);
    let report = link_profile(paths, "acct2", &copy_mode()).unwrap();
    let profile = profile_dir(paths, "acct2");
    for dir in ["projects", "skills", "plugins", "file-history"] {
        assert_eq!(action(&report, dir), "linked");
        assert!(
            is_link_to(&profile.join(dir), &paths.claude_dir.join(dir)),
            "{dir}"
        );
    }
    assert_eq!(action(&report, "settings.json"), "copied");
    assert_eq!(action(&report, "CLAUDE.md"), "copied");
    assert_eq!(action(&report, "history.jsonl"), "skipped");
    assert_eq!(action(&report, "agents"), "no-source");
    assert!(
        !fs::symlink_metadata(profile.join("settings.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // The shared transcripts are the same files.
    write(&profile.join("projects").join("E--work").join("s2.jsonl"), "{}\n");
    assert!(
        paths
            .claude_dir
            .join("projects")
            .join("E--work")
            .join("s2.jsonl")
            .exists()
    );
    // Never the credentials, never per-process folders.
    assert!(!profile.join(".credentials.json").exists());
    assert!(!profile.join("sessions").exists());
    no_sentinel(&profile);
    no_sentinel(&paths.root);
}

#[test]
fn linking_again_changes_nothing_and_a_changed_source_file_is_refreshed_at_the_next_launch() {
    let home = temp_home();
    let paths = &home.paths;
    shared_base(paths);
    link_profile(paths, "acct2", &copy_mode()).unwrap();
    let again = link_profile(paths, "acct2", &copy_mode()).unwrap();
    assert!(
        again
            .iter()
            .all(|r| ["ok", "no-source", "skipped"].contains(&r.action)),
        "{again:?}"
    );
    write_json(
        &paths.claude_dir.join("settings.json"),
        &json!({ "theme": "light" }),
    );
    let refreshed = link_profile(paths, "acct2", &copy_mode()).unwrap();
    assert_eq!(action(&refreshed, "settings.json"), "refreshed");
    assert_eq!(
        read_json(&profile_dir(paths, "acct2").join("settings.json")),
        Some(json!({ "theme": "light" }))
    );
    // The prompt history is left out on purpose where file links are not allowed: that is "not
    // shared", never "missing".
    let rest: Vec<Inspected> = inspect_profile(paths, "acct2", Some(false))
        .into_iter()
        .filter(|i| !["linked", "copy-fresh", "no-source"].contains(&i.status))
        .collect();
    assert_eq!(
        rest,
        vec![Inspected {
            name: "history.jsonl".into(),
            status: "not-shared",
            detail: None
        }]
    );
}

#[test]
fn the_prompt_history_is_left_out_on_purpose_without_file_links_and_missing_with_them() {
    let home = temp_home();
    let paths = &home.paths;
    shared_base(paths);
    link_profile(paths, "acct2", &copy_mode()).unwrap();
    let status = |file_links: bool| {
        inspect_profile(paths, "acct2", Some(file_links))
            .into_iter()
            .find(|i| i.name == "history.jsonl")
            .unwrap()
            .status
    };
    assert_eq!(status(false), "not-shared");
    // With file links the link should be there, and a baton session makes it.
    assert_eq!(status(true), "missing");
    // A profile whose Claude Code made a history of its own keeps it.
    write(&profile_dir(paths, "acct2").join("history.jsonl"), "{}\n");
    assert_eq!(status(false), "own");
    assert_eq!(status(true), "own");
}

#[test]
fn file_symbolic_links_are_used_where_the_os_allows_them() {
    let home = temp_home();
    let paths = &home.paths;
    if !can_symlink_files(&paths.tmp) {
        eprintln!("skipped: file symbolic links are not allowed here (Windows without Developer Mode)");
        return;
    }
    shared_base(paths);
    let report = link_profile(paths, "acct2", &without_sync(true)).unwrap();
    assert_eq!(action(&report, "settings.json"), "linked");
    assert_eq!(action(&report, "history.jsonl"), "linked");
    let profile = profile_dir(paths, "acct2");
    assert!(is_link_to(
        &profile.join("settings.json"),
        &paths.claude_dir.join("settings.json")
    ));
    // A file of its own with the same bytes becomes the link; one that differs is a conflict.
    let other = profile_dir(paths, "acct3");
    write(&other.join("CLAUDE.md"), "# rules\n");
    write(&other.join("settings.json"), "{ \"mine\": true }");
    let report = link_profile(paths, "acct3", &without_sync(true)).unwrap();
    assert_eq!(action(&report, "CLAUDE.md"), "linked");
    assert_eq!(action(&report, "settings.json"), "conflict");
    assert_eq!(
        fs::read_to_string(other.join("settings.json")).unwrap(),
        "{ \"mine\": true }"
    );
    let inspected = inspect_profile(paths, "acct2", Some(true));
    assert!(
        inspected
            .iter()
            .all(|i| ["linked", "no-source"].contains(&i.status)),
        "{inspected:?}"
    );
    assert_eq!(unlink_profile(paths, "acct2").unwrap().len(), 7);
}

#[test]
fn the_symbolic_link_probe_answers_and_leaves_nothing_behind() {
    let home = temp_home();
    let paths = &home.paths;
    reset_symlink_cache();
    let allowed = can_symlink_files(&paths.tmp);
    assert_eq!(
        can_symlink_files(&paths.tmp),
        allowed,
        "asked once, answered the same"
    );
    if !cfg!(windows) {
        assert!(allowed);
    }
    // Where file links are always allowed (not Windows) the probe never makes its folder at all.
    let left: Vec<_> = fs::read_dir(&paths.tmp)
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_profiles_own_folder_is_a_conflict_and_is_left_alone_and_unsafe_links_are_removed() {
    let home = temp_home();
    let paths = &home.paths;
    shared_base(paths);
    let profile = profile_dir(paths, "acct2");
    write(&profile.join("skills").join("own").join("SKILL.md"), "mine");
    link_dir(&paths.claude_dir.join("sessions"), &profile.join("sessions")).unwrap();
    assert_eq!(
        inspect_profile(paths, "acct2", Some(false))
            .iter()
            .find(|i| i.name == "sessions")
            .unwrap()
            .status,
        "unsafe-link"
    );
    let report = link_profile(paths, "acct2", &copy_mode()).unwrap();
    let skills = report.iter().find(|r| r.name == "skills").unwrap();
    assert_eq!(skills.action, "conflict");
    assert_eq!(
        skills.detail.as_deref(),
        Some("a folder of its own exists in the profile")
    );
    assert!(profile.join("skills").join("own").join("SKILL.md").exists());
    assert_eq!(action(&report, "sessions"), "removed-unsafe-link");
    assert!(fs::symlink_metadata(profile.join("sessions")).is_err());
    assert!(
        paths.claude_dir.join("sessions").exists(),
        "the target is untouched"
    );
    assert_eq!(
        inspect_profile(paths, "acct2", Some(false))
            .iter()
            .find(|i| i.name == "skills")
            .unwrap()
            .status,
        "conflict"
    );
}

#[test]
fn a_link_whose_source_disappeared_is_removed_and_unlinking_removes_only_batons_links() {
    let home = temp_home();
    let paths = &home.paths;
    shared_base(paths);
    fs::create_dir(paths.claude_dir.join("agents")).unwrap();
    link_profile(paths, "acct2", &copy_mode()).unwrap();
    let profile = profile_dir(paths, "acct2");
    assert!(
        fs::symlink_metadata(profile.join("agents"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    // The source goes: the link that pointed to it goes at the next launch.
    fs::remove_dir(paths.claude_dir.join("agents")).unwrap();
    let report = link_profile(paths, "acct2", &copy_mode()).unwrap();
    assert_eq!(action(&report, "agents"), "no-source");
    assert!(fs::symlink_metadata(profile.join("agents")).is_err());
    fs::create_dir(paths.claude_dir.join("agents")).unwrap();
    assert_eq!(
        action(&link_profile(paths, "acct2", &copy_mode()).unwrap(), "agents"),
        "linked"
    );
    let removed = unlink_profile(paths, "acct2").unwrap();
    assert!(removed.iter().any(|name| name == "projects") && removed.iter().any(|name| name == "agents"));
    assert!(fs::symlink_metadata(profile.join("projects")).is_err());
    assert!(
        paths
            .claude_dir
            .join("projects")
            .join("E--work")
            .join("s1.jsonl")
            .is_file(),
        "~/.claude keeps its data"
    );
    assert!(profile.join("settings.json").exists(), "copies stay");
}

#[test]
fn a_link_that_points_elsewhere_is_a_conflict_while_its_target_is_there_and_relinked_once_it_is_gone() {
    let home = temp_home();
    let paths = &home.paths;
    shared_base(paths);
    let profile = profile_dir(paths, "acct2");
    let elsewhere = home.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(&profile).unwrap();
    link_dir(&elsewhere, &profile.join("skills")).unwrap();
    let report = link_profile(paths, "acct2", &copy_mode()).unwrap();
    let skills = report.iter().find(|r| r.name == "skills").unwrap();
    assert_eq!(skills.action, "conflict");
    assert!(
        skills.detail.as_deref().unwrap().starts_with("links to "),
        "{skills:?}"
    );
    let inspected = inspect_profile(paths, "acct2", Some(false));
    assert_eq!(
        inspected.iter().find(|i| i.name == "skills").unwrap().status,
        "wrong-link"
    );
    assert!(
        unlink_profile(paths, "acct2")
            .unwrap()
            .iter()
            .all(|name| name != "skills"),
        "not Baton's link"
    );
    link_dir(&elsewhere, &profile.join("skills")).unwrap_err();
    fs::remove_dir(&elsewhere).unwrap();
    let report = link_profile(paths, "acct2", &copy_mode()).unwrap();
    assert_eq!(action(&report, "skills"), "relinked");
    assert!(is_link_to(
        &profile.join("skills"),
        &paths.claude_dir.join("skills")
    ));
}

#[test]
fn the_seed_copies_only_the_allowlist_and_keeps_the_profiles_own_identity() {
    let source = json!({
        "hasCompletedOnboarding": true,
        "theme": "dark",
        "oauthAccount": { "emailAddress": "one@example.invalid" },
        "userID": "u1",
        "primaryApiKey": "k",
        "cachedUsageUtilization": {},
        "mcpServers": { "m": { "command": "x" } },
        "projects": { "E:/w": { "hasTrustDialogAccepted": true, "lastCost": 3, "lastSessionId": "s" } }
    });
    let target = json!({
        "oauthAccount": { "emailAddress": "two@example.invalid" },
        "userID": "u2",
        "numStartups": 4,
        "projects": { "E:/w": { "lastCost": 1 } }
    });
    let out = seed_global_config(Some(&source), Some(&target));
    assert_eq!(
        out["oauthAccount"],
        json!({ "emailAddress": "two@example.invalid" })
    );
    assert_eq!(out["userID"], "u2");
    assert_eq!(out.get("primaryApiKey"), None);
    assert_eq!(out.get("cachedUsageUtilization"), None);
    assert_eq!(out["hasCompletedOnboarding"], true);
    assert_eq!(out["mcpServers"], json!({ "m": { "command": "x" } }));
    assert_eq!(
        out["projects"]["E:/w"],
        json!({ "lastCost": 1, "hasTrustDialogAccepted": true })
    );
    assert_eq!(out["numStartups"], 4);
    // The profile's own keys first, in their order; what is seeded after, in the allowlist's.
    assert_eq!(
        out.as_object().unwrap().keys().collect::<Vec<_>>(),
        [
            "oauthAccount",
            "userID",
            "numStartups",
            "projects",
            "hasCompletedOnboarding",
            "theme",
            "mcpServers"
        ]
    );
    let fresh = seed_global_config(Some(&source), None);
    let mut keys: Vec<&String> = fresh.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["hasCompletedOnboarding", "mcpServers", "projects", "theme"]
    );
    // Nothing to seed from: the profile's own, as it is.
    assert_eq!(seed_global_config(None, Some(&target)), target);
    assert_eq!(seed_global_config(Some(&json!("text")), None), json!({}));
}

#[test]
fn the_seed_is_written_to_the_profiles_global_config_without_identity_fields() {
    let home = temp_home();
    let paths = &home.paths;
    shared_base(paths);
    let report = link_profile(paths, "acct2", &copy_mode()).unwrap();
    assert_eq!(action(&report, ".claude.json"), "seeded");
    let seeded = global_config(paths, "acct2");
    assert_eq!(seeded["hasCompletedOnboarding"], true);
    assert_eq!(seeded.get("oauthAccount"), None);
    assert_eq!(
        seeded["projects"]["E:/work"],
        json!({ "hasTrustDialogAccepted": true, "allowedTools": ["Bash"], "mcpServers": { "local": { "command": "x" } } })
    );
    assert_eq!(seeded["projects"].get("E:/untrusted"), None);
    let again = link_profile(paths, "acct2", &copy_mode()).unwrap();
    let entry = again.iter().find(|r| r.name == ".claude.json").unwrap();
    assert_eq!((entry.action, entry.detail.as_deref()), ("ok", Some("seeded")));
    // A change in ~/.claude.json reaches the profile at the next launch.
    let mut source: Value = read_json(&paths.claude_json).unwrap();
    source["theme"] = json!("light");
    write_json(&paths.claude_json, &source);
    assert_eq!(
        action(
            &link_profile(paths, "acct2", &copy_mode()).unwrap(),
            ".claude.json"
        ),
        "refreshed"
    );
    assert_eq!(global_config(paths, "acct2")["theme"], "light");
    // Without a ~/.claude.json there is nothing to seed.
    fs::remove_file(&paths.claude_json).unwrap();
    assert_eq!(
        action(
            &link_profile(paths, "acct2", &copy_mode()).unwrap(),
            ".claude.json"
        ),
        "no-source"
    );
}

#[test]
fn a_profiles_global_config_that_cannot_be_read_is_never_written_over() {
    let home = temp_home();
    let paths = &home.paths;
    shared_base(paths);
    let own = profile_dir(paths, "acct2").join(".claude.json");
    write(
        &own,
        "{ \"oauthAccount\": { \"emailAddress\": \"two@example.invalid\" }, half",
    );
    let report = link_profile(paths, "acct2", &copy_mode()).unwrap();
    let entry = report.iter().find(|r| r.name == ".claude.json").unwrap();
    assert_eq!(entry.action, "skipped");
    assert!(
        fs::read_to_string(&own).unwrap().ends_with("half"),
        "left as it was"
    );
    write_json(
        &profile_dir(paths, "acct3").join(".claude.json"),
        &json!({ "projects": { "E:/work": { "hasTrustDialogAccepted": true } } }),
    );
    assert!(!carry_trust(paths, "acct3", "acct2", "E:/work").unwrap());
    assert!(fs::read_to_string(&own).unwrap().ends_with("half"));
    // A text Claude Code cut in the middle of an emoji is still read, and the identity kept.
    write(
        &own,
        r#"{"oauthAccount":{"emailAddress":"two@example.invalid"},"tip":"cut \ud83d"}"#,
    );
    assert_eq!(
        action(
            &link_profile(paths, "acct2", &copy_mode()).unwrap(),
            ".claude.json"
        ),
        "refreshed"
    );
    let seeded = global_config(paths, "acct2");
    assert_eq!(seeded["oauthAccount"]["emailAddress"], "two@example.invalid");
    assert_eq!(seeded["hasCompletedOnboarding"], true);
    // An empty file is no config at all: seeded like a missing one.
    write(&own, "");
    assert_eq!(
        action(
            &link_profile(paths, "acct2", &copy_mode()).unwrap(),
            ".claude.json"
        ),
        "seeded"
    );
}

#[test]
fn folder_trust_moves_with_a_session_to_the_next_profile_never_into_the_shared_global_config() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(
        &profile_dir(paths, "a2").join(".claude.json"),
        &json!({
            "oauthAccount": { "emailAddress": "a2" },
            "projects": { "E:\\new": { "hasTrustDialogAccepted": true, "lastCost": 2 } }
        }),
    );
    write_json(
        &profile_dir(paths, "a3").join(".claude.json"),
        &json!({ "oauthAccount": { "emailAddress": "a3" } }),
    );
    write_json(&paths.claude_json, &json!({ "projects": {} }));
    assert!(carry_trust(paths, "a2", "a3", "E:/new").unwrap());
    let a3 = global_config(paths, "a3");
    assert_eq!(
        a3["projects"]["E:\\new"],
        json!({ "hasTrustDialogAccepted": true })
    );
    assert_eq!(a3["oauthAccount"]["emailAddress"], "a3");
    assert!(!carry_trust(paths, "a2", "a3", "E:/new").unwrap());
    // An account named "default" is a profile like any other.
    assert!(carry_trust(paths, "a2", "default", "E:/new").unwrap());
    let default = read_json(&paths.accounts.join("default").join(".claude.json")).unwrap();
    assert_eq!(default["projects"]["E:\\new"]["hasTrustDialogAccepted"], true);
    assert_eq!(read_json(&paths.claude_json), Some(json!({ "projects": {} })));
    // Nothing to carry: no folder, the same profile, a folder that is not trusted there.
    assert!(!carry_trust(paths, "a2", "a3", "").unwrap());
    assert!(!carry_trust(paths, "a2", "a2", "E:/new").unwrap());
    assert!(!carry_trust(paths, "a3", "a2", "E:/other").unwrap());
    assert!(!carry_trust(paths, "nobody", "a3", "E:/new").unwrap());
}

#[test]
fn the_seed_never_takes_back_what_a_profile_has_its_own_trust_its_approvals_its_counts() {
    // Claude Code saves every folder with its defaults: trust false, empty lists.
    let source = json!({
        "projects": {
            "E:/w": {
                "hasTrustDialogAccepted": false,
                "projectOnboardingSeenCount": 0,
                "allowedTools": ["Bash"],
                "enabledMcpjsonServers": [],
                "mcpServers": { "shared": { "command": "a" } }
            },
            "E:/granted": { "hasTrustDialogAccepted": true }
        }
    });
    let target = json!({
        "projects": {
            "E:/w": {
                "hasTrustDialogAccepted": true,
                "projectOnboardingSeenCount": 3,
                "allowedTools": ["Read", "Bash"],
                "enabledMcpjsonServers": ["local"],
                "mcpServers": { "own": { "command": "b" } }
            },
            "E:/granted": { "hasTrustDialogAccepted": false, "lastCost": 1 }
        }
    });
    let out = seed_global_config(Some(&source), Some(&target));
    assert_eq!(
        out["projects"]["E:/w"],
        json!({
            "hasTrustDialogAccepted": true,
            "projectOnboardingSeenCount": 3,
            "allowedTools": ["Bash", "Read"],
            "enabledMcpjsonServers": ["local"],
            "mcpServers": { "own": { "command": "b" }, "shared": { "command": "a" } }
        })
    );
    // What ~/.claude.json grants still reaches the profile.
    assert_eq!(
        out["projects"]["E:/granted"],
        json!({ "hasTrustDialogAccepted": true, "lastCost": 1 })
    );
    // A second refresh changes nothing.
    assert_eq!(seed_global_config(Some(&source), Some(&out)), out);
}

#[test]
fn trust_carried_by_a_hop_survives_the_next_profiles_refresh_at_launch() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(
        &paths.claude_json,
        &json!({
            "hasCompletedOnboarding": true,
            "projects": { "E:/new": { "hasTrustDialogAccepted": false, "allowedTools": [] } }
        }),
    );
    write_json(
        &profile_dir(paths, "a2").join(".claude.json"),
        &json!({ "projects": { "E:/new": { "hasTrustDialogAccepted": true, "allowedTools": [] } } }),
    );
    write_json(
        &profile_dir(paths, "a3").join(".claude.json"),
        &json!({ "oauthAccount": { "emailAddress": "a3" } }),
    );
    let trusted = |id: &str| global_config(paths, id)["projects"]["E:/new"]["hasTrustDialogAccepted"] == true;
    // The order the session wrapper launches in: carry, then refresh the profile.
    assert!(carry_trust(paths, "a2", "a3", "E:/new").unwrap());
    link_profile(paths, "a3", &copy_mode()).unwrap();
    assert!(trusted("a3"));
    // Every later launch on either profile keeps it.
    link_profile(paths, "a2", &copy_mode()).unwrap();
    link_profile(paths, "a3", &copy_mode()).unwrap();
    assert!(trusted("a2"));
    assert!(trusted("a3"));
}
