//! Claude Code's command line: which conversation it opens, what a resume passes again, and how the
//! `claude` program is found without a shell.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions fail loudly on purpose"
)]

use baton_claude::args::{Live, SessionChoice, SessionMode, parse_claude_args, resume_args};

const SID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";

fn args(tokens: &[&str]) -> Vec<String> {
    tokens.iter().map(|token| (*token).to_owned()).collect()
}

fn session(tokens: &[&str]) -> SessionChoice {
    parse_claude_args(&args(tokens)).session
}

fn choice(mode: SessionMode, id: Option<&str>) -> SessionChoice {
    SessionChoice {
        mode,
        id: id.map(str::to_owned),
    }
}

#[test]
fn which_conversation_a_command_line_opens_and_whether_it_is_an_interactive_session() {
    assert_eq!(session(&[]), choice(SessionMode::New, None));
    assert_eq!(session(&["-r", SID]), choice(SessionMode::Resume, Some(SID)));
    assert_eq!(
        session(&[&format!("--resume={SID}")]),
        choice(SessionMode::Resume, Some(SID))
    );
    assert_eq!(session(&["--resume"]), choice(SessionMode::Picker, None));
    assert_eq!(
        session(&["-r", "--model", "opus"]),
        choice(SessionMode::Picker, None)
    );
    assert_eq!(session(&["-c"]), choice(SessionMode::Continue, None));
    assert_eq!(
        session(&["--session-id", SID]),
        choice(SessionMode::New, Some(SID))
    );
    assert_eq!(session(&["-r", SID, "--fork-session"]).mode, SessionMode::Fork);
    assert!(parse_claude_args(&args(&["fix the bug"])).interactive);
    let passthroughs: [&[&str]; 7] = [
        &["-p", "hi"],
        &["--print"],
        &["--version"],
        &["auth", "status"],
        &["mcp", "list"],
        &["--bg"],
        &["-h"],
    ];
    for passthrough in passthroughs {
        assert!(
            !parse_claude_args(&args(passthrough)).interactive,
            "{}",
            passthrough.join(" ")
        );
    }
    let parsed = parse_claude_args(&args(&["--model", "opus", "--add-dir", "a", "b", "do it"]));
    assert_eq!(
        parsed
            .options
            .iter()
            .find(|o| o.name == "--add-dir")
            .unwrap()
            .values,
        ["a", "b", "do it"],
        "commander reads a prompt after --add-dir as a directory too"
    );
    assert_eq!(
        parse_claude_args(&args(&["--model", "opus", "do it"])).positionals,
        ["do it"]
    );
}

#[test]
fn resume_keeps_what_resume_does_not_restore_and_drops_the_session_selection_and_the_prompt() {
    let parsed = parse_claude_args(&args(&[
        "--model",
        "opus",
        "--add-dir",
        "../shared",
        "../more",
        "--mcp-config",
        "mcp.json",
        "--settings",
        "s.json",
        "--plugin-dir",
        "plugins",
        "--fallback-model",
        "sonnet",
        "--permission-mode",
        "acceptEdits",
        "-n",
        "work",
        "--continue",
        "first prompt",
    ]));
    let resumed = resume_args(&parsed, SID, Some("carry on"), &Live::default());
    assert_eq!(
        resumed,
        [
            "--model",
            "opus",
            "--add-dir",
            "../shared",
            "../more",
            "--mcp-config",
            "mcp.json",
            "--settings",
            "s.json",
            "--plugin-dir",
            "plugins",
            "--fallback-model",
            "sonnet",
            "--permission-mode",
            "acceptEdits",
            "--resume",
            SID,
            "carry on",
        ]
    );
    // --resume ends the variadic --add-dir, so the prompt is the prompt.
    let reparsed = parse_claude_args(&resumed);
    assert_eq!(reparsed.positionals, ["carry on"]);
    assert_eq!(reparsed.session, choice(SessionMode::Resume, Some(SID)));
    assert_eq!(
        resume_args(
            &parse_claude_args(&args(&["-r", SID, "--worktree", "w1", "--tmux"])),
            SID,
            None,
            &Live::default()
        ),
        ["--resume", SID]
    );
}

#[test]
fn resume_takes_the_sessions_live_model_effort_and_permission_mode() {
    let live = |model: Option<&str>, effort: Option<&str>, mode: Option<&str>| Live {
        model: model.map(str::to_owned),
        effort: effort.map(str::to_owned),
        permission_mode: mode.map(str::to_owned),
    };
    let parsed = parse_claude_args(&args(&["--model", "opus", "--permission-mode", "plan"]));
    assert_eq!(
        resume_args(
            &parsed,
            SID,
            None,
            &live(Some("claude-sonnet-5"), Some("high"), Some("acceptEdits"))
        ),
        [
            "--model",
            "claude-sonnet-5",
            "--effort",
            "high",
            "--permission-mode",
            "acceptEdits",
            "--resume",
            SID
        ]
    );
    // Without --model on the original line, the resumed session keeps its own model.
    assert_eq!(
        resume_args(
            &parse_claude_args(&[]),
            SID,
            None,
            &live(Some("claude-sonnet-5"), None, None)
        ),
        ["--resume", SID]
    );
    // "default" is the CLI's "manual".
    assert_eq!(
        resume_args(
            &parse_claude_args(&[]),
            SID,
            None,
            &live(None, None, Some("default"))
        ),
        ["--permission-mode", "manual", "--resume", SID]
    );
    // Bypass mode is not restored by a terminal resume: passed again.
    assert_eq!(
        resume_args(
            &parse_claude_args(&args(&["--allow-dangerously-skip-permissions"])),
            SID,
            None,
            &live(None, None, Some("bypassPermissions"))
        ),
        [
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "bypassPermissions",
            "--resume",
            SID
        ]
    );
    assert_eq!(
        resume_args(
            &parse_claude_args(&args(&["--dangerously-skip-permissions"])),
            SID,
            None,
            &live(None, None, Some("bypassPermissions"))
        ),
        ["--dangerously-skip-permissions", "--resume", SID]
    );
    // Left bypass mode during the session: bypass stays available, not forced.
    assert_eq!(
        resume_args(
            &parse_claude_args(&args(&["--dangerously-skip-permissions"])),
            SID,
            None,
            &live(None, None, Some("acceptEdits"))
        ),
        [
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "acceptEdits",
            "--resume",
            SID
        ]
    );
}

#[cfg(windows)]
#[test]
fn windows_resolution_pathext_order_per_folder_explicit_paths_and_npm_shims_run_without_a_shell() {
    use std::fs;
    use std::path::PathBuf;

    use baton_core::claude_path::{Launcher, find_on_path, resolve_claude, shim_target};
    use baton_core::env::Env;

    let home = baton_testkit::temp_home();
    let root = home.home.parent().unwrap().to_path_buf();
    let a = root.join("a dir");
    let b = root.join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("claude.cmd"), "@echo off\r\n").unwrap();
    fs::write(b.join("claude.exe"), "").unwrap();
    let text = |path: &PathBuf| path.to_string_lossy().into_owned();
    let env = Env::from_pairs([
        ("PATH", format!("\"{}\";{}", text(&a), text(&b))),
        ("PATHEXT", ".COM;.EXE;.BAT;.CMD".to_owned()),
        ("BATON_NODE", "NODE".to_owned()),
    ]);
    assert_eq!(find_on_path("claude", &env), Some(a.join("claude.cmd")));
    // The shim in the first folder cannot be read for a target: resolution fails clearly.
    let error = resolve_claude(None, &env, None).unwrap_err();
    assert!(error.0.contains("shell shim"), "{error}");
    let pkg = a.join("node_modules").join("@anthropic-ai").join("claude-code");
    fs::create_dir_all(&pkg).unwrap();
    fs::write(pkg.join("cli.js"), "").unwrap();
    fs::write(
        a.join("claude.cmd"),
        "@ECHO off\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n)\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n",
    )
    .unwrap();
    assert_eq!(shim_target(&a.join("claude.cmd")), Some(pkg.join("cli.js")));
    assert_eq!(
        resolve_claude(None, &env, None).unwrap(),
        Launcher {
            command: PathBuf::from("NODE"),
            prefix: vec![text(&pkg.join("cli.js"))],
            path: pkg.join("cli.js")
        }
    );
    let only_b = Env::from_pairs([("PATH", text(&b))]);
    assert_eq!(
        resolve_claude(None, &only_b, None).unwrap(),
        Launcher::program(b.join("claude.exe"))
    );
    assert_eq!(
        resolve_claude(Some(&text(&b.join("claude.exe"))), &Env::new(), None)
            .unwrap()
            .command,
        b.join("claude.exe")
    );
    let other = root.join("other-home");
    fs::create_dir_all(other.join(".local").join("bin")).unwrap();
    fs::write(other.join(".local").join("bin").join("claude.exe"), "").unwrap();
    let no_path = Env::from_pairs([("PATH", "")]);
    assert_eq!(
        resolve_claude(None, &no_path, Some(&other)).unwrap().command,
        other.join(".local").join("bin").join("claude.exe")
    );
    assert!(
        resolve_claude(None, &no_path, None)
            .unwrap_err()
            .0
            .contains("not on PATH")
    );
}
