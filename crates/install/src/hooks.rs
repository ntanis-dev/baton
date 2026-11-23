//! The hook handlers and the status line.
//!
//! Claude Code runs them on every session event, so they are small, synchronous, write atomically,
//! print nothing (a SessionStart or UserPromptSubmit hook's stdout would reach the model) and never
//! fail: bad input is ignored. Each one learns its account from `CLAUDE_CONFIG_DIR` (a profile of one of
//! Baton's accounts) and, when the process was started by `baton`, the wrapper from `BATON_WRAPPER`.
//! They live in the shared settings.json, so they also run in plain claude (no `CLAUDE_CONFIG_DIR`, or
//! a folder that is not one of Baton's accounts'): there they do nothing at all, and the status line
//! only runs the operator's own status line command, if any.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use baton_core::charts::usage_tone;
use baton_core::clock::{Ms, iso, ms_of, parse_ms};
use baton_core::config::{
    Config, config_problem, effective_policy, load_config, name_of, same_id, thresholds,
};
use baton_core::env::Env;
use baton_core::fsutil::{read_json, write_json_atomic};
use baton_core::json::{Get, compact, is_truthy, num};
use baton_core::lines::{Line, Span, line_width, p, s};
use baton_core::paths::{Paths, account_from_env, resolve};
use baton_core::policy::window_state;
use baton_core::sessions::{
    append_turn_sample, read_session_file, valid_id, wrapper_session_file, write_session_file,
};
use baton_core::term::{Terminal, inherited_depth, make_terminal, render};
use baton_core::text::{clean, round, width};
use baton_core::theme::{GlyphSet, pick_glyphs};
use baton_core::usage::{
    LIMIT_ERRORS, Reading, RecordOptions, Rejection, Sample, infer_rejection, no_five_hour, read_reading,
    reading_from_status_line, record_event, record_reading, record_rejection,
};
use baton_core::window::{PerWindow, WINDOWS, Window};
use serde_json::{Value, json};

use crate::proc;

/// The account of this process, or None when it is not one of Baton's. The id is the config's own
/// spelling: the profile folder may be given in another case, and it is the same folder.
pub fn baton_account(env: &Env, paths: &Paths) -> Option<String> {
    let id = account_from_env(env, paths)?;
    load_config(paths)
        .accounts
        .into_iter()
        .find(|a| same_id(&a.id, &id))
        .map(|a| a.id)
}

/// What to tell the operator when this process runs in one of Baton's profiles and config.json cannot
/// be used: without the config the profile is nobody's account, and nothing would be recorded or shown
/// with no word of why.
pub fn config_notice(env: &Env, paths: &Paths) -> Option<String> {
    account_from_env(env, paths)?;
    let problem = config_problem(paths)?;
    Some(format!("baton: {problem} (baton doctor says more)"))
}

/// The pid of the Claude Code process running this hook or status line (Claude Code sets
/// `CLAUDE_PID` for them), or None. It goes with the wrapper's session mapping: a claude started inside
/// a session inherits `BATON_WRAPPER`, and the wrapper follows only the claude it started.
pub fn claude_pid(env: &Env) -> Option<i64> {
    let pid = env.get("CLAUDE_PID")?.trim().parse::<f64>().ok()?;
    (pid.is_finite() && pid.fract() == 0.0 && pid > 0.0).then_some(pid as i64)
}

/// The hooks Baton installs: the name `baton hook` takes, and Claude Code's event.
pub const HOOK_EVENTS: [(&str, &str); 6] = [
    ("session-start", "SessionStart"),
    ("prompt", "UserPromptSubmit"),
    ("stop", "Stop"),
    ("stop-failure", "StopFailure"),
    ("session-end", "SessionEnd"),
    // A settings file changed during a session: where the profiles hold copies of it, the command
    // line brings the others in step ([`CONFIG_CHANGE`]). Nothing is recorded for it.
    (CONFIG_CHANGE, "ConfigChange"),
];

/// The hook that says a settings file changed during a session.
pub const CONFIG_CHANGE: &str = "config-change";

/// Claude Code's event for a name `baton hook` takes.
pub fn hook_event(name: &str) -> Option<&'static str> {
    HOOK_EVENTS
        .iter()
        .find(|(own, _)| *own == name)
        .map(|(_, event)| *event)
}

/// A text cut to `n` UTF-16 units, the way Claude Code's own fields are counted; None for anything
/// that is not text.
fn clip(value: &Value, n: usize) -> Option<String> {
    let text = value.as_str()?;
    let mut units = 0;
    let mut out = String::new();
    for ch in text.chars() {
        units += ch.len_utf16();
        if units > n {
            break;
        }
        out.push(ch);
    }
    Some(out)
}

/// The text at `key`, or null.
fn text_or_null(input: &Value, key: &str) -> Value {
    json!(input.text(key))
}

/// A turn's background tasks, as little as the wrapper needs of them.
fn task_summary(tasks: &Value) -> Value {
    let Some(tasks) = tasks.as_array() else {
        return json!([]);
    };
    tasks
        .iter()
        .take(50)
        .map(|task| {
            let name = match task.get("name") {
                Some(name) if !name.is_null() => name,
                _ => task.at("agent_type"),
            };
            json!({ "type": task.text("type"), "status": task.text("status"), "name": clip(name, 80) })
        })
        .collect()
}

/// Handles one hook event: `event` is the name `baton hook` was given and `input` the JSON Claude
/// Code wrote to it. Returns what was recorded (`start`, `wrapper`, `compact`, `turn`, `sample`,
/// `rejection`, `end`); nothing for bad input, an event Baton does not know, or a process that is not
/// one of Baton's accounts'. Never fails: a file that cannot be written ends it quietly.
pub fn handle_hook(event: &str, input: &Value, env: &Env, paths: &Paths, now: Ms) -> Vec<&'static str> {
    let mut recorded = Vec::new();
    if !input.is_object() || hook_event(event).is_none() {
        return recorded;
    }
    let Some(account) = baton_account(env, paths) else {
        return recorded;
    };
    let Some(id) = input.text("session_id").filter(|id| valid_id(id)) else {
        return recorded;
    };
    let session = Session {
        env,
        paths,
        now,
        account: &account,
        id,
    };
    let _ = record_hook(event, input, &session, &mut recorded);
    recorded
}

/// The session a hook runs in: where it writes, when, and for which account and conversation.
struct Session<'a> {
    /// The environment Claude Code ran the hook with.
    env: &'a Env,
    /// Where Baton keeps its files.
    paths: &'a Paths,
    /// The time now.
    now: Ms,
    /// The account's id.
    account: &'a str,
    /// The conversation's id.
    id: &'a str,
}

/// What [`handle_hook`] writes, stopping at the first file that cannot be written.
fn record_hook(
    event: &str,
    input: &Value,
    session: &Session<'_>,
    recorded: &mut Vec<&'static str>,
) -> io::Result<()> {
    let Session {
        env,
        paths,
        now,
        account,
        id,
    } = *session;
    let at = iso(now);
    let wrapper = env.get("BATON_WRAPPER").filter(|w| valid_id(w));
    let cwd = text_or_null(input, "cwd");
    match event {
        "session-start" => {
            let source = text_or_null(input, "source");
            write_session_file(
                paths,
                id,
                "start.json",
                &json!({
                    "at": at,
                    "source": source,
                    "account": account,
                    "cwd": cwd,
                    "transcriptPath": text_or_null(input, "transcript_path"),
                    "model": text_or_null(input, "model"),
                    "wrapper": wrapper,
                }),
            )?;
            recorded.push("start");
            if let Some(wrapper) = wrapper {
                write_json_atomic(
                    &wrapper_session_file(paths, wrapper),
                    &json!({
                        "session": id,
                        "at": at,
                        "source": source,
                        "account": account,
                        "pid": claude_pid(env),
                    }),
                )?;
                recorded.push("wrapper");
            }
            if source == "compact" {
                write_session_file(paths, id, "compact.json", &json!({ "at": at }))?;
                recorded.push("compact");
            }
            record_event(
                paths,
                json!({ "type": "session", "session": id, "account": account, "source": source, "wrapper": wrapper }),
                now,
            );
        }
        "prompt" => {
            write_session_file(
                paths,
                id,
                "turn.json",
                &json!({
                    "at": at,
                    "state": "running",
                    "event": "prompt",
                    "account": account,
                    "permissionMode": text_or_null(input, "permission_mode"),
                }),
            )?;
            recorded.push("turn");
        }
        "stop" => {
            write_session_file(
                paths,
                id,
                "turn.json",
                &json!({
                    "at": at,
                    "state": "idle",
                    "event": "stop",
                    "account": account,
                    "permissionMode": text_or_null(input, "permission_mode"),
                    "backgroundTasks": task_summary(input.at("background_tasks")),
                }),
            )?;
            recorded.push("turn");
            // The session's latest reading (from its status line) at this turn end.
            if let Some(live) = read_session_file(paths, id, "live.json")
                && live.truthy("reading")
                && live.text("account") == Some(account)
            {
                let reading = live.at("reading");
                append_turn_sample(
                    paths,
                    id,
                    &json!({
                        "at": at,
                        "account": account,
                        "five_hour": reading.at("five_hour"),
                        "seven_day": reading.at("seven_day"),
                    }),
                )?;
                recorded.push("sample");
            }
        }
        "stop-failure" => {
            let error = input.text("error").unwrap_or("unknown");
            let message = clip(input.at("last_assistant_message"), 400);
            let details = match input.at("error_details") {
                Value::String(details) => clip(&json!(details), 400),
                other if is_truthy(other) => clip(&json!(compact(other)), 400),
                _ => None,
            };
            let mut rejection = None;
            if LIMIT_ERRORS.contains(&error) {
                let inferred = infer_rejection(
                    Some(error),
                    message.as_deref(),
                    details.as_deref(),
                    read_reading(paths, account).as_ref(),
                    now,
                );
                // The refusal first, then the turn: the wrapper acts on the turn.
                record_rejection(
                    paths,
                    account,
                    &Rejection {
                        account: account.to_owned(),
                        window: inferred.window.as_str().to_owned(),
                        at: Some(at.clone()),
                        until: inferred.until.clone(),
                        until_source: Some(inferred.until_source.to_owned()),
                        error: Some(error.to_owned()),
                        session: Some(id.to_owned()),
                        message: message.clone(),
                    },
                );
                record_event(
                    paths,
                    json!({
                        "type": "refusal",
                        "session": id,
                        "account": account,
                        "error": error,
                        "window": inferred.window.as_str(),
                        "until": inferred.until,
                    }),
                    now,
                );
                recorded.push("rejection");
                rejection = Some(inferred);
            }
            write_session_file(
                paths,
                id,
                "turn.json",
                &json!({
                    "at": at,
                    "state": "idle",
                    "event": "stop-failure",
                    "account": account,
                    "error": error,
                    "window": rejection.as_ref().map(|r| r.window.as_str()),
                    "until": rejection.as_ref().and_then(|r| r.until.as_deref()),
                    "message": message,
                }),
            )?;
            recorded.push("turn");
        }
        "session-end" => {
            write_session_file(
                paths,
                id,
                "end.json",
                &json!({ "at": at, "reason": text_or_null(input, "reason"), "account": account }),
            )?;
            recorded.push("end");
        }
        _ => {}
    }
    Ok(())
}

/// What `baton statusline` prints.
#[derive(Clone, Debug, PartialEq)]
pub enum StatusLine {
    /// Baton's own line; empty when there is nothing to print.
    Text(String),
    /// The operator's own status line command, to run instead.
    Previous {
        /// The command.
        command: String,
        /// Baton's line, to add after their output when the policy says so (see [`append_usage`]).
        usage: Option<Box<UsageLine>>,
    },
}

/// Baton's status line for one account at one moment, for any width.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageLine {
    /// The account's current name.
    name: String,
    /// The account's reading.
    reading: Option<Reading>,
    /// The moment.
    now: Ms,
    /// Every account together.
    overall: Overall,
    /// The account's thresholds.
    limits: PerWindow<f64>,
    /// The colour depth of the terminal Claude Code draws in.
    depth: u8,
    /// The glyphs it can show.
    glyphs: GlyphSet,
}

impl UsageLine {
    /// The line for a width in cells (0: as wide as it likes).
    pub fn line(&self, columns: usize) -> String {
        own_status_line(
            Some(&self.name),
            self.reading.as_ref(),
            self.now,
            &LineOptions {
                columns,
                overall: Some(&self.overall),
                limits: self.limits,
                // The line goes through a pipe to Claude Code, which draws it in the terminal this
                // process inherited its environment from.
                term: Some(make_terminal(false, self.depth, self.glyphs, columns)),
            },
        )
    }
}

/// A width from the `COLUMNS` variable: 0 when it is not set or not a width.
pub fn columns_of(env: &Env) -> usize {
    match env
        .get("COLUMNS")
        .and_then(|columns| columns.trim().parse::<f64>().ok())
    {
        Some(columns) if columns.is_finite() && columns >= 1.0 => columns as usize,
        _ => 0,
    }
}

/// What a status line run says of each window, as live.json holds it; null when it said nothing.
fn live_reading(parsed: Option<&PerWindow<Option<Sample>>>) -> Value {
    let Some(parsed) = parsed else {
        return Value::Null;
    };
    let mut out = serde_json::Map::new();
    for (window, sample) in parsed.iter() {
        if let Some(sample) = sample {
            out.insert(
                window.as_str().to_owned(),
                json!({ "used": num(sample.used), "resetsAt": sample.resets_at }),
            );
        }
    }
    Value::Object(out)
}

/// The status line: records the account's rate_limits and the session's live state, then returns
/// what to print: Baton's own line, or the operator's own status line command to run instead.
pub fn handle_status_line(input: &Value, env: &Env, paths: &Paths, now: Ms) -> StatusLine {
    let account = baton_account(env, paths);
    let previous = read_json(&paths.install).and_then(|install| {
        install
            .at("previousStatusLine")
            .word("command")
            .map(str::to_owned)
    });
    let Some(account) = account else {
        return match previous {
            Some(command) => StatusLine::Previous { command, usage: None },
            None => StatusLine::Text(String::new()),
        };
    };
    let mut reading = None;
    // A list reads as input that says nothing, as it always has.
    if input.is_object() || input.is_array() {
        let id = input.text("session_id").filter(|id| valid_id(id));
        let parsed = reading_from_status_line(input);
        reading = match &parsed {
            Some(parsed) => {
                let options = RecordOptions {
                    session: id.map(str::to_owned),
                    ..RecordOptions::at(now)
                };
                record_reading(paths, &account, parsed, options)
                    .reading
                    .or_else(|| read_reading(paths, &account))
            }
            None => read_reading(paths, &account),
        };
        if let Some(id) = id {
            record_live(input, env, paths, now, &account, id, parsed.as_ref());
        }
    }
    let config = load_config(paths);
    let policy = effective_policy(&config);
    let line = || UsageLine {
        // The account's current name (the files are keyed by its id).
        name: name_of(&config, &account),
        overall: overall_usage(paths, &config, &account, reading.as_ref(), now),
        limits: match config.accounts.iter().find(|a| a.id == account) {
            Some(own) => thresholds(own, &policy),
            None => PerWindow {
                five_hour: policy.five_hour,
                seven_day: policy.weekly,
            },
        },
        reading: reading.clone(),
        now,
        depth: inherited_depth(env),
        glyphs: pick_glyphs(env),
    };
    // With the operator's own status line, Baton's line is added to the end of theirs only when the
    // policy says so (append_usage).
    match previous {
        Some(command) => StatusLine::Previous {
            command,
            usage: policy.status_line_usage.then(|| Box::new(line())),
        },
        None => StatusLine::Text(line().line(columns_of(env))),
    }
}

/// The session's live state and, under a wrapper, which session its claude runs.
fn record_live(
    input: &Value,
    env: &Env,
    paths: &Paths,
    now: Ms,
    account: &str,
    id: &str,
    parsed: Option<&PerWindow<Option<Sample>>>,
) {
    let cwd = input
        .at("workspace")
        .text("current_dir")
        .or_else(|| input.text("cwd"));
    let live = json!({
        "at": iso(now),
        "account": account,
        "model": input.at("model").text("id"),
        "effort": input.at("effort").text("level"),
        "cwd": cwd,
        "reading": live_reading(parsed),
    });
    let same = read_session_file(paths, id, "live.json").is_some_and(|before| {
        ["account", "model", "effort", "cwd"]
            .iter()
            .all(|key| before.at(key) == live.at(key))
            && compact(before.at("reading")) == compact(live.at("reading"))
            && now - ms_of(before.at("at")).unwrap_or(0) < 30_000
    });
    if !same {
        let _ = write_session_file(paths, id, "live.json", &live);
    }
    let Some(wrapper) = env.get("BATON_WRAPPER").filter(|w| valid_id(w)) else {
        return;
    };
    let file = wrapper_session_file(paths, wrapper);
    if read_json(&file).is_none_or(|mapped| mapped.text("session") != Some(id)) {
        let _ = write_json_atomic(
            &file,
            &json!({
                "session": id,
                "at": iso(now),
                "source": "statusline",
                "account": account,
                "pid": claude_pid(env),
            }),
        );
    }
}

/// The operator's own status line output with Baton's line added after it, on the same (last) line,
/// as far as the terminal is wide: `usage(room)` is Baton's line for the room that is left (it drops
/// its parts to fit), or nothing when there is none. Their output is otherwise left as it is.
pub fn append_usage(output: &str, usage: &dyn Fn(usize) -> String, columns: usize) -> String {
    let text = output.replace("\r\n", "\n");
    let mut lines: Vec<String> = text
        .trim_end_matches('\n')
        .split('\n')
        .map(str::to_owned)
        .collect();
    let last = lines.len() - 1;
    let ends = if text.ends_with('\n') { "\n" } else { "" };
    let theirs = if text.trim().is_empty() {
        String::new()
    } else {
        lines[last].clone()
    };
    let taken = width(&clean(&theirs)) + 2;
    if columns > 0 && columns <= taken {
        return output.to_owned();
    }
    let room = if columns > 0 { columns - taken } else { 0 };
    let ours = usage(room);
    if ours.is_empty() || (columns > 0 && width(&clean(&ours)) > room) {
        return output.to_owned();
    }
    // Their line may end in an open colour: close it before ours.
    let reset = if ours.contains('\x1b') { "\x1b[0m" } else { "" };
    if theirs.is_empty() {
        return format!("{ours}\n");
    }
    lines[last] = format!("{theirs}{reset}  {ours}");
    lines.join("\n") + ends
}

/// One account's bar in the status line.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Percent used; None when it was never measured.
    pub used: Option<f64>,
    /// The account's threshold.
    pub limit: f64,
    /// Whether it is this session's account.
    pub current: bool,
}

/// Every account together.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overall {
    /// How many accounts there are.
    pub count: usize,
    /// The mean use per window of the accounts that have a reading; None when none has.
    pub means: PerWindow<Option<f64>>,
    /// Per window, one cell per account that has that limit, in order; None when the cells are not
    /// known.
    pub cells: PerWindow<Option<Vec<Cell>>>,
}

/// Every account together: how many there are, the mean 5-hour and the mean weekly use of those that
/// have a reading (a window past its reset counts as 0 used), and per window one cell per account in
/// order (used None when it was never measured). An account whose plan has no 5-hour limit has no
/// cell in that window and no part in its mean: when no account has one, the window is empty and the
/// status line leaves it out. This session's account is read from `reading`, the one this very status
/// line run was given, so the overall figure is only as old as the other accounts' stored readings.
pub fn overall_usage(
    paths: &Paths,
    config: &Config,
    account: &str,
    reading: Option<&Reading>,
    now: Ms,
) -> Overall {
    let policy = effective_policy(config);
    let readings: Vec<Option<Reading>> = config
        .accounts
        .iter()
        .map(|a| {
            if a.id == account {
                reading.cloned()
            } else {
                read_reading(paths, &a.id)
            }
        })
        .collect();
    let mut overall = Overall {
        count: config.accounts.len(),
        ..Overall::default()
    };
    for window in WINDOWS {
        let cells: Vec<Cell> = config
            .accounts
            .iter()
            .zip(&readings)
            .filter(|(_, reading)| {
                !(window == Window::FiveHour && reading.as_ref().is_some_and(no_five_hour))
            })
            .map(|(a, reading)| {
                let limit = thresholds(a, &policy)[window];
                let stored = reading.as_ref().and_then(|r| r.window(window));
                let w = window_state(window, stored, None, limit, now, &policy);
                Cell {
                    used: if w.reset { Some(0.0) } else { w.used },
                    limit,
                    current: a.id == account,
                }
            })
            .collect();
        let known: Vec<f64> = cells.iter().filter_map(|c| c.used).collect();
        overall.means[window] = (!known.is_empty()).then(|| known.iter().sum::<f64>() / known.len() as f64);
        overall.cells[window] = Some(cells);
    }
    overall
}

/// A window's tag in the status line.
fn window_tag(window: Window) -> &'static str {
    match window {
        Window::FiveHour => "5h",
        Window::SevenDay => "week",
    }
}

/// The thresholds of an account that has none of its own.
pub const DEFAULT_LIMITS: PerWindow<f64> = PerWindow {
    five_hour: 90.0,
    seven_day: 95.0,
};

/// How [`own_status_line`] draws.
#[derive(Clone, Copy, Debug)]
pub struct LineOptions<'a> {
    /// The width to fit, in cells (0: as wide as it likes).
    pub columns: usize,
    /// Every account together; None leaves that part out.
    pub overall: Option<&'a Overall>,
    /// The account's thresholds.
    pub limits: PerWindow<f64>,
    /// The glyphs and colours; None is plain text with the rich glyphs.
    pub term: Option<Terminal>,
}

impl Default for LineOptions<'_> {
    fn default() -> Self {
        LineOptions {
            columns: 0,
            overall: None,
            limits: DEFAULT_LIMITS,
            term: None,
        }
    }
}

/// Baton's own status line (when the operator had none): the baton mark, the account and its two
/// windows, then all the accounts together, one little bar each (taller the fuller, coloured by its
/// threshold, this account's underlined) and their mean:
///
/// ```text
/// ╺━╸ acct2 · 5h 42% · week 63% ┃ 5h ▂▇▅▃ 31% · week ▃▅▇▅ 58%
/// ```
///
/// A plan with no 5-hour limit has no `5h` of its own, and when no account has one the overall part
/// has none either. A narrow terminal drops the overall part first, then the windows. `term` decides
/// the glyphs and colours (plain text when it is not styled).
pub fn own_status_line(
    account: Option<&str>,
    reading: Option<&Reading>,
    now: Ms,
    options: &LineOptions<'_>,
) -> String {
    let Some(account) = account.filter(|account| !account.is_empty()) else {
        return String::new();
    };
    let columns = options.columns;
    let term = options
        .term
        .unwrap_or_else(|| make_terminal(false, 1, GlyphSet::Rich, columns));
    let dot = s(" · ", "muted");
    let figure = |window: Window, used: f64| -> [Span; 2] {
        [
            s(format!("{} ", window_tag(window)), "muted"),
            s(
                format!("{}%", round(used) as i64),
                usage_tone(Some(used), options.limits[window]),
            ),
        ]
    };
    let head: Line = vec![s(term.g.baton, "brand bold"), p(" "), s(account, "bold")];
    let mut own: Line = Vec::new();
    // A plan with no 5-hour limit: what is left of an old reading of it says nothing.
    let no_five = reading.is_some_and(no_five_hour);
    for window in WINDOWS {
        if no_five && window == Window::FiveHour {
            continue;
        }
        let Some(w) = reading
            .and_then(|r| r.window(window))
            .filter(|w| w.used.is_finite())
        else {
            continue;
        };
        own.push(dot.clone());
        if w.resets_at
            .as_deref()
            .and_then(parse_ms)
            .is_some_and(|resets| resets <= now)
        {
            own.push(s(format!("{} ", window_tag(window)), "muted"));
            own.push(s("reset", "ok"));
        } else {
            own.extend(figure(window, w.used));
        }
    }
    // One bar per account: its use as a height, unknown as a dot; this account's, the live one,
    // underlined.
    let ticks = term.g.ticks;
    let bars = |cells: &[Cell]| -> Line {
        cells
            .iter()
            .map(|cell| match cell.used {
                None => s(
                    term.g.normal,
                    if cell.current { "muted underline" } else { "muted" },
                ),
                Some(used) => {
                    let top = ticks.len() - 1;
                    let level = (round(used.max(0.0) / 100.0 * top as f64) as usize).min(top);
                    let tone = usage_tone(Some(used), cell.limit);
                    s(
                        ticks[level],
                        &if cell.current {
                            format!("{tone} underline")
                        } else {
                            tone.to_owned()
                        },
                    )
                }
            })
            .collect()
    };
    let mut together: Line = Vec::new();
    if let Some(overall) = options.overall {
        // Only a window that another account has a reading for: otherwise the overall figure would
        // only repeat this account's own.
        let comparable = |window: Window| match &overall.cells[window] {
            Some(cells) => cells.iter().any(|c| !c.current && c.used.is_some()),
            None => true,
        };
        let shown: Vec<(Window, f64)> = WINDOWS
            .into_iter()
            .filter_map(|window| Some((window, overall.means[window].filter(|_| comparable(window))?)))
            .collect();
        if overall.count > 1 && !shown.is_empty() {
            together.push(s(format!(" {} ", term.g.limit), "muted"));
            for (i, (window, mean)) in shown.into_iter().enumerate() {
                if i > 0 {
                    together.push(dot.clone());
                }
                let [tag, value] = figure(window, mean);
                let cells = bars(overall.cells[window].as_deref().unwrap_or_default());
                together.push(tag);
                let any = !cells.is_empty();
                together.extend(cells);
                if any {
                    together.push(p(" "));
                }
                together.push(value);
            }
        }
    }
    let fits = |line: &Line| columns == 0 || line_width(line) <= columns;
    let with_own: Line = head.iter().chain(&own).cloned().collect();
    let full: Line = with_own.iter().chain(&together).cloned().collect();
    let line = [full, with_own]
        .into_iter()
        .find(|line| fits(line))
        .unwrap_or(head);
    render(&[line], &term)
}

/// The shell Claude Code itself runs a status line command in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    /// The shell's program.
    pub command: PathBuf,
    /// What comes before the command line it is given.
    flags: Vec<&'static str>,
}

impl Shell {
    /// The shell's arguments to run `command`.
    pub fn args(&self, command: &str) -> Vec<String> {
        self.flags
            .iter()
            .map(|flag| (*flag).to_owned())
            .chain([command.to_owned()])
            .collect()
    }
}

/// The shell Claude Code itself runs a status line command in: Git Bash on Windows when it is
/// installed (`CLAUDE_CODE_GIT_BASH_PATH`, or next to git on PATH, or the default install), else
/// PowerShell; sh elsewhere.
pub fn status_line_shell(env: &Env) -> Shell {
    status_line_shell_on(env, cfg!(windows))
}

/// [`status_line_shell`] for Windows or not.
pub fn status_line_shell_on(env: &Env, windows: bool) -> Shell {
    if !windows {
        return Shell {
            command: PathBuf::from("/bin/sh"),
            flags: vec!["-c"],
        };
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(bash) = env.some("CLAUDE_CODE_GIT_BASH_PATH") {
        candidates.push(PathBuf::from(bash));
    }
    let path = env.get("PATH").or_else(|| env.get("Path")).unwrap_or_default();
    for dir in path.split(';').filter(|dir| !dir.is_empty()) {
        let dir = dir.strip_prefix('"').unwrap_or(dir);
        let dir = Path::new(dir.strip_suffix('"').unwrap_or(dir));
        if dir.join("git.exe").exists() {
            candidates.push(resolve(dir.join("..").join("bin").join("bash.exe")));
            candidates.push(resolve(dir.join("..").join("..").join("bin").join("bash.exe")));
        }
    }
    for root in ["ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|name| env.some(name))
    {
        candidates.push(Path::new(root).join("Git").join("bin").join("bash.exe"));
    }
    match candidates.into_iter().find(|candidate| candidate.exists()) {
        Some(bash) => Shell {
            command: bash,
            flags: vec!["-c"],
        },
        None => Shell {
            command: PathBuf::from("powershell.exe"),
            flags: vec!["-NoProfile", "-NonInteractive", "-Command"],
        },
    }
}

/// What the operator's own status line command printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Previous {
    /// Its output.
    pub stdout: String,
    /// Its exit code; 1 when it could not be run or was ended.
    pub code: i32,
}

/// Runs the operator's previous status line command with the same stdin.
pub fn run_previous_status_line(command: &str, stdin_text: &str, env: &Env) -> Previous {
    let shell = status_line_shell(env);
    let mut run = Command::new(&shell.command);
    run.args(shell.args(command));
    env.apply(&mut run);
    let done = proc::run(run, Some(stdin_text), Duration::from_secs(15));
    Previous {
        stdout: done.stdout,
        code: done.status.unwrap_or(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pid_claude_code_names() {
        let pid = |value: &str| claude_pid(&Env::from_pairs([("CLAUDE_PID", value)]));
        assert_eq!(pid("4242"), Some(4242));
        assert_eq!(pid("0"), None);
        assert_eq!(pid("-3"), None);
        assert_eq!(pid("1.5"), None);
        assert_eq!(pid("abc"), None);
        assert_eq!(claude_pid(&Env::new()), None);
    }

    #[test]
    fn events_by_name() {
        assert_eq!(hook_event("stop-failure"), Some("StopFailure"));
        assert_eq!(hook_event("prompt"), Some("UserPromptSubmit"));
        assert_eq!(hook_event("Stop"), None);
        assert_eq!(HOOK_EVENTS.len(), 6);
    }

    #[test]
    fn texts_are_cut_by_utf16_units() {
        assert_eq!(clip(&json!("abcdef"), 4).as_deref(), Some("abcd"));
        assert_eq!(clip(&json!("abc"), 4).as_deref(), Some("abc"));
        // An astral character is two units and is never cut in half.
        assert_eq!(clip(&json!("ab\u{1F600}c"), 3).as_deref(), Some("ab"));
        assert_eq!(clip(&json!("ab\u{1F600}c"), 4).as_deref(), Some("ab\u{1F600}"));
        assert_eq!(clip(&json!(7), 4), None);
        assert_eq!(clip(&json!(null), 4), None);
    }

    #[test]
    fn background_tasks_are_summarised() {
        let tasks = json!([
            { "id": "1", "type": "subagent", "status": "running", "agent_type": "Explore" },
            { "type": 4, "name": "named", "agent_type": "ignored" },
            "odd"
        ]);
        assert_eq!(
            task_summary(&tasks),
            json!([
                { "type": "subagent", "status": "running", "name": "Explore" },
                { "type": null, "status": null, "name": "named" },
                { "type": null, "status": null, "name": null }
            ])
        );
        assert_eq!(task_summary(&json!("none")), json!([]));
        let many: Vec<Value> = (0..60).map(|_| json!({ "type": "shell" })).collect();
        assert_eq!(task_summary(&json!(many)).as_array().unwrap().len(), 50);
    }

    #[test]
    fn widths_from_the_environment() {
        let columns = |value: &str| columns_of(&Env::from_pairs([("COLUMNS", value)]));
        assert_eq!(columns("40"), 40);
        assert_eq!(columns("40.9"), 40);
        assert_eq!(columns(""), 0);
        assert_eq!(columns("wide"), 0);
        assert_eq!(columns("-5"), 0);
        assert_eq!(columns_of(&Env::new()), 0);
    }

    #[test]
    fn the_shell_of_a_status_line() {
        let sh = status_line_shell_on(&Env::new(), false);
        assert_eq!(sh.command, PathBuf::from("/bin/sh"));
        assert_eq!(sh.args("echo hi"), ["-c", "echo hi"]);
        // Windows without Git Bash anywhere: PowerShell.
        let bare = status_line_shell_on(&Env::from_pairs([("PATH", "")]), true);
        assert_eq!(bare.command, PathBuf::from("powershell.exe"));
        assert_eq!(bare.args("x"), ["-NoProfile", "-NonInteractive", "-Command", "x"]);
        // Git Bash next to git on PATH, and the path Claude Code is told to use.
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("Git").join("cmd");
        let bash = dir.path().join("Git").join("bin").join("bash.exe");
        std::fs::create_dir_all(&cmd).unwrap();
        std::fs::create_dir_all(bash.parent().unwrap()).unwrap();
        std::fs::write(cmd.join("git.exe"), "").unwrap();
        std::fs::write(&bash, "").unwrap();
        let on_path = Env::from_pairs([("PATH", format!("\"{}\"", cmd.display()))]);
        let found = status_line_shell_on(&on_path, true);
        assert_eq!(found.command, resolve(&bash));
        assert_eq!(found.args("x"), ["-c", "x"]);
        let told = Env::from_pairs([
            ("PATH", String::new()),
            ("CLAUDE_CODE_GIT_BASH_PATH", bash.display().to_string()),
        ]);
        assert_eq!(status_line_shell_on(&told, true).command, bash);
        let files = Env::from_pairs([
            ("PATH", String::new()),
            ("ProgramFiles", dir.path().display().to_string()),
        ]);
        assert_eq!(status_line_shell_on(&files, true).command, bash);
    }
}
