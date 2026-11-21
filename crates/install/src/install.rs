//! `baton install` / `baton uninstall`: the only changes Baton makes to `~/.claude` (shared by every
//! profile):
//!
//! - settings.json: a status line command (`baton statusline`, which records rate_limits and then runs
//!   the operator's previous status line command if there was one), and command hooks (exec form: the
//!   baton executable itself, no shell) for SessionStart, UserPromptSubmit, Stop, StopFailure and
//!   SessionEnd;
//! - commands/hop.md: the /hop command;
//!
//! and, when Ferry is on this machine, its launcher file for Baton (`ferry`), outside `~/.claude`.
//!
//! Every change is planned first (the plan is what `--dry-run` shows), the previous files are backed up
//! under `~/.baton/backups/<time>/`, writes are atomic, and running it again changes nothing. Uninstall
//! removes exactly Baton's entries and puts the previous status line back. A file that cannot be
//! written stops with an [`InstallError`], except Ferry's launcher, which lives in another tool's
//! folder: its failure is a note.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use baton_core::claude_path::find_on_path;
use baton_core::clock::{Ms, iso};
use baton_core::env::Env;
use baton_core::fsutil::{Modes, read_json, read_text, remove_file, write_file_atomic, write_json_atomic};
use baton_core::json::{Get, Object, compact};
use baton_core::paths::{Paths, forward, same_path};
use regex::Regex;
use serde_json::{Value, json};
use thiserror::Error;

use crate::diff::diff_lines;
use crate::ferry::{
    FerryAccess, LAUNCHER_ID, PRIVATE, RunFerry, detect_ferry, ferry_agent, ferry_home, is_baton_launcher,
    launcher_path, launcher_text, parse_launcher, set_ferry_agent, takes_over,
};
use crate::hooks::{HOOK_EVENTS, hook_event};

/// What says a /hop command file is the one Baton installed.
pub const HOP_MARKER: &str = "(installed by Baton)";

/// The unchanged lines shown around a change in a plan's diff.
const CONTEXT: usize = 2;

/// `~/.claude/settings.json`.
pub fn settings_path(paths: &Paths) -> PathBuf {
    paths.claude_dir.join("settings.json")
}

/// `~/.claude/commands/hop.md`.
pub fn hop_command_path(paths: &Paths) -> PathBuf {
    paths.claude_dir.join("commands").join("hop.md")
}

/// The events Claude Code names, in the order Baton installs their hooks.
fn hook_names() -> Vec<&'static str> {
    HOOK_EVENTS.iter().map(|(_, event)| *event).collect()
}

/// Baton's status line command: the baton executable, then `statusline`.
static OWN_STATUS_LINE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:^|[\\/"\s])baton(?:-[^\\/"\s]*)?(?:\.exe)?"?\s+statusline\b"#).ok()
});

/// Where a settings.json line's indentation starts.
static INDENT: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new("\n([ \t]+)\"").ok());

/// Whether a program is the baton executable, by its file name: `baton`, `baton.exe`, or either with
/// a suffix after a dash (`baton-0.1.0`).
fn is_baton_program(program: &str) -> bool {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    name == "baton" || name.starts_with("baton-")
}

/// Whether a hook handler in settings.json is Baton's: `<baton> hook <event>`.
pub fn is_baton_handler(handler: &Value) -> bool {
    let Some(args) = handler.at("args").as_array() else {
        return false;
    };
    handler.text("type") == Some("command")
        && args.len() >= 2
        && args[0].as_str() == Some("hook")
        && args[1].as_str().is_some_and(|event| hook_event(event).is_some())
        && handler.text("command").is_some_and(is_baton_program)
}

/// Whether a status line in settings.json is Baton's.
pub fn is_baton_status_line(status_line: &Value) -> bool {
    status_line.text("command").is_some_and(|command| {
        OWN_STATUS_LINE
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(command))
    })
}

/// The baton executable at the start of a command line both Git Bash and PowerShell run. A path
/// cannot be quoted there (PowerShell reads a quoted word as text, not as a program), so one with
/// spaces, or with anything else a shell reads as its own, is replaced by `baton` from PATH.
pub fn baton_command(exe: &Path) -> String {
    let program = forward(exe);
    let special = |c: char| c.is_whitespace() || "\"'`$&|;<>()[]{}*?!#^,".contains(c);
    if program.is_empty() || program.chars().any(special) {
        "baton".to_owned()
    } else {
        program
    }
}

/// Whether the status line and the /hop command run `baton` from PATH, because the executable's own
/// path cannot start a command line every shell runs.
pub fn runs_from_path(exe: &Path) -> bool {
    baton_command(exe) != forward(exe)
}

/// Whether two paths are the same file: by name, or once the links in them are followed.
fn same_file(a: &Path, b: &Path) -> bool {
    same_path(a, b) || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// The status line command Baton installs.
pub fn status_line_command(exe: &Path) -> String {
    format!("{} statusline", baton_command(exe))
}

/// The handler Baton installs for the hook `event` (the name `baton hook` takes).
pub fn hook_handler(exe: &Path, event: &str) -> Value {
    json!({ "type": "command", "command": forward(exe), "args": ["hook", event], "timeout": 10 })
}

/// The /hop command file Baton installs.
pub fn hop_command_text(exe: &Path) -> String {
    let baton = baton_command(exe);
    [
        "---".to_owned(),
        format!("description: Move this conversation to another account at the end of this turn {HOP_MARKER}"),
        "argument-hint: \"[account]\"".to_owned(),
        format!("allowed-tools: Bash({baton}:*)"),
        "disable-model-invocation: true".to_owned(),
        "---".to_owned(),
        // Quoted: an account name may hold spaces and any character.
        format!("!`{baton} hop --session \"${{CLAUDE_SESSION_ID}}\" --from-command --to='$ARGUMENTS'`"),
        String::new(),
        "Baton was asked to move this conversation to another account; its answer is above. If it accepted, the move happens when this turn ends: reply with one short line saying so and do nothing else. If it refused, tell the operator why in one line.".to_owned(),
        String::new(),
    ]
    .join("\n")
}

/// How a settings.json is written: its own indentation and line ends.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Format {
    /// One level of indentation.
    indent: String,
    /// The line end.
    eol: &'static str,
    /// Whether the file ends with a line end.
    trailing: bool,
}

/// How a settings.json text is written; two spaces, `\n` and a final line end when there is none.
fn json_format(text: Option<&str>) -> Format {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return Format {
            indent: "  ".to_owned(),
            eol: "\n",
            trailing: true,
        };
    };
    let indent = INDENT
        .as_ref()
        .and_then(|pattern| pattern.captures(text))
        .and_then(|found| found.get(1))
        // No more than ten characters, the most an indentation can be.
        .map_or_else(
            || "  ".to_owned(),
            |found| found.as_str().chars().take(10).collect(),
        );
    Format {
        indent,
        eol: if text.contains("\r\n") { "\r\n" } else { "\n" },
        trailing: text.ends_with('\n'),
    }
}

/// A number as JSON has always been written for the operator: whole numbers without a fraction, and
/// an exponent only for the very large and the very small.
fn number_text(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    if !value.is_finite() {
        return "null".to_owned();
    }
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let point = exponent.parse::<i64>().unwrap_or(0) + 1;
    let count = digits.len() as i64;
    let body = if count <= point && point <= 21 {
        format!("{digits}{}", "0".repeat((point - count) as usize))
    } else if 0 < point && point <= 21 {
        format!("{}.{}", &digits[..point as usize], &digits[point as usize..])
    } else if -6 < point && point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let power = point - 1;
        let sign = if power < 0 { '-' } else { '+' };
        let fraction = if count > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        format!("{}{fraction}e{sign}{}", &digits[..1], power.abs())
    };
    if value < 0.0 { format!("-{body}") } else { body }
}

/// A key that is a whole number below 2^32 - 1 written the plain way: such keys come first in an
/// object, in numeric order.
fn index_key(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) || !key.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    key.parse::<u32>().ok().filter(|index| *index < u32::MAX)
}

/// Writes `value` as indented JSON the way the operator's file has always been written.
fn write_value(out: &mut String, value: &Value, format: &Format, level: usize) {
    let pad = |out: &mut String, level: usize| {
        out.push_str(format.eol);
        for _ in 0..level {
            out.push_str(&format.indent);
        }
    };
    match value {
        Value::Number(number) => match number.as_f64() {
            Some(float) if number.is_f64() || float.abs() >= 9_007_199_254_740_992.0 => {
                out.push_str(&number_text(float));
            }
            _ => out.push_str(&number.to_string()),
        },
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                pad(out, level + 1);
                write_value(out, item, format, level + 1);
            }
            pad(out, level);
            out.push(']');
        }
        Value::Object(fields) if fields.is_empty() => out.push_str("{}"),
        Value::Object(fields) => {
            let mut indexed: Vec<(u32, &String, &Value)> = fields
                .iter()
                .filter_map(|(key, value)| Some((index_key(key)?, key, value)))
                .collect();
            indexed.sort_by_key(|(index, _, _)| *index);
            let named = fields.iter().filter(|(key, _)| index_key(key).is_none());
            out.push('{');
            for (i, (key, value)) in indexed
                .iter()
                .map(|(_, key, value)| (*key, *value))
                .chain(named)
                .enumerate()
            {
                if i > 0 {
                    out.push(',');
                }
                pad(out, level + 1);
                out.push_str(&compact(key));
                out.push_str(": ");
                write_value(out, value, format, level + 1);
            }
            pad(out, level);
            out.push('}');
        }
        other => out.push_str(&compact(other)),
    }
}

/// `value` as the text of a settings.json in `format`.
fn render(value: &Value, format: &Format) -> String {
    let mut out = String::new();
    write_value(&mut out, value, format, 0);
    if format.trailing {
        out.push_str(format.eol);
    }
    out
}

/// A change `baton install` cannot make.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct InstallError(pub String);

/// settings.json: its text (None when there is none) and its value (empty when there is none).
fn read_settings(file: &Path) -> Result<(Option<String>, Object), InstallError> {
    let text = read_text(file);
    let Some(body) = text.as_deref().filter(|text| !text.trim().is_empty()) else {
        return Ok((text, Object::new()));
    };
    let invalid = |why: String| {
        InstallError(format!(
            "{} is not valid JSON ({why}); fix it first.",
            file.display()
        ))
    };
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(value)) => Ok((text, value)),
        Ok(_) => Err(invalid("not an object".to_owned())),
        Err(error) => Err(invalid(error.to_string())),
    }
}

/// The handlers of a hook group, when it has a list of them.
fn handlers(group: &Value) -> Option<&Vec<Value>> {
    group.get("hooks").and_then(Value::as_array)
}

/// The hooks of settings.json without Baton's handlers; a group left empty goes, and so does an
/// event left without groups.
fn without_baton_hooks(hooks: Option<&Value>) -> Object {
    let mut out = Object::new();
    let Some(Value::Object(hooks)) = hooks else {
        return out;
    };
    for (event, groups) in hooks {
        let Some(groups) = groups.as_array() else {
            out.insert(event.clone(), groups.clone());
            continue;
        };
        let mut kept = Vec::new();
        for group in groups {
            let Some(all) = handlers(group) else {
                kept.push(group.clone());
                continue;
            };
            let own: Vec<Value> = all.iter().filter(|h| !is_baton_handler(h)).cloned().collect();
            if own.len() == all.len() {
                kept.push(group.clone());
            } else if !own.is_empty() {
                let mut group = group.clone();
                if let Some(fields) = group.as_object_mut() {
                    fields.insert("hooks".to_owned(), Value::Array(own));
                }
                kept.push(group);
            }
        }
        if !kept.is_empty() {
            out.insert(event.clone(), Value::Array(kept));
        }
    }
    out
}

/// Whether a list of hook groups holds one of Baton's handlers.
fn groups_have_baton(groups: &Value) -> bool {
    groups.as_array().is_some_and(|groups| {
        groups
            .iter()
            .any(|g| handlers(g).is_some_and(|all| all.iter().any(is_baton_handler)))
    })
}

/// Whether the hooks of settings.json hold one of Baton's handlers.
fn has_baton_hooks(hooks: Option<&Value>) -> bool {
    hooks
        .and_then(Value::as_object)
        .is_some_and(|hooks| hooks.values().any(groups_have_baton))
}

/// Baton's handlers for an event.
fn baton_handlers<'a>(hooks: Option<&'a Value>, event: &str) -> Vec<&'a Value> {
    hooks
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .map(|groups| {
            groups
                .iter()
                .filter_map(handlers)
                .flatten()
                .filter(|handler| is_baton_handler(handler))
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the hooks of settings.json hold exactly the handlers Baton wants: one per event, each as
/// it is written now, and none anywhere else.
fn same_baton_hooks(hooks: Option<&Value>, desired: &[(&'static str, Value)]) -> bool {
    for (event, handler) in desired {
        let found = baton_handlers(hooks, event);
        if found.len() != 1 || compact(found[0]) != compact(handler) {
            return false;
        }
    }
    !hooks.and_then(Value::as_object).is_some_and(|hooks| {
        hooks
            .iter()
            .any(|(event, groups)| !desired.iter().any(|(own, _)| own == event) && groups_have_baton(groups))
    })
}

/// A file a plan writes or removes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanFile {
    /// The file.
    pub path: PathBuf,
    /// Its text now; None when it is not there.
    pub before: Option<String>,
    /// Its text after; None when it is removed.
    pub after: Option<String>,
    /// The changed lines.
    pub diff: Vec<String>,
    /// The name of its backup in the backup folder.
    pub backup_name: String,
    /// Whether it is Ferry's launcher file: a failure to write it is a note, not an error.
    pub ferry: bool,
}

/// What one piece of the install comes to, for the screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Item {
    /// The piece: `hooks`, `statusline`, `hop`, `ferry`, `agent`.
    pub name: &'static str,
    /// What happens to it: `add`, `update`, `same`, `skip`, `missing`, `set`, `keep`, `remove`,
    /// `absent`, `restore`, and `failed` once applying it failed.
    pub action: &'static str,
    /// For the hooks: Claude Code's events.
    pub events: Option<Vec<&'static str>>,
    /// The file it is about.
    pub path: Option<PathBuf>,
    /// Why it was skipped.
    pub why: Option<&'static str>,
    /// For Ferry's agent: what it was before Baton.
    pub previous: Option<String>,
    /// For Ferry's agent: the launcher the operator chose.
    pub current: Option<String>,
    /// For a removed status line: whether the operator's previous one is back.
    pub restored: Option<bool>,
}

impl Item {
    /// An item with a name and an action.
    fn new(name: &'static str, action: &'static str) -> Item {
        Item {
            name,
            action,
            ..Item::default()
        }
    }

    /// The item about a file.
    fn at(mut self, path: &Path) -> Item {
        self.path = Some(path.to_path_buf());
        self
    }
}

/// The `ferry config agent` a plan runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentChange {
    /// Where `ferry` is.
    pub program: PathBuf,
    /// The launcher to make the agent.
    pub id: String,
}

/// What `baton install` or `baton uninstall` will do.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The files it writes or removes.
    pub files: Vec<PlanFile>,
    /// What the operator should know.
    pub notes: Vec<String>,
    /// What each piece comes to.
    pub items: Vec<Item>,
    /// The `ferry config agent` to run; None when there is none.
    pub agent: Option<AgentChange>,
    /// What install.json will hold; None for an uninstall, which removes it.
    pub record: Option<Object>,
}

/// The value at `key` of install.json, when it is there and not null.
fn recorded<'a>(record: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    record
        .and_then(|record| record.get(key))
        .filter(|value| !value.is_null())
}

/// Plans `baton install`: `exe` is the baton executable the hooks, the status line, the /hop command
/// and Ferry's launcher will run. `env` and `ferry` find Ferry and run it.
pub fn plan_install(
    paths: &Paths,
    exe: &Path,
    env: &Env,
    ferry: FerryAccess<'_>,
) -> Result<Plan, InstallError> {
    let file = settings_path(paths);
    let (text, value) = read_settings(&file)?;
    let record = read_json(&paths.install);
    let mut settings = value;
    let mut notes = Vec::new();
    // What each piece comes to, for the screen: action is add, update or same.
    let mut items = Vec::new();

    let desired: Vec<(&'static str, Value)> = HOOK_EVENTS
        .iter()
        .map(|(name, event)| (*event, hook_handler(exe, name)))
        .collect();
    let hooks_item = |action: &'static str| Item {
        events: Some(hook_names()),
        ..Item::new("hooks", action)
    };
    if same_baton_hooks(settings.get("hooks"), &desired) {
        items.push(hooks_item("same"));
    } else {
        let had = has_baton_hooks(settings.get("hooks"));
        let mut hooks = without_baton_hooks(settings.get("hooks"));
        for (event, handler) in &desired {
            let mut groups = hooks
                .get(*event)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            groups.push(json!({ "hooks": [handler] }));
            hooks.insert((*event).to_owned(), Value::Array(groups));
        }
        settings.insert("hooks".to_owned(), Value::Object(hooks));
        items.push(hooks_item(if had { "update" } else { "add" }));
    }

    let command = status_line_command(exe);
    let mut previous_status_line = recorded(record.as_ref(), "previousStatusLine")
        .cloned()
        .unwrap_or(Value::Null);
    let current = settings
        .get("statusLine")
        .filter(|current| !current.is_null())
        .cloned();
    match current {
        Some(current) if is_baton_status_line(&current) => {
            let same = current.text("command") == Some(command.as_str());
            if !same {
                let mut changed = current;
                if let Some(fields) = changed.as_object_mut() {
                    fields.insert("command".to_owned(), json!(command));
                }
                settings.insert("statusLine".to_owned(), changed);
            }
            items.push(Item::new("statusline", if same { "same" } else { "update" }));
        }
        current => {
            let mut status_line = Object::new();
            status_line.insert("type".to_owned(), json!("command"));
            status_line.insert("command".to_owned(), json!(command));
            for key in ["padding", "refreshInterval", "hideVimModeIndicator"] {
                if let Some(kept) = current.as_ref().and_then(|current| current.get(key)) {
                    status_line.insert(key.to_owned(), kept.clone());
                }
            }
            settings.insert("statusLine".to_owned(), Value::Object(status_line));
            items.push(Item::new("statusline", "add"));
            if current.as_ref().is_some_and(baton_core::json::is_truthy) {
                notes.push(
                    "Your status line command stays: Baton runs it after recording usage. To see Baton's usage after it: baton policy --status-line-usage true."
                        .to_owned(),
                );
            }
            previous_status_line = current.unwrap_or(Value::Null);
        }
    }

    let mut files = Vec::new();
    let after = render(&Value::Object(settings), &json_format(text.as_deref()));
    if text.as_deref() != Some(after.as_str()) {
        files.push(PlanFile {
            diff: diff_lines(text.as_deref(), Some(&after), CONTEXT),
            path: file.clone(),
            before: text,
            after: Some(after),
            backup_name: "settings.json".to_owned(),
            ferry: false,
        });
    }

    let hop_file = hop_command_path(paths);
    let hop_before = read_text(&hop_file);
    let hop_after = hop_command_text(exe);
    if hop_before
        .as_deref()
        .is_some_and(|before| !before.contains(HOP_MARKER))
    {
        notes.push(format!(
            "{} is your own command; Baton leaves it and does not install /hop (use baton hop).",
            hop_file.display()
        ));
        items.push(Item::new("hop", "skip").at(&hop_file));
    } else if hop_before.as_deref() != Some(hop_after.as_str()) {
        items.push(Item::new("hop", if hop_before.is_none() { "add" } else { "update" }).at(&hop_file));
        files.push(PlanFile {
            diff: diff_lines(hop_before.as_deref(), Some(&hop_after), CONTEXT),
            path: hop_file.clone(),
            before: hop_before,
            after: Some(hop_after),
            backup_name: "hop.md".to_owned(),
            ferry: false,
        });
    } else {
        items.push(Item::new("hop", "same").at(&hop_file));
    }
    // The executable may not be on PATH.
    if runs_from_path(exe) && !find_on_path("baton", env).is_some_and(|found| same_file(&found, exe)) {
        notes.push(format!(
            "The status line and /hop run baton from PATH, since {} cannot start a shell command (a space or a shell's own character in its path), and it is not on PATH: add {} to PATH.",
            exe.display(),
            exe.parent().unwrap_or(exe).display()
        ));
    }

    // Ferry's launcher for Baton: missing means Ferry was not found.
    let found = detect_ferry(paths, env, ferry.find);
    let launcher = found.found.then(|| found.file.clone());
    match &launcher {
        None => items.push(Item::new("ferry", "missing")),
        Some(launcher) => {
            let before = read_text(launcher);
            let after = launcher_text(exe);
            if before.as_deref() == Some(after.as_str()) {
                items.push(Item::new("ferry", "same").at(launcher));
            } else {
                if before
                    .as_deref()
                    .is_some_and(|before| !is_baton_launcher(parse_launcher(before).as_ref()))
                {
                    notes.push(format!(
                        "{} is not Baton's; it is backed up and replaced by Baton's.",
                        launcher.display()
                    ));
                }
                items.push(Item::new("ferry", if before.is_none() { "add" } else { "update" }).at(launcher));
                files.push(PlanFile {
                    diff: diff_lines(before.as_deref(), Some(&after), CONTEXT),
                    path: launcher.clone(),
                    before,
                    after: Some(after),
                    backup_name: "ferry-baton.json".to_owned(),
                    ferry: true,
                });
            }
        }
    }

    // The agent Ferry opens with `c`: Baton, once it is registered, unless a launcher was chosen on
    // purpose. Through Ferry's own CLI, in apply_plan; the record keeps what it was, for uninstall,
    // while it is Baton's doing.
    let mut agent = None;
    let mut previous_agent = recorded(record.as_ref(), "previousAgent").cloned();
    if launcher.is_some() {
        let skipped = |why: &'static str| Item {
            why: Some(why),
            ..Item::new("agent", "skip")
        };
        match &found.cli {
            None => items.push(skipped("ferry is not on PATH")),
            Some(cli) => match ferry_agent(cli, env, ferry.run) {
                None => items.push(skipped("ferry config agent did not answer")),
                Some(current) if current.value == LAUNCHER_ID => items.push(Item::new("agent", "same")),
                Some(current) if takes_over(&current) => {
                    let previous = if current.default {
                        "default".to_owned()
                    } else {
                        current.value
                    };
                    agent = Some(AgentChange {
                        program: cli.clone(),
                        id: LAUNCHER_ID.to_owned(),
                    });
                    items.push(Item {
                        previous: Some(previous.clone()),
                        ..Item::new("agent", "set")
                    });
                    previous_agent = Some(json!(previous));
                }
                Some(current) => {
                    previous_agent = None;
                    notes.push(format!(
                        "Ferry opens {} with c, as you chose; ferry config agent baton makes it Baton.",
                        current.value
                    ));
                    items.push(Item {
                        current: Some(current.value),
                        ..Item::new("agent", "keep")
                    });
                }
            },
        }
    }

    let mut out = Object::new();
    out.insert("version".to_owned(), json!(1));
    out.insert("exe".to_owned(), json!(forward(exe)));
    out.insert("settingsPath".to_owned(), json!(file.to_string_lossy()));
    out.insert("commandPath".to_owned(), json!(hop_file.to_string_lossy()));
    out.insert("previousStatusLine".to_owned(), previous_status_line);
    out.insert("hooks".to_owned(), json!(hook_names()));
    if let Some(launcher) = &launcher {
        out.insert("launcher".to_owned(), json!(launcher.to_string_lossy()));
    }
    if let Some(previous) = previous_agent {
        out.insert("previousAgent".to_owned(), previous);
    }
    Ok(Plan {
        files,
        notes,
        items,
        agent,
        record: Some(out),
    })
}

/// Plans `baton uninstall`: Baton's entries go, and what was there before comes back.
pub fn plan_uninstall(paths: &Paths, env: &Env, ferry: FerryAccess<'_>) -> Result<Plan, InstallError> {
    let file = settings_path(paths);
    let (text, value) = read_settings(&file)?;
    let record = read_json(&paths.install);
    let mut settings = value;
    let mut notes = Vec::new();
    let mut items = Vec::new();
    let had_hooks = has_baton_hooks(settings.get("hooks"));
    if settings.get("hooks").is_some_and(baton_core::json::is_truthy) {
        let hooks = without_baton_hooks(settings.get("hooks"));
        if hooks.is_empty() {
            settings.shift_remove("hooks");
        } else {
            settings.insert("hooks".to_owned(), Value::Object(hooks));
        }
    }
    items.push(Item {
        events: Some(hook_names()),
        ..Item::new("hooks", if had_hooks { "remove" } else { "absent" })
    });
    let previous =
        recorded(record.as_ref(), "previousStatusLine").filter(|value| baton_core::json::is_truthy(value));
    if settings.get("statusLine").is_some_and(is_baton_status_line) {
        match previous {
            Some(previous) => {
                settings.insert("statusLine".to_owned(), previous.clone());
            }
            None => {
                settings.shift_remove("statusLine");
            }
        }
        items.push(Item {
            restored: Some(previous.is_some()),
            ..Item::new("statusline", "remove")
        });
    } else {
        items.push(Item::new("statusline", "absent"));
        if settings
            .get("statusLine")
            .is_some_and(baton_core::json::is_truthy)
            && previous.is_some()
        {
            notes.push("The status line was changed after install; Baton leaves it as it is.".to_owned());
        }
    }
    let mut files = Vec::new();
    let after = if text.is_none() && settings.is_empty() {
        None
    } else {
        Some(render(&Value::Object(settings), &json_format(text.as_deref())))
    };
    if after != text {
        files.push(PlanFile {
            diff: diff_lines(text.as_deref(), after.as_deref(), CONTEXT),
            path: file,
            before: text,
            after,
            backup_name: "settings.json".to_owned(),
            ferry: false,
        });
    }
    let hop_file = hop_command_path(paths);
    let hop_before = read_text(&hop_file).filter(|before| before.contains(HOP_MARKER));
    items.push(Item::new("hop", if hop_before.is_some() { "remove" } else { "absent" }).at(&hop_file));
    if let Some(before) = hop_before {
        files.push(PlanFile {
            diff: diff_lines(Some(&before), None, CONTEXT),
            path: hop_file,
            before: Some(before),
            after: None,
            backup_name: "hop.md".to_owned(),
            ferry: false,
        });
    }
    // Ferry's launcher file, where it is now and where install wrote it (FERRY_HOME may have changed
    // since): removed only while Baton's.
    let data = ferry_home(paths, env);
    let mut launchers = vec![launcher_path(&data)];
    if let Some(written) = recorded(record.as_ref(), "launcher").and_then(Value::as_str)
        && !written.is_empty()
        && !same_path(&launchers[0], written)
    {
        launchers.push(PathBuf::from(written));
    }
    let mut listed = false;
    for (i, launcher) in launchers.iter().enumerate() {
        let Some(before) = read_text(launcher) else {
            continue;
        };
        listed = true;
        if !is_baton_launcher(parse_launcher(&before).as_ref()) {
            notes.push(format!("{} is not Baton's; Baton leaves it.", launcher.display()));
            items.push(Item::new("ferry", "skip").at(launcher));
            continue;
        }
        items.push(Item::new("ferry", "remove").at(launcher));
        files.push(PlanFile {
            diff: diff_lines(Some(&before), None, CONTEXT),
            path: launcher.clone(),
            before: Some(before),
            after: None,
            backup_name: if i == 0 {
                "ferry-baton.json".to_owned()
            } else {
                format!("ferry-baton-{}.json", i + 1)
            },
            ferry: true,
        });
    }
    if !listed && data.exists() {
        items.push(Item::new("ferry", "absent"));
    }
    // The agent Ferry opens goes back to what it was before Baton took its place, while it is still
    // Baton.
    let mut agent = None;
    if let Some(previous) = recorded(record.as_ref(), "previousAgent").and_then(Value::as_str) {
        let found = detect_ferry(paths, env, ferry.find);
        match &found.cli {
            None => {
                items.push(Item {
                    why: Some("ferry is not on PATH"),
                    ..Item::new("agent", "skip")
                });
                notes.push(format!(
                    "Ferry may still open Baton with c: ferry config agent {previous} puts the one before back."
                ));
            }
            Some(cli) => {
                if ferry_agent(cli, env, ferry.run).is_some_and(|current| current.value == LAUNCHER_ID) {
                    agent = Some(AgentChange {
                        program: cli.clone(),
                        id: previous.to_owned(),
                    });
                    items.push(Item {
                        previous: Some(previous.to_owned()),
                        ..Item::new("agent", "restore")
                    });
                } else {
                    items.push(Item::new("agent", "absent"));
                }
            }
        }
    }
    Ok(Plan {
        files,
        notes,
        items,
        record: None,
        agent,
    })
}

/// A record for comparing: without when it was written and where its backup is.
fn record_key(record: Option<&Object>) -> Option<String> {
    let mut record = record?.clone();
    record.insert("installedAt".to_owned(), Value::Null);
    record.insert("backup".to_owned(), Value::Null);
    Some(compact(&record))
}

/// install.json as it is, when it holds a record.
fn existing_record(paths: &Paths) -> Option<Object> {
    match read_json(&paths.install)? {
        Value::Object(record) => Some(record),
        _ => None,
    }
}

/// Whether applying a plan would change nothing.
pub fn plan_is_noop(plan: &Plan, paths: &Paths) -> bool {
    let existing = read_json(&paths.install).filter(baton_core::json::is_truthy);
    plan.files.is_empty()
        && plan.agent.is_none()
        && match &plan.record {
            Some(record) => {
                record_key(existing.as_ref().and_then(Value::as_object)) == record_key(Some(record))
            }
            None => existing.is_none(),
        }
}

/// What applying a plan did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// Whether anything was changed.
    pub changed: bool,
    /// The folder the previous files were copied to; None when there were none.
    pub backup: Option<PathBuf>,
}

/// File work that fails as an [`InstallError`] saying what could not be done.
fn attempt(what: &str, file: &Path, work: impl FnOnce() -> io::Result<()>) -> Result<(), InstallError> {
    work().map_err(|error| InstallError(format!("Could not {what} {}: {error}", file.display())))
}

/// Writes a plan: backups first; then an install's record, before any file, since it holds the
/// previous status line, which must not be lost to a write that fails after settings.json; then each
/// file atomically (through a symbolic link to its target); and an uninstall's record goes last, for
/// the same reason. Ferry's launcher that cannot be written or removed is a note on the plan (its item
/// says it failed), and the rest goes on; so is `ferry config agent`, run last, once the launcher is
/// there for Ferry to find. Returns the backup folder.
pub fn apply_plan(
    plan: &mut Plan,
    paths: &Paths,
    now: Ms,
    env: &Env,
    run: Option<RunFerry<'_>>,
) -> Result<Applied, InstallError> {
    if plan_is_noop(plan, paths) {
        return Ok(Applied {
            changed: false,
            backup: None,
        });
    }
    let stamp = iso(now).replace([':', '.'], "-");
    let mut backup = None;
    for f in &plan.files {
        let Some(before) = &f.before else {
            continue;
        };
        let folder = paths.backups.join(&stamp);
        attempt("back up", &f.path, || {
            fs::create_dir_all(&folder)?;
            write_file_atomic(&folder.join(&f.backup_name), before, Modes::default())
        })?;
        backup = Some(folder);
    }
    let installing = plan.record.is_some();
    if let Some(record) = &plan.record {
        let mut record = record.clone();
        record.insert("installedAt".to_owned(), json!(iso(now)));
        let kept = existing_record(paths).and_then(|existing| existing.get("backup").cloned());
        record.insert(
            "backup".to_owned(),
            match &backup {
                Some(folder) => json!(folder.to_string_lossy()),
                None => kept.unwrap_or(Value::Null),
            },
        );
        attempt("write", &paths.install, || {
            write_json_atomic(&paths.install, &record)
        })?;
    }
    for f in &plan.files {
        // A symbolic link stays one: its target is what is written.
        let target = match fs::symlink_metadata(&f.path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                fs::canonicalize(&f.path).unwrap_or_else(|_| f.path.clone())
            }
            _ => f.path.clone(),
        };
        let done = match &f.after {
            None => attempt("remove", &f.path, || remove_file(&target)),
            Some(after) => attempt("write", &f.path, || {
                write_file_atomic(&target, after, if f.ferry { PRIVATE } else { Modes::default() })
            }),
        };
        let Err(error) = done else {
            continue;
        };
        if !f.ferry {
            return Err(error);
        }
        plan.notes.push(format!(
            "{error}. The rest is done; fix it and run baton {} again to {} Ferry.",
            if installing { "install" } else { "uninstall" },
            if installing {
                "register Baton with"
            } else {
                "remove Baton from"
            }
        ));
        for item in &mut plan.items {
            if item.name == "ferry" && item.path.as_deref() == Some(f.path.as_path()) {
                item.action = "failed";
            }
        }
    }
    if let Some(agent) = &plan.agent {
        let result = set_ferry_agent(&agent.program, &agent.id, env, run);
        if result.status != Some(0) {
            let said = result.stderr.trim();
            plan.notes.push(format!(
                "ferry config agent {} failed{}. The rest is done; run it yourself to {}.",
                agent.id,
                if said.is_empty() {
                    String::new()
                } else {
                    format!(": {said}")
                },
                if installing {
                    "make Baton the agent Ferry opens"
                } else {
                    "put the agent Ferry opens back"
                }
            ));
            for item in &mut plan.items {
                if item.name == "agent" {
                    item.action = "failed";
                }
            }
        }
    }
    if !installing {
        attempt("remove", &paths.install, || remove_file(&paths.install))?;
    }
    Ok(Applied {
        changed: true,
        backup,
    })
}

/// One of Baton's hooks in settings.json.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookState {
    /// Claude Code's event.
    pub event: &'static str,
    /// The name `baton hook` takes for it.
    pub name: &'static str,
    /// Whether Baton's handler is there.
    pub installed: bool,
    /// Whether the program the handler runs is still where it says (false when it is not installed).
    pub program_exists: bool,
}

/// Whether Baton's pieces are in settings.json.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InstallState {
    /// Whether settings.json could be read.
    pub ok: bool,
    /// Why not.
    pub error: Option<String>,
    /// Whether the status line is Baton's.
    pub status_line: bool,
    /// Each of Baton's hooks.
    pub hooks: Vec<HookState>,
    /// Whether the /hop command is Baton's.
    pub hop_command: bool,
    /// install.json.
    pub record: Option<Value>,
}

/// Whether Baton's pieces are in settings.json (for doctor and status).
pub fn install_state(paths: &Paths) -> InstallState {
    let Ok((_, settings)) = read_settings(&settings_path(paths)) else {
        return InstallState {
            error: Some("settings.json is not valid JSON".to_owned()),
            ..InstallState::default()
        };
    };
    let hooks: Vec<HookState> = HOOK_EVENTS
        .iter()
        .map(
            |&(name, event)| match baton_handlers(settings.get("hooks"), event).first() {
                None => HookState {
                    event,
                    name,
                    installed: false,
                    program_exists: false,
                },
                Some(handler) => HookState {
                    event,
                    name,
                    installed: true,
                    program_exists: handler
                        .text("command")
                        .is_some_and(|program| Path::new(program).exists()),
                },
            },
        )
        .collect();
    InstallState {
        ok: true,
        error: None,
        status_line: settings.get("statusLine").is_some_and(is_baton_status_line),
        hooks,
        hop_command: read_text(&hop_command_path(paths)).is_some_and(|hop| hop.contains(HOP_MARKER)),
        record: read_json(&paths.install),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_as_they_always_were() {
        for (value, text) in [
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-2.5, "-2.5"),
            (1000.0, "1000"),
            (1e21, "1e+21"),
            (1.5e22, "1.5e+22"),
            (123456789012345680000.0, "123456789012345680000"),
            (0.000001, "0.000001"),
            (0.0000001, "1e-7"),
            (1.25e-9, "1.25e-9"),
            (0.1, "0.1"),
            (12.5, "12.5"),
            (f64::NAN, "null"),
        ] {
            assert_eq!(number_text(value), text);
        }
    }

    #[test]
    fn settings_are_rendered_like_the_operators_own_file() {
        let value: Value = serde_json::from_str(
            r#"{"b":1.0,"a":[1,2.50,"x\ty"],"10":"ten","2":"two","empty":{},"none":[],"big":12345678901234567890,"small":1e-7,"nested":{"k":null,"t":true}}"#,
        )
        .unwrap();
        let two = Format {
            indent: "  ".to_owned(),
            eol: "\n",
            trailing: true,
        };
        assert_eq!(
            render(&value, &two),
            "{\n  \"2\": \"two\",\n  \"10\": \"ten\",\n  \"b\": 1,\n  \"a\": [\n    1,\n    2.5,\n    \"x\\ty\"\n  ],\n  \"empty\": {},\n  \"none\": [],\n  \"big\": 12345678901234567000,\n  \"small\": 1e-7,\n  \"nested\": {\n    \"k\": null,\n    \"t\": true\n  }\n}\n"
        );
        let tabs = Format {
            indent: "\t".to_owned(),
            eol: "\r\n",
            trailing: false,
        };
        assert_eq!(
            render(&json!({ "a": [1] }), &tabs),
            "{\r\n\t\"a\": [\r\n\t\t1\r\n\t]\r\n}"
        );
        assert_eq!(render(&json!({}), &two), "{}\n");
    }

    #[test]
    fn a_files_own_format_is_kept() {
        assert_eq!(
            json_format(None),
            Format {
                indent: "  ".to_owned(),
                eol: "\n",
                trailing: true
            }
        );
        assert_eq!(
            json_format(Some("")),
            Format {
                indent: "  ".to_owned(),
                eol: "\n",
                trailing: true
            }
        );
        assert_eq!(
            json_format(Some("{\r\n    \"a\": 1\r\n}\r\n")),
            Format {
                indent: "    ".to_owned(),
                eol: "\r\n",
                trailing: true
            }
        );
        assert_eq!(
            json_format(Some("{\n\t\"a\": 1\n}")),
            Format {
                indent: "\t".to_owned(),
                eol: "\n",
                trailing: false
            }
        );
        assert_eq!(
            json_format(Some("{}")),
            Format {
                indent: "  ".to_owned(),
                eol: "\n",
                trailing: false
            }
        );
        assert_eq!(
            json_format(Some("{\n               \"a\": 1\n}\n")).indent.len(),
            10
        );
    }

    #[test]
    fn the_program_in_a_shell_line() {
        assert_eq!(
            baton_command(Path::new("C:/Users/op/.cargo/bin/baton.exe")),
            "C:/Users/op/.cargo/bin/baton.exe"
        );
        assert_eq!(
            baton_command(Path::new("/usr/local/bin/baton")),
            "/usr/local/bin/baton"
        );
        assert_eq!(
            baton_command(Path::new("C:/Program Files/Baton/baton.exe")),
            "baton"
        );
        assert_eq!(baton_command(Path::new("C:/Users/O'Brien/baton.exe")), "baton");
        assert_eq!(
            baton_command(Path::new("C:/Χρήστης/baton.exe")),
            "C:/Χρήστης/baton.exe"
        );
        assert_eq!(
            status_line_command(Path::new("C:/tools/baton.exe")),
            "C:/tools/baton.exe statusline"
        );
    }

    #[test]
    fn whose_a_handler_is() {
        let own = hook_handler(Path::new("C:/tools/baton/baton.exe"), "stop");
        assert_eq!(
            own,
            json!({ "type": "command", "command": "C:/tools/baton/baton.exe", "args": ["hook", "stop"], "timeout": 10 })
        );
        assert!(is_baton_handler(&own));
        assert!(is_baton_handler(&hook_handler(
            Path::new("/usr/local/bin/baton"),
            "session-start"
        )));
        assert!(is_baton_handler(&hook_handler(
            Path::new("target/debug/baton-0f3a.exe"),
            "prompt"
        )));
        // Anyone else's.
        for other in [
            json!({ "type": "command", "command": "echo pre" }),
            json!({ "type": "command", "command": "C:/tools/other.exe", "args": ["hook", "stop"] }),
            json!({ "type": "command", "command": "C:/tools/baton.exe", "args": ["hook", "unknown"] }),
            json!({ "type": "command", "command": "C:/tools/baton.exe", "args": ["status"] }),
            json!({ "type": "prompt", "command": "C:/tools/baton.exe", "args": ["hook", "stop"] }),
            json!({ "type": "command", "command": "node", "args": ["C:/b/src/cli.js", "hook", "stop"] }),
            json!("text"),
            json!(null),
        ] {
            assert!(!is_baton_handler(&other), "{other}");
        }
    }

    #[test]
    fn whose_a_status_line_is() {
        let ours = |command: &str| is_baton_status_line(&json!({ "type": "command", "command": command }));
        assert!(ours(&status_line_command(Path::new("C:/tools/baton/baton.exe"))));
        assert!(ours("baton statusline"));
        assert!(ours("/usr/local/bin/baton statusline"));
        assert!(ours("\"C:/Program Files/Baton/baton.exe\" statusline"));
        assert!(ours("C:\\tools\\BATON.EXE statusline"));
        assert!(ours("target/debug/baton-0f3a.exe statusline"));
        for other in [
            "~/.claude/statusline.sh",
            "node \"C:/b/src/cli.js\" statusline",
            "mybaton statusline",
            "baton status",
            "baton statuslines",
            "echo baton",
        ] {
            assert!(!ours(other), "{other}");
        }
        assert!(!is_baton_status_line(&json!({ "command": 7 })));
        assert!(!is_baton_status_line(&json!(null)));
    }

    #[test]
    fn the_hop_command_runs_the_baton_executable() {
        let text = hop_command_text(Path::new("C:/tools/baton/baton.exe"));
        assert_eq!(
            text,
            "---\ndescription: Move this conversation to another account at the end of this turn (installed by Baton)\nargument-hint: \"[account]\"\nallowed-tools: Bash(C:/tools/baton/baton.exe:*)\ndisable-model-invocation: true\n---\n!`C:/tools/baton/baton.exe hop --session \"${CLAUDE_SESSION_ID}\" --from-command --to='$ARGUMENTS'`\n\nBaton was asked to move this conversation to another account; its answer is above. If it accepted, the move happens when this turn ends: reply with one short line saying so and do nothing else. If it refused, tell the operator why in one line.\n"
        );
        let spaced = hop_command_text(Path::new("C:/Program Files/Baton/baton.exe"));
        assert!(spaced.contains("allowed-tools: Bash(baton:*)\n"));
        assert!(spaced.contains("!`baton hop --session "));
    }
}
