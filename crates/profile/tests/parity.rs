//! What the JavaScript implementation gave for the same inputs, byte for byte: the seed of a profile's
//! `.claude.json`, the copies record, and the reports of linking and of a two-way sync.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once, as the tests do"
)]

mod common;

use std::fs;

use baton_core::clock::parse_ms;
use baton_core::paths::profile_dir;
use baton_profile::copies::{conflicts_file, copies_file, sync_copies};
use baton_profile::profile::{LinkOptions, inspect_profile, link_profile, seed_global_config};
use common::{temp_home, write};
use serde_json::{Value, json};

/// Linking as it is with full sync off: what these tests are about is the same either way, and the
/// settings' own tests are in `tests/sync.rs`.
fn without_sync(file_links: bool) -> LinkOptions {
    LinkOptions {
        full_sync: Some(false),
        ..LinkOptions::file_links(file_links)
    }
}

/// What `~/.claude.json` holds: seeded keys, identity, and folders of every kind.
fn source() -> Value {
    json!({
        "hasCompletedOnboarding": true,
        "theme": "dark",
        "verbose": false,
        "oauthAccount": { "emailAddress": "one@example.invalid" },
        "userID": "u1",
        "mcpServers": { "m": { "command": "x", "args": ["a"] } },
        "autoUpdates": null,
        "projects": {
            "E:/w": {
                "hasTrustDialogAccepted": false,
                "projectOnboardingSeenCount": 0,
                "allowedTools": ["Bash", "Edit"],
                "enabledMcpjsonServers": [],
                "mcpServers": { "shared": { "command": "a" } },
                "lastCost": 3
            },
            "E:/granted": { "hasTrustDialogAccepted": true, "mcpContextUris": ["u"] },
            "E:/none": { "lastCost": 1 },
            "E:/odd": 5,
            "E:/new": { "hasCompletedProjectOnboarding": true, "disabledMcpjsonServers": ["x"] }
        }
    })
}

/// What a profile's own `.claude.json` holds.
fn target() -> Value {
    json!({
        "numStartups": 4,
        "theme": "light",
        "oauthAccount": { "emailAddress": "two@example.invalid" },
        "projects": {
            "E:/own": { "lastCost": 2 },
            "E:/w": {
                "lastCost": 1,
                "hasTrustDialogAccepted": true,
                "projectOnboardingSeenCount": 3,
                "allowedTools": ["Read", "Bash"],
                "enabledMcpjsonServers": ["local"],
                "mcpServers": { "own": { "command": "b" }, "shared": { "command": "old" } }
            },
            "E:/granted": { "hasTrustDialogAccepted": false, "lastCost": 1, "mcpContextUris": "text" }
        },
        "userID": "u2"
    })
}

#[test]
fn the_seed_is_what_it_always_was_keys_in_the_same_order() {
    let seeded = seed_global_config(Some(&source()), Some(&target()));
    assert_eq!(
        serde_json::to_string(&seeded).unwrap(),
        r#"{"numStartups":4,"theme":"dark","oauthAccount":{"emailAddress":"two@example.invalid"},"projects":{"E:/own":{"lastCost":2},"E:/w":{"lastCost":1,"hasTrustDialogAccepted":true,"projectOnboardingSeenCount":3,"allowedTools":["Bash","Edit","Read"],"enabledMcpjsonServers":["local"],"mcpServers":{"own":{"command":"b"},"shared":{"command":"a"}}},"E:/granted":{"hasTrustDialogAccepted":true,"lastCost":1,"mcpContextUris":["u"]},"E:/new":{"hasCompletedProjectOnboarding":true,"disabledMcpjsonServers":["x"]}},"userID":"u2","hasCompletedOnboarding":true,"mcpServers":{"m":{"command":"x","args":["a"]}},"verbose":false,"autoUpdates":null}"#
    );
    let fresh = seed_global_config(Some(&source()), None);
    assert_eq!(
        serde_json::to_string(&fresh).unwrap(),
        r#"{"hasCompletedOnboarding":true,"theme":"dark","mcpServers":{"m":{"command":"x","args":["a"]}},"verbose":false,"autoUpdates":null,"projects":{"E:/w":{"hasTrustDialogAccepted":false,"projectOnboardingSeenCount":0,"allowedTools":["Bash","Edit"],"mcpServers":{"shared":{"command":"a"}},"enabledMcpjsonServers":[]},"E:/granted":{"hasTrustDialogAccepted":true,"mcpContextUris":["u"]},"E:/new":{"hasCompletedProjectOnboarding":true,"disabledMcpjsonServers":["x"]}}}"#
    );
    assert_eq!(seed_global_config(None, Some(&target())), target());
}

#[test]
fn linking_and_a_two_way_sync_leave_what_they_always_left() {
    let home = temp_home();
    let paths = &home.paths;
    write(
        &paths.claude_dir.join("settings.json"),
        "{\n  \"theme\": \"dark\"\n}",
    );
    write(&paths.claude_dir.join("CLAUDE.md"), "# one\n");
    let copy_mode = without_sync(false);
    link_profile(paths, "acct2", &copy_mode).unwrap();
    let report = link_profile(paths, "acct3", &copy_mode).unwrap();
    assert_eq!(
        fs::read_to_string(copies_file(paths)).unwrap(),
        "{\n  \"version\": 1,\n  \"profiles\": {\n    \"acct2\": {\n      \"settings.json\": \"30af62543c69355ac76e15ada3e02f8e6a1a92546717fd3de8c4ca4c186881ff\",\n      \"CLAUDE.md\": \"54c592ac3a26758bdc4232b5a2f32eed508b2b3c48bc3b177eae44a5b47bd0c8\"\n    },\n    \"acct3\": {\n      \"settings.json\": \"30af62543c69355ac76e15ada3e02f8e6a1a92546717fd3de8c4ca4c186881ff\",\n      \"CLAUDE.md\": \"54c592ac3a26758bdc4232b5a2f32eed508b2b3c48bc3b177eae44a5b47bd0c8\"\n    }\n  },\n  \"source\": {\n    \"settings.json\": \"30af62543c69355ac76e15ada3e02f8e6a1a92546717fd3de8c4ca4c186881ff\",\n    \"CLAUDE.md\": \"54c592ac3a26758bdc4232b5a2f32eed508b2b3c48bc3b177eae44a5b47bd0c8\"\n  }\n}\n"
    );
    assert_eq!(
        serde_json::to_string(&report).unwrap(),
        r#"[{"name":"projects","action":"no-source"},{"name":"skills","action":"no-source"},{"name":"agents","action":"no-source"},{"name":"commands","action":"no-source"},{"name":"plugins","action":"no-source"},{"name":"keybindings.json","action":"no-source"},{"name":"file-history","action":"no-source"},{"name":"plans","action":"no-source"},{"name":"tasks","action":"no-source"},{"name":"todos","action":"no-source"},{"name":"rules","action":"no-source"},{"name":"output-styles","action":"no-source"},{"name":"history.jsonl","action":"no-source"},{"name":"settings.json","action":"copied"},{"name":"CLAUDE.md","action":"copied"},{"name":".claude.json","action":"no-source"}]"#
    );
    assert_eq!(
        serde_json::to_string(&inspect_profile(paths, "acct3", Some(false))).unwrap(),
        r#"[{"name":"projects","status":"no-source"},{"name":"settings.json","status":"copy-fresh"},{"name":"CLAUDE.md","status":"copy-fresh"},{"name":"skills","status":"no-source"},{"name":"agents","status":"no-source"},{"name":"commands","status":"no-source"},{"name":"plugins","status":"no-source"},{"name":"keybindings.json","status":"no-source"},{"name":"file-history","status":"no-source"},{"name":"plans","status":"no-source"},{"name":"tasks","status":"no-source"},{"name":"todos","status":"no-source"},{"name":"rules","status":"no-source"},{"name":"output-styles","status":"no-source"},{"name":"history.jsonl","status":"no-source"}]"#
    );
    // Both profiles change the same file; the paths in what is said are shown from the home folder.
    write(
        &profile_dir(paths, "acct2").join("settings.json"),
        "{\n  \"theme\": \"two\"\n}",
    );
    write(
        &profile_dir(paths, "acct3").join("settings.json"),
        "{\n  \"theme\": \"three\"\n}",
    );
    let at = parse_ms("2026-10-25T12:00:00.123Z").unwrap();
    let synced = sync_copies(paths, &["acct2", "acct3"], at).unwrap();
    let home_text = home.home.to_string_lossy().replace('\\', "/");
    let shown = |text: String| {
        text.replace("\\\\", "/")
            .replace('\\', "/")
            .replace(&home_text, "<home>")
    };
    assert_eq!(
        shown(serde_json::to_string(&synced).unwrap()),
        r#"[{"account":"acct2","name":"settings.json","action":"copied-back","detail":"~/.claude's previous settings.json is in <home>/.baton/backups/2026-10-25T12-00-00-123Z/copyback-settings.json"},{"account":"acct3","name":"settings.json","action":"conflict","detail":"kept acct2's change; this profile's is in <home>/.baton/backups/2026-10-25T12-00-00-123Z/conflict-acct3-settings.json"},{"account":"acct2","name":"CLAUDE.md","action":"ok","detail":"copy"},{"account":"acct3","name":"CLAUDE.md","action":"ok","detail":"copy"},{"account":"acct2","name":"keybindings.json","action":"no-source"},{"account":"acct3","name":"keybindings.json","action":"no-source"}]"#
    );
    assert_eq!(
        shown(fs::read_to_string(conflicts_file(paths)).unwrap()),
        "{\n  \"conflicts\": [\n    {\n      \"at\": \"2026-10-25T12:00:00.123Z\",\n      \"account\": \"acct3\",\n      \"item\": \"settings.json\",\n      \"backup\": \"<home>/.baton/backups/2026-10-25T12-00-00-123Z/conflict-acct3-settings.json\",\n      \"kept\": \"acct2\"\n    }\n  ]\n}\n"
    );
}
