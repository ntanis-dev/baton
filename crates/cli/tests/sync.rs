//! Full sync from the command line: `baton sync`, the answer to a question, the policy switch and
//! what doctor says.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

mod common;

use baton_core::fsutil::read_json;
use baton_core::paths::global_config_file;
use baton_testkit::{TestHome, temp_home, write_json};
use common::baton;
use serde_json::{Value, json};

/// A home whose plain Claude Code has a theme, with three accounts added through the command line.
fn home() -> TestHome {
    let home = temp_home();
    write_json(
        &home.paths.claude_json,
        &json!({ "theme": "dark", "mcpServers": { "files": { "command": "files" } } }),
    );
    for name in ["Work account", "Spare", "Third"] {
        assert_eq!(baton(&home, &["account", "add", name]).code, 0);
    }
    home
}

/// An account's `.claude.json`.
fn settings(home: &TestHome, id: &str) -> Value {
    read_json(&global_config_file(&home.paths, id)).unwrap()
}

/// Changes an account's `.claude.json` the way Claude Code would while it runs there.
fn claude_writes(home: &TestHome, id: &str, change: impl FnOnce(&mut Value)) {
    let mut value = settings(home, id);
    change(&mut value);
    write_json(&global_config_file(&home.paths, id), &value);
}

/// A screen's words on one line each, without the spaces that only line them up.
fn words(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn an_added_account_has_what_the_others_have_and_baton_sync_says_everyone_is_in_step() {
    let home = home();
    for id in ["work-account", "spare", "third"] {
        let own = settings(&home, id);
        assert_eq!(own["theme"], "dark", "{id}");
        assert_eq!(
            own["mcpServers"],
            json!({ "files": { "command": "files" } }),
            "{id}"
        );
    }
    let sync = baton(&home, &["sync"]);
    assert_eq!(sync.code, 0);
    assert!(
        words(&sync.stdout)
            .iter()
            .any(|line| line.ends_with("Every account is in step.")),
        "{}",
        sync.stdout
    );
    assert_eq!(
        baton(&home, &["sync", "--json"]).json(),
        json!({ "brought": [], "written": [], "questions": [], "reset": [], "unreadable": [], "busy": false, "on": true })
    );
}

#[test]
fn what_one_account_changed_goes_to_the_others_and_what_two_changed_is_asked() {
    let home = home();
    claude_writes(&home, "work-account", |own| {
        own["theme"] = json!("light");
        own["editorMode"] = json!("vim");
        own["mcpServers"]["github"] = json!({ "command": "gh" });
    });
    claude_writes(&home, "spare", |own| own["theme"] = json!("dark-daltonized"));
    let sync = baton(&home, &["sync"]);
    assert_eq!(sync.code, 0);
    let said = words(&sync.stdout);
    assert!(
        said.iter()
            .any(|line| line.ends_with("Brought 2 accounts in step.")),
        "{}",
        sync.stdout
    );
    assert!(said.contains(&"Spare 2 settings".to_owned()), "{}", sync.stdout);
    assert!(said.contains(&"Third 3 settings".to_owned()), "{}", sync.stdout);
    assert!(said.contains(&"1 theme".to_owned()), "{}", sync.stdout);
    assert!(
        said.contains(&"\"light\" Work account, Third".to_owned()),
        "{}",
        sync.stdout
    );
    assert!(
        said.contains(&"\"dark-daltonized\" Spare".to_owned()),
        "{}",
        sync.stdout
    );
    assert!(
        said.contains(&"\"dark\" ~/.claude.json".to_owned()),
        "{}",
        sync.stdout
    );
    assert!(
        said.contains(&"Answer baton sync take <number> <account>".to_owned()),
        "{}",
        sync.stdout
    );
    // Nobody's own value was written over; what did not clash arrived.
    assert_eq!(settings(&home, "spare")["theme"], "dark-daltonized");
    assert_eq!(settings(&home, "spare")["editorMode"], "vim");
    assert_eq!(
        settings(&home, "third")["mcpServers"]["github"],
        json!({ "command": "gh" })
    );
    // Doctor says so until it is answered.
    let doctor = baton(&home, &["doctor", "--json"]).json();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "full sync")
        .unwrap();
    assert_eq!(check["ok"], false);
    assert_eq!(check["detail"], "the accounts differ on 1 setting: baton sync");

    // What cannot be an answer.
    let none = baton(&home, &["sync", "take", "1"]);
    assert_eq!(
        (none.code, none.stderr.as_str()),
        (2, "baton: baton sync take <number|all> <account|home>\n")
    );
    let far = baton(&home, &["sync", "take", "4", "Spare"]);
    assert_eq!(
        (far.code, far.stderr.as_str()),
        (2, "baton: baton sync take takes a number from 1 to 1, or all.\n")
    );
    let nobody = baton(&home, &["sync", "take", "1", "Nobody"]);
    assert_eq!(
        (nobody.code, nobody.stderr.as_str()),
        (2, "baton: No account named Nobody.\n")
    );
    assert_eq!(
        baton(&home, &["sync", "nonsense"]).stderr,
        "baton: Unknown: baton sync nonsense\n"
    );

    // The answer: Spare's theme, for everyone.
    let took = baton(&home, &["sync", "take", "1", "spare"]);
    assert_eq!(took.code, 0);
    let said = words(&took.stdout);
    assert!(
        said.iter()
            .any(|line| line.ends_with("Took Spare's value for 1 setting.")),
        "{}",
        took.stdout
    );
    for id in ["work-account", "spare", "third"] {
        assert_eq!(settings(&home, id)["theme"], "dark-daltonized", "{id}");
    }
    assert_eq!(
        read_json(&home.paths.claude_json).unwrap()["theme"],
        "dark",
        "~/.claude.json is only read"
    );
    let after = baton(&home, &["sync", "take", "1", "spare"]);
    assert_eq!(
        (after.code, after.stderr.as_str()),
        (2, "baton: The accounts differ on nothing.\n")
    );
    let doctor = baton(&home, &["doctor", "--json"]).json();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "full sync")
        .unwrap();
    assert_eq!(check["ok"], Value::Null);
    assert_eq!(check["detail"], "on: what is set on one account is set on all");
}

#[test]
fn home_can_be_the_answer_too() {
    let home = home();
    claude_writes(&home, "work-account", |own| own["theme"] = json!("light"));
    claude_writes(&home, "spare", |own| own["theme"] = json!("dark-daltonized"));
    assert_eq!(baton(&home, &["sync"]).code, 0);
    assert_eq!(baton(&home, &["sync", "take", "all", "home"]).code, 0);
    for id in ["work-account", "spare", "third"] {
        assert_eq!(settings(&home, id)["theme"], "dark", "{id}");
    }
}

#[test]
fn full_sync_is_a_policy_setting_that_is_on_unless_turned_off() {
    let home = home();
    let policy = baton(&home, &["policy", "--json"]).json();
    assert_eq!(policy["fullSync"], true);
    let off = baton(&home, &["policy", "--full-sync", "off"]);
    assert_eq!(off.code, 0);
    assert!(
        words(&off.stdout).contains(&"full-sync false changed".to_owned()),
        "{}",
        off.stdout
    );
    claude_writes(&home, "work-account", |own| own["theme"] = json!("light"));
    let sync = baton(&home, &["sync"]);
    assert_eq!(sync.code, 0);
    assert!(
        words(&sync.stdout)
            .iter()
            .any(|line| line.ends_with("Full sync is off.")),
        "{}",
        sync.stdout
    );
    assert_eq!(baton(&home, &["sync", "--json"]).json(), json!({ "on": false }));
    assert_eq!(settings(&home, "spare")["theme"], "dark");
    let doctor = baton(&home, &["doctor", "--json"]).json();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["name"] == "full sync")
        .unwrap();
    assert_eq!(
        check["detail"],
        "off: each account keeps its own settings (baton policy --full-sync on)"
    );
    // Back on: the change made meanwhile goes to the others.
    assert_eq!(baton(&home, &["policy", "--full-sync", "default"]).code, 0);
    assert_eq!(baton(&home, &["sync"]).code, 0);
    assert_eq!(settings(&home, "spare")["theme"], "light");
}
