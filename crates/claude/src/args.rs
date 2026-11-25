//! Reading and rebuilding Claude Code's command line (2.1.302's `claude --help`).
//!
//! Baton needs to know which conversation a launch opens, whether the launch is an interactive session
//! at all, and what to pass again when it resumes the conversation under another account: `claude
//! --resume <id>` restores the conversation, its model and (from a terminal) its permission mode, but
//! not --mcp-config, --settings, --plugin-dir, --fallback-model or --add-dir, so everything but the
//! session selection and the first prompt is passed again, with the session's live model, effort and
//! permission mode where Baton saw them change.

/// Options that take no value.
const BOOLEAN: [&str; 32] = [
    "--allow-dangerously-skip-permissions",
    "--ax-screen-reader",
    "--bg",
    "--background",
    "--bare",
    "--brief",
    "--chrome",
    "-c",
    "--continue",
    "--dangerously-skip-permissions",
    "--desktop",
    "--disable-slash-commands",
    "--exclude-dynamic-system-prompt-sections",
    "--fork-session",
    "--forward-subagent-text",
    "-h",
    "--help",
    "--ide",
    "--include-hook-events",
    "--include-partial-messages",
    "--no-chrome",
    "--no-session-persistence",
    "-p",
    "--print",
    "--replay-user-messages",
    "--restricted",
    "--safe-mode",
    "--strict-mcp-config",
    "--tmux",
    "--verbose",
    "-v",
    "--version",
];

/// Options that take one value.
const VALUE: [&str; 29] = [
    "--agent",
    "--agents",
    "--append-system-prompt",
    "--append-system-prompt-file",
    "--autocompact",
    "--debug-file",
    "--effort",
    "--environment",
    "--fallback-model",
    "--input-format",
    "--json-schema",
    "--max-budget-usd",
    "--max-turns",
    "--model",
    "-n",
    "--name",
    "--output-format",
    "--permission-mode",
    "--permission-prompt-tool",
    "--permission-prompts",
    "--plugin-dir",
    "--plugin-url",
    "--remote-control-session-name-prefix",
    "--session-id",
    "--setting-sources",
    "--settings",
    "--system-prompt",
    "--system-prompt-file",
    "--system-prompt-snapshot",
];

/// Options that may take one value.
const OPTIONAL: [&str; 11] = [
    "--cloud",
    "-d",
    "--debug",
    "--from-pr",
    "--prompt-suggestions",
    "--remote-control",
    "-r",
    "--resume",
    "--teleport",
    "-w",
    "--worktree",
];

/// Options that take every value up to the next option.
const VARIADIC: [&str; 9] = [
    "--add-dir",
    "--allowedTools",
    "--allowed-tools",
    "--betas",
    "--disallowedTools",
    "--disallowed-tools",
    "--file",
    "--mcp-config",
    "--tools",
];

/// Claude Code's own commands: a launch that starts with one is not a session.
const SUBCOMMANDS: [&str; 22] = [
    "agents",
    "attach",
    "auth",
    "auto-mode",
    "doctor",
    "gateway",
    "help",
    "import",
    "install",
    "kill",
    "logs",
    "mcp",
    "plugin",
    "plugins",
    "purge",
    "respawn",
    "rm",
    "setup-token",
    "stop",
    "ultrareview",
    "update",
    "upgrade",
];

/// Options that pick or create the conversation, or only apply to the first launch: not passed again
/// on a resume.
const DROP_ON_RESUME: [&str; 9] = [
    "--continue",
    "--resume",
    "--session-id",
    "--fork-session",
    "--from-pr",
    "--worktree",
    "--tmux",
    "--name",
    "--teleport",
];

/// Options that mean the launch is not an interactive session.
const PASSTHROUGH: [&str; 8] = [
    "--print",
    "--help",
    "--version",
    "--bg",
    "--desktop",
    "--cloud",
    "--teleport",
    "--from-pr",
];

/// The name an option is known by, whichever way it was written.
fn canonical(flag: &str) -> &str {
    match flag {
        "-c" => "--continue",
        "-d" => "--debug",
        "-h" => "--help",
        "-n" => "--name",
        "-p" => "--print",
        "-r" => "--resume",
        "-v" => "--version",
        "-w" => "--worktree",
        "--background" => "--bg",
        "--allowed-tools" => "--allowedTools",
        "--disallowed-tools" => "--disallowedTools",
        other => other,
    }
}

/// How many values an option takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// None.
    Boolean,
    /// Exactly one.
    Value,
    /// One when the next token is not an option.
    Optional,
    /// Every token up to the next option.
    Variadic,
    /// Not an option Baton knows (a Claude Code newer than its tables): the token after it, when it
    /// is no option, is taken for its value, also when it is the last one.
    Unknown,
}

/// The kind of an option as it was written.
fn kind(flag: &str) -> Kind {
    if BOOLEAN.contains(&flag) {
        Kind::Boolean
    } else if VALUE.contains(&flag) {
        Kind::Value
    } else if OPTIONAL.contains(&flag) {
        Kind::Optional
    } else if VARIADIC.contains(&flag) {
        Kind::Variadic
    } else {
        Kind::Unknown
    }
}

/// Whether a token is an option (a lone `-` is not).
fn is_flag(token: &str) -> bool {
    token.starts_with('-') && token != "-"
}

/// One option of a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeOption {
    /// The option as it was written, without a `=value`.
    pub flag: String,
    /// The name it is known by (`-r` is `--resume`).
    pub name: String,
    /// Its values.
    pub values: Vec<String>,
    /// The tokens it took from the command line, to pass again as they were.
    pub raw: Vec<String>,
}

/// How a launch picks its conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionMode {
    /// A new conversation.
    New,
    /// A conversation named by its id.
    Resume,
    /// The latest conversation of the folder.
    Continue,
    /// Claude Code's own picker.
    Picker,
    /// A copy of an existing conversation.
    Fork,
}

impl SessionMode {
    /// The mode in a word: `new`, `resume`, `continue`, `picker`, `fork`.
    pub fn as_str(self) -> &'static str {
        match self {
            SessionMode::New => "new",
            SessionMode::Resume => "resume",
            SessionMode::Continue => "continue",
            SessionMode::Picker => "picker",
            SessionMode::Fork => "fork",
        }
    }
}

/// The conversation a launch opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionChoice {
    /// How it is picked.
    pub mode: SessionMode,
    /// Its id, when the command line names one.
    pub id: Option<String>,
}

/// A command line of Claude Code, read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedArgs {
    /// The options, in order.
    pub options: Vec<ClaudeOption>,
    /// What is not an option: the prompt, or a subcommand's own arguments.
    pub positionals: Vec<String>,
    /// Claude Code's own command, when the launch runs one.
    pub subcommand: Option<String>,
    /// The conversation the launch opens.
    pub session: SessionChoice,
    /// Whether the launch is an interactive session (what Baton wraps).
    pub interactive: bool,
    /// The options Baton does not know that were read with the token after them as their value: a
    /// guess, kept on a resume and worth a line in the log.
    pub guessed: Vec<String>,
}

impl ParsedArgs {
    /// Whether the option known as `name` is on the command line.
    pub fn has(&self, name: &str) -> bool {
        self.options.iter().any(|o| o.name == name)
    }

    /// The first value of the last option known as `name`.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|o| o.name == name)
            .and_then(|o| o.values.first())
            .map(String::as_str)
    }
}

/// Reads a command line of Claude Code.
pub fn parse_claude_args(args: &[String]) -> ParsedArgs {
    let mut options: Vec<ClaudeOption> = Vec::new();
    let mut positionals: Vec<String> = Vec::new();
    let mut subcommand = None;
    let mut guessed = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        if token == "--" {
            positionals.extend(args[i + 1..].iter().cloned());
            break;
        }
        if !is_flag(token) {
            if options.is_empty() && positionals.is_empty() && SUBCOMMANDS.contains(&token.as_str()) {
                subcommand = Some(token.clone());
                positionals.extend(args[i + 1..].iter().cloned());
                break;
            }
            positionals.push(token.clone());
            i += 1;
            continue;
        }
        let eq = if token.starts_with("--") {
            token.find('=').filter(|at| *at > 0)
        } else {
            None
        };
        let flag = match eq {
            Some(at) => &token[..at],
            None => token.as_str(),
        };
        let mut raw = vec![token.clone()];
        let mut values = Vec::new();
        let takes = kind(flag);
        if let Some(at) = eq {
            values.push(token[at + 1..].to_owned());
        } else if takes == Kind::Value {
            if i + 1 < args.len() {
                i += 1;
                values.push(args[i].clone());
                raw.push(args[i].clone());
            }
        } else if takes == Kind::Optional {
            if i + 1 < args.len() && !is_flag(&args[i + 1]) {
                i += 1;
                values.push(args[i].clone());
                raw.push(args[i].clone());
            }
        } else if takes == Kind::Variadic {
            while i + 1 < args.len() && !is_flag(&args[i + 1]) {
                i += 1;
                values.push(args[i].clone());
                raw.push(args[i].clone());
            }
        } else if takes == Kind::Unknown && i + 1 < args.len() && !is_flag(&args[i + 1]) {
            // The token goes with the option, also on a resume, where an option left without its
            // value would take the next token for it. Write `--flag=value`, or the prompt before the
            // option, to tell them apart.
            i += 1;
            values.push(args[i].clone());
            raw.push(args[i].clone());
            guessed.push(flag.to_owned());
        }
        options.push(ClaudeOption {
            flag: flag.to_owned(),
            name: canonical(flag).to_owned(),
            values,
            raw,
        });
        i += 1;
    }
    let mut parsed = ParsedArgs {
        options,
        positionals,
        subcommand,
        session: SessionChoice {
            mode: SessionMode::New,
            id: None,
        },
        interactive: true,
        guessed,
    };
    parsed.session.id = parsed.value("--session-id").map(str::to_owned);
    if parsed.has("--resume") {
        parsed.session = match parsed.value("--resume").filter(|target| !target.is_empty()) {
            Some(target) => SessionChoice {
                mode: SessionMode::Resume,
                id: Some(target.to_owned()),
            },
            None => SessionChoice {
                mode: SessionMode::Picker,
                id: None,
            },
        };
    } else if parsed.has("--continue") {
        parsed.session = SessionChoice {
            mode: SessionMode::Continue,
            id: None,
        };
    }
    if parsed.has("--fork-session") && parsed.session.mode != SessionMode::New {
        parsed.session = SessionChoice {
            mode: SessionMode::Fork,
            id: None,
        };
    }
    parsed.interactive = parsed.subcommand.is_none() && !PASSTHROUGH.iter().any(|name| parsed.has(name));
    parsed
}

/// Whether `id` is a conversation id as Claude Code writes them (a UUID).
pub fn is_session_id(id: &str) -> bool {
    let groups: Vec<&str> = id.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// What Baton last saw of a session, to resume it as it was.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Live {
    /// Its model.
    pub model: Option<String>,
    /// Its effort.
    pub effort: Option<String>,
    /// Its permission mode.
    pub permission_mode: Option<String>,
}

/// The command line that resumes `session_id`, keeping the original options.
///
/// - `--model` / `--effort` keep their flag but take the live value;
/// - a live permission mode replaces `--permission-mode`; bypassPermissions (which a terminal resume
///   does not restore) is passed again explicitly, and `--dangerously-skip-permissions` is turned into
///   `--allow-dangerously-skip-permissions` when the session left bypass mode.
pub fn resume_args(parsed: &ParsedArgs, session_id: &str, prompt: Option<&str>, live: &Live) -> Vec<String> {
    let set = |value: &Option<String>| value.clone().filter(|v| !v.is_empty());
    let model = set(&live.model);
    let effort = set(&live.effort);
    let mode = set(&live.permission_mode).map(|mode| {
        if mode == "default" {
            "manual".to_owned()
        } else {
            mode
        }
    });
    let mut out: Vec<String> = Vec::new();
    let mut allow_bypass = false;
    for option in &parsed.options {
        if DROP_ON_RESUME.contains(&option.name.as_str()) {
            continue;
        }
        if let (true, Some(model)) = (option.name == "--model", &model) {
            out.push("--model".to_owned());
            out.push(model.clone());
            continue;
        }
        if let (true, Some(effort)) = (option.name == "--effort", &effort) {
            out.push("--effort".to_owned());
            out.push(effort.clone());
            continue;
        }
        if let Some(mode) = &mode {
            if option.name == "--permission-mode" {
                continue;
            }
            if option.name == "--dangerously-skip-permissions" {
                if mode != "bypassPermissions" {
                    allow_bypass = true;
                } else {
                    out.extend(option.raw.iter().cloned());
                }
                continue;
            }
        }
        if option.name == "--allow-dangerously-skip-permissions" {
            allow_bypass = false;
        }
        out.extend(option.raw.iter().cloned());
    }
    let has = |out: &[String], flag: &str| out.iter().any(|token| token == flag);
    if allow_bypass && !has(&out, "--allow-dangerously-skip-permissions") {
        out.push("--allow-dangerously-skip-permissions".to_owned());
    }
    if let Some(effort) = effort.filter(|_| !parsed.has("--effort")) {
        out.push("--effort".to_owned());
        out.push(effort);
    }
    if let Some(mode) = mode {
        let bypass_forced = has(&out, "--dangerously-skip-permissions");
        if !(mode == "bypassPermissions" && bypass_forced) {
            out.push("--permission-mode".to_owned());
            out.push(mode);
        }
    }
    // --resume last before the prompt: it ends a preceding variadic option (--add-dir a b) and the
    // prompt then reads as the positional prompt.
    out.push("--resume".to_owned());
    out.push(session_id.to_owned());
    if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
        out.push(prompt.to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| (*t).to_owned()).collect()
    }

    #[test]
    fn session_ids_are_uuids() {
        assert!(is_session_id("6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f"));
        assert!(is_session_id("6F0B2D4E-1C3A-4B5D-8E9F-0A1B2C3D4E5F"));
        assert!(!is_session_id("6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5"));
        assert!(!is_session_id("6f0b2d4e1c3a4b5d8e9f0a1b2c3d4e5f"));
        assert!(!is_session_id("zf0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f"));
        assert!(!is_session_id(""));
    }

    #[test]
    fn options_keep_what_they_took() {
        let parsed = parse_claude_args(&args(&["--model=opus", "-d", "api", "--weird", "--", "x", "-p"]));
        assert_eq!(
            parsed.options[0],
            ClaudeOption {
                flag: "--model".into(),
                name: "--model".into(),
                values: vec!["opus".into()],
                raw: vec!["--model=opus".into()]
            }
        );
        assert_eq!(parsed.options[1].name, "--debug");
        assert_eq!(parsed.options[1].raw, ["-d", "api"]);
        assert!(
            parsed.options[2].values.is_empty(),
            "an option Baton does not know takes nothing"
        );
        assert_eq!(parsed.positionals, ["x", "-p"]);
        assert!(parsed.interactive, "after -- nothing is an option");
        assert_eq!(SessionMode::Picker.as_str(), "picker");
    }

    #[test]
    fn an_option_baton_does_not_know_keeps_the_token_after_it() {
        let live = Live::default();
        // More follows the token: it is the option's value, and stays with it on a resume.
        let parsed = parse_claude_args(&args(&["--newer", "value", "--model", "opus", "do this"]));
        assert_eq!(parsed.guessed, ["--newer"]);
        assert_eq!(parsed.options[0].raw, ["--newer", "value"]);
        assert_eq!(parsed.positionals, ["do this"]);
        assert_eq!(
            resume_args(&parsed, "sid", Some("note"), &live),
            ["--newer", "value", "--model", "opus", "--resume", "sid", "note"]
        );
        // Also when it is the last token.
        let parsed = parse_claude_args(&args(&["--newer", "value"]));
        assert_eq!(parsed.guessed, ["--newer"]);
        assert!(parsed.positionals.is_empty());
        assert_eq!(
            resume_args(&parsed, "sid", Some("note"), &live),
            ["--newer", "value", "--resume", "sid", "note"]
        );
        // An option after it takes nothing from it.
        let parsed = parse_claude_args(&args(&["--newer", "--model", "opus"]));
        assert!(parsed.guessed.is_empty());
        assert_eq!(parsed.options[0].raw, ["--newer"]);
        // Written with `=` there is nothing to guess.
        let parsed = parse_claude_args(&args(&["--newer=value", "do this"]));
        assert!(parsed.guessed.is_empty());
        assert_eq!(parsed.options[0].values, ["value"]);
    }
}
