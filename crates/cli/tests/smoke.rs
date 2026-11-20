//! The command line end to end: what is typed, what is printed, the exit code.

mod common;

use baton_testkit::temp_home;
use baton_testkit::test_env;
use common::{baton, baton_io};

#[test]
fn version_and_unknown_commands() {
    let home = temp_home();
    let version = baton(&home, &["version"]);
    assert_eq!(
        (version.code, version.stdout.as_str()),
        (0, concat!(env!("CARGO_PKG_VERSION"), "\n"))
    );
    assert_eq!(baton(&home, &["--version"]).stdout, version.stdout);
    assert_eq!(
        baton(&home, &["version", "--json"]).json()["version"],
        env!("CARGO_PKG_VERSION")
    );
    let unknown = baton(&home, &["nonsense"]);
    assert_eq!(
        (unknown.code, unknown.stderr.as_str()),
        (2, "baton: unknown command nonsense. Try baton help.\n")
    );
    let option = baton(&home, &["version", "--nope"]);
    assert_eq!(option.code, 2);
    assert!(
        option.stderr.starts_with("baton: Unknown option '--nope'."),
        "{}",
        option.stderr
    );
}

#[test]
fn a_terminal_gets_the_screen_and_a_pipe_gets_plain_text() {
    let home = temp_home();
    let piped = baton(&home, &["account", "list"]);
    assert_eq!(piped.code, 0);
    assert!(
        piped.stdout.starts_with("  ○ No accounts yet.") || piped.stdout.starts_with("  o No accounts yet.")
    );
    assert!(!piped.stdout.contains('\x1b'));
    let shown = baton_io(test_env(&home.home), &["account", "list"], |io| {
        io.out.tty = true;
        io.out.columns = Some(100);
        io.env.set("FORCE_COLOR", "3");
    });
    assert!(shown.stdout.contains("\x1b["), "{}", shown.stdout);
    assert!(shown.stdout.contains("BATON"), "{}", shown.stdout);
}

#[test]
fn help_lists_the_commands_and_each_commands_usage() {
    let home = temp_home();
    let help = baton(&home, &["help"]);
    assert_eq!(help.code, 0);
    for name in [
        "start",
        "hop",
        "account",
        "login",
        "status",
        "stats",
        "policy",
        "install",
        "uninstall",
        "doctor",
        "view",
    ] {
        assert!(
            help.stdout.contains(&format!("\n  {name} ")),
            "{name} is listed:\n{}",
            help.stdout
        );
    }
    // The other names for start and view are not listed.
    for name in ["claude", "menu"] {
        assert!(
            !help.stdout.contains(&format!("\n  {name} ")),
            "{name} is not listed"
        );
    }
    assert!(
        help.stdout
            .contains("baton starts Claude Code on the best account; baton view opens the menu"),
        "{}",
        help.stdout
    );
    let start = baton(&home, &["help", "start"]).stdout;
    let lines: Vec<&str> = start.lines().map(str::trim).collect();
    assert!(lines.contains(&"baton [claude arguments...]"), "{start}");
    assert!(lines.contains(&"baton start [claude arguments...]"), "{start}");
    assert!(
        baton(&home, &["help", "menu"])
            .stdout
            .contains("Same as baton view")
    );
    assert!(
        baton(&home, &["help", "account"])
            .stdout
            .contains("baton account add <name> [--mark m]")
    );
    assert!(
        baton(&home, &["status", "--help"])
            .stdout
            .contains("baton status [--json]")
    );
    assert_eq!(baton(&home, &["nonsense"]).code, 2);
    assert_eq!(baton(&home, &["status", "--bogus"]).code, 2);
}
