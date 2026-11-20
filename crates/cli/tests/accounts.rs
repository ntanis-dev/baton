//! `baton account` and `baton policy` from the command line: what is typed, what is printed, the exit
//! code, and what is left in config.json.

#![allow(
    clippy::unwrap_used,
    reason = "a helper that cannot read what a command left stops the test at once"
)]

mod common;

use std::fs;

use baton_core::config::{Mark, load_config};
use baton_core::fsutil::read_json;
use baton_core::paths::{Paths, profile_dir};
use baton_testkit::{temp_home, write, write_json};
use common::baton;
use serde_json::json;

/// The names on the numbered lines of an order screen, top to bottom.
fn order(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|line| {
            let numbered = line.trim_start();
            let digits = numbered.chars().take_while(char::is_ascii_digit).count();
            let after = &numbered[digits..];
            let name = after.trim_start();
            let shaped =
                numbered.len() < line.len() && digits > 0 && name.len() < after.len() && !name.is_empty();
            shaped.then(|| name.to_owned())
        })
        .collect()
}

/// A label, `spaces` spaces and a value: a row of a screen as it is printed.
fn row(label: &str, spaces: usize, value: &str) -> String {
    format!("{label}{}{value}", " ".repeat(spaces))
}

/// The accounts' names and marks, in order.
fn marks(paths: &Paths) -> Vec<(String, Mark)> {
    load_config(paths)
        .accounts
        .into_iter()
        .map(|a| (a.name, a.mark))
        .collect()
}

#[test]
fn accounts_add_order_marks_thresholds_remove() {
    let home = temp_home();
    let paths = &home.paths;
    write_json(&paths.claude_dir.join("settings.json"), &json!({}));
    assert_eq!(baton(&home, &["account", "add", "acct1"]).code, 0);
    let added = baton(&home, &["account", "add", "acct2"]);
    assert_eq!(added.code, 0, "{}", added.stderr);
    for said in [
        "Added acct2",
        &row("Position", 4, "#2"),
        &row("Mark", 8, "normal"),
        "baton login acct2",
    ] {
        assert!(added.stdout.contains(said), "{said}: {}", added.stdout);
    }
    assert!(profile_dir(paths, "acct2").exists());
    assert_eq!(
        baton(&home, &["account", "add", "acct3", "--last-resort"]).code,
        0
    );
    assert_eq!(
        baton(
            &home,
            &[
                "account",
                "add",
                "acct4",
                "--position",
                "1",
                "--mark",
                "preferred"
            ]
        )
        .code,
        0
    );
    assert_eq!(baton(&home, &["account", "add", "acct2"]).code, 2);
    // Names are labels (any printable text), never control characters.
    assert_eq!(baton(&home, &["account", "add", "bad\nname"]).code, 2);
    assert_eq!(baton(&home, &["account", "add", "ACCT2"]).code, 2);
    let list = baton(&home, &["account", "list", "--json"]).json();
    let listed: Vec<(&str, &str)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["name"].as_str().unwrap(), a["mark"].as_str().unwrap()))
        .collect();
    assert_eq!(
        listed,
        [
            ("acct4", "preferred"),
            ("acct1", "normal"),
            ("acct2", "normal"),
            ("acct3", "last-resort")
        ]
    );
    // The new order, one numbered line per account.
    assert_eq!(
        order(&baton(&home, &["account", "order", "acct1", "acct2"]).stdout),
        ["acct1", "acct2", "acct4", "acct3"]
    );
    assert_eq!(
        order(&baton(&home, &["account", "move", "acct3", "2"]).stdout),
        ["acct1", "acct3", "acct2", "acct4"]
    );
    let set = baton(
        &home,
        &[
            "account",
            "set",
            "acct2",
            "--five-hour",
            "80",
            "--weekly",
            "90",
            "--mark",
            "preferred",
        ],
    );
    let rows = [
        row("Mark", 8, "preferred"),
        row("5-hour", 6, "80%"),
        row("Weekly", 6, "90%"),
    ];
    for said in ["Updated acct2"]
        .into_iter()
        .chain(rows.iter().map(String::as_str))
    {
        assert!(set.stdout.contains(said), "{said}: {}", set.stdout);
    }
    let reset = baton(
        &home,
        &[
            "account",
            "set",
            "acct2",
            "--five-hour",
            "default",
            "--last-resort",
            "true",
        ],
    );
    for said in [row("Mark", 8, "last resort"), row("5-hour", 6, "default")] {
        assert!(reset.stdout.contains(&said), "{said}: {}", reset.stdout);
    }
    assert_eq!(
        baton(&home, &["account", "set", "acct2", "--weekly", "120"]).code,
        2
    );
    assert_eq!(
        baton(&home, &["account", "set", "nobody", "--mark", "normal"]).code,
        2
    );
    let removed = baton(&home, &["account", "remove", "acct2"]);
    assert!(
        removed.stdout.contains("still holds that account's sign-in"),
        "{}",
        removed.stdout
    );
    assert!(profile_dir(paths, "acct2").exists(), "the folder stays");
    let settings = profile_dir(paths, "acct2").join("settings.json");
    assert!(!settings.exists() || !fs::symlink_metadata(&settings).unwrap().file_type().is_symlink());
    assert_eq!(baton(&home, &["account", "remove", "acct1"]).code, 0);
    assert_eq!(baton(&home, &["account", "remove", "acct3"]).code, 0);
    // The last account stays.
    let last = baton(&home, &["account", "remove", "acct4"]);
    assert_eq!(last.code, 2);
    assert!(
        last.stderr
            .contains("acct4 is the only account and Baton needs one"),
        "{}",
        last.stderr
    );
    let config = read_json(&paths.config).unwrap();
    assert_eq!(config["version"], 1);
    assert_eq!(marks(paths), [("acct4".to_owned(), Mark::Preferred)]);
}

#[test]
fn a_config_that_cannot_be_read_stops_a_change_instead_of_being_saved_over_every_account() {
    let home = temp_home();
    let paths = &home.paths;
    let broken = "{\n  \"version\": 1,\n  \"accounts\": [\n    { \"id\": \"acct1\", \"name\": \"acct1\", \"mark\": \"normal\" },\n    { \"id\": \"acct2\", \"name\": \"acct2\", \"mark\": \"normal\" },\n  ]\n}\n";
    write(&paths.config, broken);
    let changes: [&[&str]; 4] = [
        &["account", "add", "acct3"],
        &["account", "rename", "acct1", "one"],
        &["account", "set", "acct1", "--mark", "preferred"],
        &["policy", "--mode", "order"],
    ];
    for argv in changes {
        let r = baton(&home, argv);
        assert_eq!(r.code, 2, "{}", argv.join(" "));
        assert!(
            r.stderr.contains("config.json is not valid JSON"),
            "{}: {}",
            argv.join(" "),
            r.stderr
        );
    }
    assert_eq!(fs::read_to_string(&paths.config).unwrap(), broken);
    // Nor one that is not a config.
    write_json(
        &paths.config,
        &json!({ "version": 1, "accounts": { "acct1": {} } }),
    );
    let not_config = baton(&home, &["account", "add", "acct3"]);
    assert_eq!(not_config.code, 2);
    assert!(
        not_config.stderr.contains("is not a Baton config"),
        "{}",
        not_config.stderr
    );
    // None at all is a first run: the change makes one.
    fs::remove_file(&paths.config).unwrap();
    assert_eq!(baton(&home, &["account", "add", "acct3"]).code, 0);
}

#[test]
fn policy_show_change_and_reset_settings() {
    let home = temp_home();
    let shown = baton(&home, &["policy"]);
    assert!(
        shown.stdout.contains(&row("mode", 18, "smart  default")),
        "{}",
        shown.stdout
    );
    let changed = baton(
        &home,
        &[
            "policy",
            "--mode",
            "order",
            "--start-reserve",
            "30",
            "--min-stay",
            "45",
        ],
    );
    assert_eq!(changed.code, 0, "{}", changed.stderr);
    let json = baton(&home, &["policy", "--json"]).json();
    assert_eq!(
        json!([
            json["mode"],
            json["startReserve"],
            json["minStayMinutes"],
            json["planFactor"]
        ]),
        json!(["order", 30, 45, 1.5])
    );
    assert_eq!(baton(&home, &["policy", "--mode", "chaos"]).code, 2);
    assert_eq!(baton(&home, &["policy", "--start-reserve", "-5"]).code, 2);
    baton(&home, &["policy", "--mode", "default"]);
    assert_eq!(baton(&home, &["policy", "--json"]).json()["mode"], "smart");
}
