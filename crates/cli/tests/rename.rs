//! Renaming accounts from the command line: a name is a label, and the id, the profile folder, the
//! files and the history stay.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot read what a command left stops the test at once"
)]

mod common;

use baton_core::clock::now_ms;
use baton_core::config::{Mark, load_config};
use baton_core::env::Env;
use baton_core::fsutil::read_json;
use baton_core::names::id_from_name;
use baton_core::paths::{Paths, account_from_env, profile_dir};
use baton_install::hooks::{handle_hook, handle_status_line};
use baton_testkit::temp_home;
use common::baton;
use serde_json::{Value, json};

/// A conversation's id.
const SID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";
/// An email as a name.
const EMAIL: &str = "jane.work+baton@example.com";

/// The accounts' ids and names, in order.
fn listed(paths: &Paths) -> Vec<(String, String)> {
    load_config(paths)
        .accounts
        .into_iter()
        .map(|a| (a.id, a.name))
        .collect()
}

/// An id and a name, owned.
fn pair(id: &str, name: &str) -> (String, String) {
    (id.to_owned(), name.to_owned())
}

/// The environment of a Claude Code that runs under an account's profile, and nothing else.
fn under(paths: &Paths, id: &str) -> Env {
    Env::from_pairs([("CLAUDE_CONFIG_DIR", profile_dir(paths, id).into_os_string())])
}

/// Whether a line of `out` holds these cells in a row, apart by spaces; `?` stands for any one
/// character (an account's mark).
fn has_row(out: &str, cells: &[&str]) -> bool {
    out.lines().any(|line| {
        let tokens: Vec<&str> = line.split(' ').filter(|token| !token.is_empty()).collect();
        tokens.windows(cells.len()).any(|window| {
            window.iter().zip(cells).all(|(token, cell)| {
                if *cell == "?" {
                    token.chars().count() == 1
                } else {
                    token == cell
                }
            })
        })
    })
}

#[test]
fn rename_from_the_command_line_the_name_changes_the_id_folder_files_and_history_stay() {
    let home = temp_home();
    let paths = &home.paths;
    baton(&home, &["account", "add", "acct1"]);
    baton(&home, &["account", "add", "acct2"]);
    let id = load_config(paths).accounts[1].id.clone();
    assert_eq!(id, "acct2");
    let now = now_ms();
    handle_status_line(
        &json!({
            "session_id": SID,
            "rate_limits": { "five_hour": { "used_percentage": 33, "resets_at": now / 1000 + 3600 } },
        }),
        &under(paths, &id),
        paths,
        now,
    );
    let r = baton(&home, &["account", "rename", "acct2", "work"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("Renamed acct2 to work"), "{}", r.stdout);
    assert_eq!(listed(paths), [pair("acct1", "acct1"), pair("acct2", "work")]);
    assert!(
        profile_dir(paths, "acct2").exists(),
        "the profile folder did not move"
    );
    assert!(!paths.accounts.join("work").exists());
    let status = baton(&home, &["status", "--json"]).json();
    let shown: Vec<Value> = status["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| json!([a["id"], a["name"]]))
        .collect();
    assert_eq!(shown, [json!(["acct1", "acct1"]), json!(["acct2", "work"])]);
    assert_eq!(
        status["accounts"][1]["five_hour"]["used"], 33,
        "the reading is still the account's"
    );
    let list = baton(&home, &["account", "list"]).stdout;
    assert!(
        has_row(&list, &["2", "?", "work", "normal", "folder", "acct2"]),
        "{list}"
    );
    // The new name works everywhere; the id still finds it.
    assert_eq!(
        baton(&home, &["account", "set", "work", "--mark", "preferred"]).code,
        0
    );
    assert_eq!(
        baton(&home, &["account", "set", "acct2", "--mark", "normal"]).code,
        0,
        "the id still finds it"
    );
    assert_eq!(load_config(paths).accounts[1].mark, Mark::Normal);
    // A new account may take the old name; it gets an id of its own.
    assert_eq!(baton(&home, &["account", "add", "acct2"]).code, 0);
    assert_eq!(listed(paths)[2], pair("acct2-2", "acct2"));
    assert!(profile_dir(paths, "acct2-2").exists());
    // Now "acct2" is a name: it means the new account.
    baton(&home, &["account", "set", "acct2", "--mark", "last-resort"]);
    let marks: Vec<Mark> = load_config(paths).accounts.iter().map(|a| a.mark).collect();
    assert_eq!(marks, [Mark::Normal, Mark::Normal, Mark::LastResort]);
}

#[test]
fn names_are_labels_and_never_control_characters_or_a_name_taken_in_another_case() {
    let home = temp_home();
    let paths = &home.paths;
    baton(&home, &["account", "add", "acct1"]);
    baton(&home, &["account", "add", "acct2"]);
    let long = "λ".repeat(120);
    for good in [
        EMAIL,
        "My Work Account",
        "Ελληνικά δοκιμή",
        "仕事",
        "a/b\\c:d",
        "-x",
        long.as_str(),
    ] {
        let r = baton(&home, &["account", "rename", "--", "acct2", good]);
        assert_eq!(r.code, 0, "{good}: {}", r.stderr);
        assert_eq!(load_config(paths).accounts[1].name, good);
        // Back by the id, whatever the name was.
        assert_eq!(baton(&home, &["account", "rename", "acct2", "acct2"]).code, 0);
    }
    let too_long = "λ".repeat(121);
    for bad in [
        "ACCT1",
        "acct1",
        " Acct1 ",
        "",
        "   ",
        "a\nb",
        "a\tb",
        "a\u{0007}b",
        "a\u{202E}b",
        too_long.as_str(),
    ] {
        let r = baton(&home, &["account", "rename", "acct2", bad]);
        assert_eq!(r.code, 2, "{bad:?}");
    }
    let said = |new_name: &str| baton(&home, &["account", "rename", "acct2", new_name]).stderr;
    assert!(said("ACCT1").contains("acct1 is the name of another account"));
    assert!(said("a\nb").contains("cannot hold control characters or line breaks"));
    assert!(said(&too_long).contains("at most 120 characters"));
    // Surrounding spaces are trimmed; its own name in another case is fine.
    assert_eq!(baton(&home, &["account", "rename", "acct2", "  Acct2  "]).code, 0);
    assert_eq!(load_config(paths).accounts[1].name, "Acct2");
    assert_eq!(baton(&home, &["account", "rename", "nobody", "x"]).code, 2);
    assert_eq!(baton(&home, &["account", "rename", "acct2"]).code, 2);
}

#[test]
fn the_cli_finds_an_account_by_its_name_or_by_its_id_and_files_use_the_id() {
    let home = temp_home();
    let paths = &home.paths;
    let added = baton(&home, &["account", "add", "My Work Account"]);
    assert_eq!(added.code, 0, "{}", added.stderr);
    assert!(
        added.stdout.contains("baton login \"My Work Account\""),
        "{}",
        added.stdout
    );
    assert_eq!(listed(paths), [pair("my-work-account", "My Work Account")]);
    assert!(profile_dir(paths, "my-work-account").exists());
    baton(&home, &["account", "add", EMAIL]);
    let b = load_config(paths).accounts[1].clone();
    assert_eq!(b.id, "jane-work-baton-example-com");
    assert_eq!(b.id, id_from_name(EMAIL));
    assert!(profile_dir(paths, &b.id).exists());
    assert_eq!(
        baton(&home, &["account", "set", "my work account", "--weekly", "80"]).code,
        0
    );
    assert_eq!(load_config(paths).accounts[0].weekly, Some(80.0));
    assert_eq!(
        baton(
            &home,
            &["account", "set", &EMAIL.to_uppercase(), "--mark", "preferred"]
        )
        .code,
        0
    );
    assert_eq!(
        baton(&home, &["account", "set", &b.id, "--mark", "last-resort"]).code,
        0
    );
    assert_eq!(load_config(paths).accounts[1].mark, Mark::LastResort);
    let list = baton(&home, &["account", "list"]).stdout;
    assert!(
        has_row(
            &list,
            &[
                "2",
                "?",
                EMAIL,
                "last",
                "resort",
                "folder",
                "jane-work-baton-example-com"
            ]
        ),
        "{list}"
    );
    // Ids from names that leave nothing usable, and clashes.
    assert_eq!(id_from_name("仕事"), "account");
    baton(&home, &["account", "add", "仕事"]);
    baton(&home, &["account", "add", "Ελληνικά"]);
    assert_eq!(
        listed(paths)[2..],
        [pair("account", "仕事"), pair("account-2", "Ελληνικά")]
    );
    // The hooks resolve the folder (the id), never the name.
    handle_hook(
        "prompt",
        &json!({ "session_id": SID }),
        &under(paths, &b.id),
        paths,
        now_ms(),
    );
    assert_eq!(
        read_json(&paths.sessions.join(SID).join("turn.json")).unwrap()["account"],
        b.id.as_str()
    );
    assert_eq!(account_from_env(&under(paths, &b.id), paths), Some(b.id));
}

#[test]
fn an_account_the_operator_names_default_is_an_account_like_any_other() {
    let home = temp_home();
    let paths = &home.paths;
    assert_eq!(baton(&home, &["account", "add", "default"]).code, 0);
    assert_eq!(listed(paths), [pair("default", "default")]);
    assert_eq!(profile_dir(paths, "default"), paths.accounts.join("default"));
    assert!(paths.accounts.join("default").exists());
    let status = baton(&home, &["status", "--json"]).json();
    assert_eq!(
        status["accounts"][0]["profile"],
        paths.accounts.join("default").to_string_lossy().as_ref()
    );
    // Plain claude (no CLAUDE_CONFIG_DIR) is not that account.
    assert_eq!(account_from_env(&Env::new(), paths), None);
    let input = json!({ "session_id": SID });
    handle_hook("prompt", &input, &Env::new(), paths, now_ms());
    assert!(!paths.sessions.join(SID).exists());
    handle_hook("prompt", &input, &under(paths, "default"), paths, now_ms());
    assert_eq!(
        read_json(&paths.sessions.join(SID).join("turn.json")).unwrap()["account"],
        "default"
    );
    assert_eq!(load_config(paths).version, 1);
}
