//! What the JavaScript implementation gave for the same inputs, byte for byte: the seed of a profile's
//! `.claude.json`.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once, as the tests do"
)]

use baton_profile::profile::seed_global_config;
use serde_json::{Value, json};

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
