//! What a command prints in a terminal and through a pipe: the menu's look where the terminal can
//! show it, the same words as plain text where it cannot, and data that is never styled.

mod common;
mod support;

use std::path::PathBuf;

use baton_core::text::clean;
use baton_testkit::temp_home;
use common::baton_io;
use serde_json::json;
use support::{EXE, baton, env_of, squash};

#[test]
fn in_a_terminal_install_shows_the_mark_the_pieces_and_the_next_step_the_same_words_piped_without_escapes() {
    let home = temp_home();
    // A terminal of 24-bit colour, 100 columns wide.
    let tty = baton_io(
        env_of(&home).with("FORCE_COLOR", "3"),
        &["install", "--dry-run"],
        |io| {
            io.exe = PathBuf::from(EXE);
            io.find_ferry = Some(Box::new(|| None));
            io.out.tty = true;
            io.out.columns = Some(100);
        },
    );
    assert_eq!(tty.code, 0, "{}", tty.stderr);
    let shown = clean(&tty.stdout);
    assert!(shown.contains("/_____/_/  |_/_/"), "the wordmark: {shown}");
    assert!(shown.contains("Keep the conversation moving."), "{shown}");
    let words = squash(&shown);
    assert!(words.contains("INSTALL \u{2500} dry run"), "{shown}");
    assert!(
        words.contains("\u{203a} Status line will add baton statusline"),
        "{shown}"
    );
    assert!(
        words.contains("\u{203a} Hooks will add SessionStart \u{b7} UserPromptSubmit"),
        "{shown}"
    );
    assert!(
        words.contains("\u{203a} /hop command will add ~/.claude/commands/hop.md"),
        "{shown}"
    );
    assert!(
        words.contains("\u{25cb} Ferry not found; run baton install again after installing it"),
        "{shown}"
    );
    assert!(tty.stdout.contains("\x1b["), "styled: {:?}", tty.stdout);

    let pipe = baton(&home, &["install", "--dry-run"]);
    assert_eq!(pipe.code, 0, "{}", pipe.stderr);
    assert!(!pipe.stdout.contains('\x1b'), "{:?}", pipe.stdout);
    assert!(
        !pipe.stdout.contains("Keep the conversation moving"),
        "no wordmark in a pipe"
    );
    assert!(
        squash(&pipe.stdout).contains("INSTALL \u{2500} dry run"),
        "{}",
        pipe.stdout
    );
    // The same pieces, in the same words.
    let piped = squash(&pipe.stdout);
    for words in [
        "\u{203a} Status line will add baton statusline",
        "\u{203a} Hooks will add SessionStart \u{b7} UserPromptSubmit",
        "\u{203a} /hop command will add ~/.claude/commands/hop.md",
        "\u{25cb} Ferry not found; run baton install again after installing it",
    ] {
        assert!(piped.contains(words), "{words}: {}", pipe.stdout);
    }
}

#[test]
fn in_a_terminal_a_failure_goes_to_stderr_with_a_cross_and_exit_code_2() {
    let home = temp_home();
    let run = baton_io(
        env_of(&home).with("FORCE_COLOR", "3"),
        &["account", "remove", "nobody"],
        |io| {
            io.err.tty = true;
            io.err.columns = Some(100);
        },
    );
    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains("\x1b["), "styled: {:?}", run.stderr);
    assert!(
        clean(&run.stderr).starts_with("\u{2717} No account named nobody"),
        "{:?}",
        run.stderr
    );
    // Through a pipe: baton: and the same words.
    let piped = baton(&home, &["account", "remove", "nobody"]);
    assert_eq!(piped.code, 2);
    assert!(
        piped.stderr.starts_with("baton: No account named nobody"),
        "{:?}",
        piped.stderr
    );
}

#[test]
fn json_is_data_never_styled_even_in_a_terminal() {
    let home = temp_home();
    let run = baton_io(
        env_of(&home).with("FORCE_COLOR", "3"),
        &["account", "list", "--json"],
        |io| {
            io.out.tty = true;
            io.out.columns = Some(100);
        },
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(!run.stdout.contains('\x1b'), "{:?}", run.stdout);
    assert_eq!(run.json(), json!([]));
}
