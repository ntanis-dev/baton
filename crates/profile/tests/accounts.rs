//! Adding, renaming, ordering, changing and removing accounts.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot set the stage stops the test at once, as the tests do"
)]

mod common;

use std::fs;

use baton_core::config::{By, Mark, effective_policy, load_config};
use baton_core::env::Env;
use baton_core::json::Object;
use baton_core::names::id_from_name;
use baton_core::paths::{Paths, account_from_env, profile_dir};
use baton_profile::accounts::{
    AccountChanges, AccountError, AddOptions, add_account, move_account, order_accounts, remove_account,
    rename_account, set_account, set_policy,
};
use common::{temp_home, write, write_json};
use serde_json::{Value, json};

/// An email as a name: the longest kind of name people use.
const EMAIL: &str = "jane.work+baton@example.com";

/// Adds an account with nothing special about it.
fn add(paths: &Paths, name: &str) -> Result<baton_profile::accounts::Added, AccountError> {
    add_account(paths, name, &AddOptions::default())
}

/// The message of an operation the operator asked for that cannot be done.
fn refused<T: std::fmt::Debug>(result: Result<T, AccountError>) -> String {
    match result {
        Err(error) if error.is_invalid() => error.to_string(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The accounts' ids and names, in order.
fn listed(paths: &Paths) -> Vec<(String, String)> {
    load_config(paths)
        .accounts
        .into_iter()
        .map(|a| (a.id, a.name))
        .collect()
}

/// The accounts' names, in order.
fn names(paths: &Paths) -> Vec<String> {
    load_config(paths).accounts.into_iter().map(|a| a.name).collect()
}

/// Policy changes from a JSON object.
fn changes(value: Value) -> Object {
    value.as_object().cloned().unwrap()
}

#[test]
fn changes_made_at_the_same_time_all_land() {
    let home = temp_home();
    let paths = &home.paths;
    add(paths, "first").unwrap();
    // Several batons at once: each reads the config, changes it and saves it back.
    let adding: Vec<_> = (0..4)
        .map(|n| {
            let paths = paths.clone();
            std::thread::spawn(move || add(&paths, &format!("more{n}")).map(|added| added.account.name))
        })
        .collect();
    for thread in adding {
        thread.join().unwrap().unwrap();
    }
    let mut all = names(paths);
    all.sort();
    assert_eq!(all, ["first", "more0", "more1", "more2", "more3"]);
}

#[test]
fn accounts_add_order_marks_thresholds_remove() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(&paths.claude_dir.join("settings.json"), &json!({}));
    add(paths, "acct1").unwrap();
    let added = add(paths, "acct2").unwrap();
    assert_eq!(
        (added.account.id.as_str(), added.account.name.as_str()),
        ("acct2", "acct2")
    );
    assert_eq!(
        (added.position, added.account.mark, added.existed),
        (2, Mark::Normal, false)
    );
    assert_eq!(added.profile, profile_dir(paths, "acct2"));
    assert!(added.profile.is_dir());
    assert!(
        added
            .links
            .iter()
            .any(|l| l.name == "settings.json" && ["copied", "linked", "ok"].contains(&l.action))
    );
    // As --json prints it.
    let shown = serde_json::to_value(&added).unwrap();
    assert_eq!(
        shown["account"],
        json!({ "id": "acct2", "name": "acct2", "mark": "normal", "fiveHour": null, "weekly": null })
    );
    assert_eq!(shown["position"], 2);
    assert_eq!(shown["existed"], false);
    // With full sync (the default) a shared folder that ~/.claude lacks is made there and linked.
    assert_eq!(
        shown["links"][0],
        json!({ "name": "projects", "action": "linked" })
    );
    assert!(paths.claude_dir.join("projects").is_dir());
    assert_eq!(
        shown["links"].as_array().unwrap().last().unwrap(),
        &json!({ "name": ".claude.json", "action": "ok", "detail": "in step" })
    );
    add_account(
        paths,
        "acct3",
        &AddOptions {
            mark: Some("last-resort".into()),
            ..AddOptions::default()
        },
    )
    .unwrap();
    let first = AddOptions {
        mark: Some("preferred".into()),
        position: Some(json!("1")),
        file_links: None,
    };
    assert_eq!(add_account(paths, "acct4", &first).unwrap().position, 1);
    assert_eq!(
        refused(add(paths, "acct2")),
        "acct2 is the name of another account."
    );
    // Names are labels (any printable text), never control characters.
    assert_eq!(
        refused(add(paths, "bad\nname")),
        "A name cannot hold control characters or line breaks."
    );
    assert_eq!(
        refused(add(paths, "ACCT2")),
        "acct2 is the name of another account."
    );
    assert_eq!(
        refused(add_account(
            paths,
            "x",
            &AddOptions {
                mark: Some("best".into()),
                ..AddOptions::default()
            }
        )),
        "A mark is one of preferred, normal, last-resort."
    );
    for position in [json!(0), json!("6"), json!("first"), json!(1.5)] {
        let options = AddOptions {
            position: Some(position.clone()),
            ..AddOptions::default()
        };
        assert_eq!(
            refused(add_account(paths, "x", &options)),
            "--position takes 1 to 5.",
            "{position}"
        );
    }
    assert!(
        !profile_dir(paths, "x").exists(),
        "a refused account makes no folder"
    );
    let marks: Vec<(String, Mark)> = load_config(paths)
        .accounts
        .into_iter()
        .map(|a| (a.name, a.mark))
        .collect();
    assert_eq!(
        marks,
        [
            ("acct4".to_owned(), Mark::Preferred),
            ("acct1".to_owned(), Mark::Normal),
            ("acct2".to_owned(), Mark::Normal),
            ("acct3".to_owned(), Mark::LastResort)
        ]
    );
    // The new order: the accounts given first, the rest keep theirs.
    assert_eq!(
        order_accounts(paths, &["acct1", "acct2"], By::Name)
            .unwrap()
            .order,
        ["acct1", "acct2", "acct4", "acct3"]
    );
    assert_eq!(
        refused(order_accounts(paths, &["acct1", "ACCT1"], By::Name)),
        "An account appears twice."
    );
    assert_eq!(
        refused(order_accounts(paths, &["acct1", "acct1", "nobody"], By::Name)),
        "No account named nobody."
    );
    assert_eq!(
        move_account(paths, "acct3", &json!("2"), By::Name).unwrap().order,
        ["acct1", "acct3", "acct2", "acct4"]
    );
    assert_eq!(
        move_account(paths, "acct1", &json!(4), By::Id).unwrap().order,
        ["acct3", "acct2", "acct4", "acct1"]
    );
    for position in [json!(0), json!(5), json!("two")] {
        assert_eq!(
            refused(move_account(paths, "acct3", &position, By::Name)),
            "A position is 1 to 4."
        );
    }
    let set = set_account(
        paths,
        "acct2",
        &AccountChanges {
            mark: Some("preferred".into()),
            five_hour: Some(json!("80")),
            weekly: Some(json!(90)),
            ..AccountChanges::default()
        },
        By::Name,
    )
    .unwrap();
    assert_eq!(
        (set.account.mark, set.account.five_hour, set.account.weekly),
        (Mark::Preferred, Some(80.0), Some(90.0))
    );
    assert_eq!(
        serde_json::to_value(&set).unwrap(),
        json!({ "account": { "id": "acct2", "name": "acct2", "mark": "preferred", "fiveHour": 80, "weekly": 90 } })
    );
    let reset = set_account(
        paths,
        "acct2",
        &AccountChanges {
            five_hour: Some(Value::Null),
            last_resort: Some(true),
            ..AccountChanges::default()
        },
        By::Name,
    )
    .unwrap();
    assert_eq!(
        (reset.account.mark, reset.account.five_hour, reset.account.weekly),
        (Mark::LastResort, None, Some(90.0))
    );
    // Not a last resort any more: normal again; an account that was not one keeps its mark.
    let back = AccountChanges {
        last_resort: Some(false),
        ..AccountChanges::default()
    };
    assert_eq!(
        set_account(paths, "acct2", &back, By::Name).unwrap().account.mark,
        Mark::Normal
    );
    assert_eq!(
        set_account(paths, "acct4", &back, By::Name).unwrap().account.mark,
        Mark::Preferred
    );
    for bad in [json!("120"), json!(0), json!("many"), json!("")] {
        let weekly = AccountChanges {
            weekly: Some(bad),
            ..AccountChanges::default()
        };
        assert_eq!(
            refused(set_account(paths, "acct2", &weekly, By::Name)),
            "--weekly takes a percentage from 1 to 100."
        );
        let five = AccountChanges {
            five_hour: weekly.weekly.clone(),
            ..AccountChanges::default()
        };
        assert_eq!(
            refused(set_account(paths, "acct2", &five, By::Name)),
            "--five-hour takes a percentage from 1 to 100."
        );
    }
    let mark = AccountChanges {
        mark: Some("normal".into()),
        ..AccountChanges::default()
    };
    assert_eq!(
        refused(set_account(paths, "nobody", &mark, By::Name)),
        "No account named nobody."
    );
    let removed = remove_account(paths, "acct2", By::Name).unwrap();
    assert_eq!((removed.id.as_str(), removed.name.as_str()), ("acct2", "acct2"));
    assert_eq!(removed.profile, profile_dir(paths, "acct2"));
    assert!(removed.profile.is_dir(), "the folder stays");
    let settings = removed.profile.join("settings.json");
    assert!(!settings.exists() || !fs::symlink_metadata(&settings).unwrap().file_type().is_symlink());
    remove_account(paths, "acct1", By::Name).unwrap();
    remove_account(paths, "acct3", By::Id).unwrap();
    // The last account stays.
    assert_eq!(
        refused(remove_account(paths, "acct4", By::Name)),
        "acct4 is the only account and Baton needs one: add another (baton account add <name>) before removing it."
    );
    let config: Value = baton_core::fsutil::read_json(&paths.config).unwrap();
    assert_eq!(config["version"], 1);
    assert_eq!(names(paths), ["acct4"]);
}

#[test]
fn a_config_that_cannot_be_read_stops_a_change_instead_of_being_saved_over_every_account() {
    let home = temp_home();
    let paths = &home.paths;
    let broken = "{\n  \"version\": 1,\n  \"accounts\": [\n    { \"id\": \"acct1\", \"name\": \"acct1\", \"mark\": \"normal\" },\n    { \"id\": \"acct2\", \"name\": \"acct2\", \"mark\": \"normal\" },\n  ]\n}\n";
    write(&paths.config, broken);
    let mark = AccountChanges {
        mark: Some("preferred".into()),
        ..AccountChanges::default()
    };
    let messages = [
        refused(add(paths, "acct3")),
        refused(rename_account(paths, "acct1", "one", By::Name)),
        refused(set_account(paths, "acct1", &mark, By::Name)),
        refused(set_policy(paths, &changes(json!({ "mode": "order" })))),
        refused(remove_account(paths, "acct1", By::Name)),
        refused(order_accounts(paths, &["acct1"], By::Name)),
        refused(move_account(paths, "acct1", &json!(1), By::Name)),
    ];
    for message in messages {
        assert!(message.contains("config.json is not valid JSON"), "{message}");
    }
    assert_eq!(fs::read_to_string(&paths.config).unwrap(), broken);
    assert!(!profile_dir(paths, "acct3").exists());
    // Nor one that is not a config.
    write_json(
        &paths.config,
        &json!({ "version": 1, "accounts": { "acct1": {} } }),
    );
    assert!(refused(add(paths, "acct3")).contains("is not a Baton config"));
    // None at all is a first run: the change makes one.
    fs::remove_file(&paths.config).unwrap();
    add(paths, "acct3").unwrap();
    assert_eq!(names(paths), ["acct3"]);
}

#[test]
fn a_rename_changes_the_name_and_the_id_and_folder_stay() {
    let home = temp_home();
    let paths = &home.paths;
    add(paths, "acct1").unwrap();
    add(paths, "acct2").unwrap();
    let renamed = rename_account(paths, "acct2", "work", By::Name).unwrap();
    assert_eq!(
        serde_json::to_value(&renamed).unwrap(),
        json!({ "id": "acct2", "from": "acct2", "to": "work" })
    );
    assert_eq!(
        listed(paths),
        [("acct1".into(), "acct1".into()), ("acct2".into(), "work".into())]
    );
    assert!(
        profile_dir(paths, "acct2").is_dir(),
        "the profile folder did not move"
    );
    assert!(!paths.accounts.join("work").exists());
    // The new name works everywhere; the id still finds it.
    let mark = |word: &str| AccountChanges {
        mark: Some(word.into()),
        ..AccountChanges::default()
    };
    set_account(paths, "work", &mark("preferred"), By::Name).unwrap();
    set_account(paths, "acct2", &mark("normal"), By::Name).unwrap();
    assert_eq!(load_config(paths).accounts[1].mark, Mark::Normal);
    // By id only (the menu): the name does not find it.
    assert_eq!(
        refused(set_account(paths, "work", &mark("preferred"), By::Id)),
        "No account named work."
    );
    assert_eq!(
        rename_account(paths, "acct2", "work 2", By::Id).unwrap().from,
        "work"
    );
    rename_account(paths, "work 2", "work", By::Name).unwrap();
    // A new account may take the old name; it gets an id of its own.
    let third = add(paths, "acct2").unwrap();
    assert_eq!(
        (third.account.id.as_str(), third.account.name.as_str()),
        ("acct2-2", "acct2")
    );
    assert!(profile_dir(paths, "acct2-2").is_dir());
    // Now "acct2" is a name: it means the new account.
    set_account(paths, "acct2", &mark("last-resort"), By::Name).unwrap();
    let marks: Vec<Mark> = load_config(paths).accounts.iter().map(|a| a.mark).collect();
    assert_eq!(marks, [Mark::Normal, Mark::Normal, Mark::LastResort]);
}

#[test]
fn names_are_labels_and_never_control_characters_or_a_name_taken_in_another_case() {
    let home = temp_home();
    let paths = &home.paths;
    add(paths, "acct1").unwrap();
    add(paths, "acct2").unwrap();
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
        rename_account(paths, "acct2", good, By::Name).unwrap_or_else(|error| panic!("{good}: {error}"));
        assert_eq!(load_config(paths).accounts[1].name, good);
        // Back by the id, whatever the name was.
        rename_account(paths, "acct2", "acct2", By::Name).unwrap();
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
        assert!(
            rename_account(paths, "acct2", bad, By::Name).is_err_and(|e| e.is_invalid()),
            "{bad:?}"
        );
    }
    assert_eq!(
        refused(rename_account(paths, "acct2", "ACCT1", By::Name)),
        "acct1 is the name of another account."
    );
    assert_eq!(
        refused(rename_account(paths, "acct2", "a\nb", By::Name)),
        "A name cannot hold control characters or line breaks."
    );
    assert_eq!(
        refused(rename_account(paths, "acct2", &too_long, By::Name)),
        "A name is at most 120 characters."
    );
    assert_eq!(
        refused(rename_account(paths, "acct2", "  ", By::Name)),
        "A name cannot be empty."
    );
    // Surrounding spaces are trimmed; its own name in another case is fine.
    rename_account(paths, "acct2", "  Acct2  ", By::Name).unwrap();
    assert_eq!(load_config(paths).accounts[1].name, "Acct2");
    assert_eq!(
        refused(rename_account(paths, "nobody", "x", By::Name)),
        "No account named nobody."
    );
}

#[test]
fn an_account_is_found_by_its_name_or_by_its_id_and_files_use_the_id() {
    let home = temp_home();
    let paths = &home.paths;
    assert_eq!(
        refused(rename_account(paths, "nobody", "x", By::Name)),
        "No account named nobody: Baton has no accounts yet (baton account add <name>)."
    );
    let a = add(paths, "My Work Account").unwrap().account;
    assert_eq!(
        (a.id.as_str(), a.name.as_str()),
        ("my-work-account", "My Work Account")
    );
    assert!(profile_dir(paths, "my-work-account").is_dir());
    let b = add(paths, EMAIL).unwrap().account;
    assert_eq!(b.id, "jane-work-baton-example-com");
    assert_eq!(b.id, id_from_name(EMAIL));
    assert!(profile_dir(paths, &b.id).is_dir());
    let weekly = AccountChanges {
        weekly: Some(json!("80")),
        ..AccountChanges::default()
    };
    set_account(paths, "my work account", &weekly, By::Name).unwrap();
    assert_eq!(load_config(paths).accounts[0].weekly, Some(80.0));
    let mark = |word: &str| AccountChanges {
        mark: Some(word.into()),
        ..AccountChanges::default()
    };
    set_account(paths, &EMAIL.to_uppercase(), &mark("preferred"), By::Name).unwrap();
    set_account(paths, &b.id, &mark("last-resort"), By::Name).unwrap();
    assert_eq!(load_config(paths).accounts[1].mark, Mark::LastResort);
    // Ids from names that leave nothing usable, and clashes.
    assert_eq!(id_from_name("仕事"), "account");
    add(paths, "仕事").unwrap();
    add(paths, "Ελληνικά").unwrap();
    assert_eq!(
        listed(paths)[2..],
        [
            ("account".to_owned(), "仕事".to_owned()),
            ("account-2".to_owned(), "Ελληνικά".to_owned())
        ]
    );
    // A folder that is already there (an account removed earlier) is never taken over.
    fs::create_dir_all(profile_dir(paths, "spare")).unwrap();
    assert_eq!(add(paths, "Spare").unwrap().account.id, "spare-2");
    // An id that differs only by case from another is not used either.
    write_json(
        &paths.config,
        &json!({ "version": 1, "accounts": [{ "id": "Mixed", "name": "first" }], "policy": {} }),
    );
    assert_eq!(add(paths, "mixed").unwrap().account.id, "mixed-2");
    // The profile's folder resolves to the id, never the name.
    let env = Env::from_pairs([("CLAUDE_CONFIG_DIR", profile_dir(paths, &b.id).into_os_string())]);
    assert_eq!(account_from_env(&env, paths), Some(b.id));
}

#[test]
fn an_account_named_default_is_an_account_like_any_other() {
    let home = temp_home();
    let paths = &home.paths;
    let a = add(paths, "default").unwrap().account;
    assert_eq!((a.id.as_str(), a.name.as_str()), ("default", "default"));
    assert_eq!(profile_dir(paths, "default"), paths.accounts.join("default"));
    assert!(paths.accounts.join("default").is_dir());
    // Plain claude (no CLAUDE_CONFIG_DIR) is not that account, and ~/.claude never is.
    assert_eq!(account_from_env(&Env::new(), paths), None);
    let plain = Env::from_pairs([("CLAUDE_CONFIG_DIR", paths.claude_dir.clone().into_os_string())]);
    assert_eq!(account_from_env(&plain, paths), None);
    let own = Env::from_pairs([(
        "CLAUDE_CONFIG_DIR",
        paths.accounts.join("default").into_os_string(),
    )]);
    assert_eq!(account_from_env(&own, paths), Some("default".to_owned()));
    assert_eq!(load_config(paths).version, 1);
}

#[test]
fn policy_settings_are_checked_kept_and_put_back() {
    let home = temp_home();
    let paths = &home.paths;
    add(paths, "acct1").unwrap();
    let set = set_policy(
        paths,
        &changes(json!({ "mode": "order", "startReserve": "25", "statusLineUsage": "on" })),
    )
    .unwrap();
    assert_eq!(
        Value::Object(set.policy),
        json!({ "mode": "order", "startReserve": 25, "statusLineUsage": true })
    );
    let policy = effective_policy(&load_config(paths));
    assert_eq!(
        (
            policy.mode.as_str(),
            policy.start_reserve,
            policy.status_line_usage
        ),
        ("order", 25.0, true)
    );
    assert_eq!(
        refused(set_policy(paths, &changes(json!({ "mode": "chaos" })))),
        "mode is one of smart, order."
    );
    assert_eq!(
        refused(set_policy(paths, &changes(json!({ "startReserve": "-5" })))),
        "startReserve takes a number from 0 to 100."
    );
    assert_eq!(
        refused(set_policy(paths, &changes(json!({ "nope": 1 })))),
        "Unknown setting nope."
    );
    // One bad value: nothing of the change is saved.
    assert!(set_policy(paths, &changes(json!({ "weekly": 80, "fiveHour": 500 }))).is_err());
    assert_eq!(effective_policy(&load_config(paths)).weekly, 95.0);
    // Null puts a setting back to its default.
    let back = set_policy(paths, &changes(json!({ "mode": null, "nope": null }))).unwrap();
    assert_eq!(
        Value::Object(back.policy),
        json!({ "startReserve": 25, "statusLineUsage": true })
    );
    assert_eq!(effective_policy(&load_config(paths)).mode.as_str(), "smart");
    assert_eq!(names(paths), ["acct1"], "the accounts stay");
}
