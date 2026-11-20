//! `baton install` and `baton uninstall` from the command line: the dry run, the screen's words, what
//! is written and what is put back.

mod common;
mod support;

use baton_core::paths::profile_dir;
use baton_testkit::{temp_home, write_json};
use serde_json::json;
use support::{after, baton, json_of, squash};

#[test]
fn install_and_uninstall_from_the_command_line_with_a_dry_run_first() {
    let home = temp_home();
    let paths = &home.paths;
    let settings = paths.claude_dir.join("settings.json");
    write_json(&settings, &json!({ "theme": "dark" }));
    assert_eq!(baton(&home, &["account", "add", "acct2"]).code, 0);
    let dry = baton(&home, &["install", "--dry-run"]);
    assert_eq!(dry.code, 0, "{}", dry.stderr);
    assert!(
        squash(&dry.stdout).contains("will add baton statusline"),
        "{}",
        dry.stdout
    );
    assert!(
        after(&dry.stdout, "change")
            .iter()
            .any(|path| path.ends_with("settings.json")),
        "{}",
        dry.stdout
    );
    assert!(
        dry.stdout.contains("Dry run: nothing was written"),
        "{}",
        dry.stdout
    );
    assert!(
        !dry.stdout.contains("\"hooks\""),
        "the diff is only shown on request"
    );
    let diff = baton(&home, &["install", "--dry-run", "--diff"]);
    assert!(squash(&diff.stdout).contains("+ \"hooks\": {"), "{}", diff.stdout);
    assert_eq!(json_of(&settings), json!({ "theme": "dark" }));
    assert!(!paths.install.exists());

    let done = baton(&home, &["install"]);
    assert_eq!(done.code, 0, "{}", done.stderr);
    assert!(done.stdout.contains("Installed"), "{}", done.stdout);
    assert!(
        after(&done.stdout, "Backup")
            .iter()
            .any(|path| path.contains("backups")),
        "{}",
        done.stdout
    );
    assert!(
        squash(&done.stdout).contains("Undo baton uninstall"),
        "{}",
        done.stdout
    );
    assert!(baton(&home, &["install"]).stdout.contains("Already installed"));
    // A profile in copy mode gets the new settings.json at once.
    let copy = json_of(&profile_dir(paths, "acct2").join("settings.json"));
    assert!(
        copy["statusLine"]["command"]
            .as_str()
            .unwrap()
            .contains("statusline")
    );

    let removed = baton(&home, &["uninstall"]);
    assert_eq!(removed.code, 0, "{}", removed.stderr);
    assert!(removed.stdout.contains("Uninstalled"), "{}", removed.stdout);
    assert_eq!(json_of(&settings), json!({ "theme": "dark" }));
    assert!(baton(&home, &["uninstall"]).stdout.contains("Not installed"));
}

#[test]
fn install_as_data_names_the_files_and_whether_anything_changed() {
    let home = temp_home();
    let paths = &home.paths;
    let dry = baton(&home, &["install", "--dry-run", "--json"]).json();
    assert_eq!(dry["dryRun"], true);
    assert!(dry.get("changed").is_none(), "a dry run applies nothing");
    let files: Vec<&str> = dry["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(files.len(), 2);
    assert!(
        files[0].ends_with("settings.json") && files[1].ends_with("hop.md"),
        "{files:?}"
    );
    assert!(
        dry["files"][0]["diff"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("statusline"))
    );
    let done = baton(&home, &["install", "--json"]).json();
    assert_eq!(
        (&done["dryRun"], &done["changed"], &done["backup"]),
        (&json!(false), &json!(true), &json!(null))
    );
    let again = baton(&home, &["install", "--json"]).json();
    assert_eq!(again["changed"], false);
    assert_eq!(again["files"], json!([]));
    assert!(paths.install.exists());
}
