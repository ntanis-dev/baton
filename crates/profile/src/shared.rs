//! The files of `~/.claude` every profile shares, where they cannot be links (Windows without Developer
//! Mode): each profile holds a copy, and Baton keeps the copies and `~/.claude`'s own in step, so that
//! what is set under one account is there for all of them, as it is where the files are one.
//!
//! Each file is kept in step the way its content allows:
//!
//! - **`settings.json`, `keybindings.json`**: setting by setting, with the engine that keeps the
//!   profiles' `.claude.json` in step ([`crate::sync`]). A change made under an account goes to
//!   `~/.claude`'s file and to the other accounts; two that changed the same setting in different ways
//!   are a question for the operator, and nobody's value is written over until it is answered.
//! - **`CLAUDE.md`**: line by line against the text Baton last brought everywhere ([`crate::merge3`]):
//!   what each account changed is in the merged text. Two that changed the same lines are a question:
//!   whose text stays.
//! - **`history.jsonl`**: every participant's prompts in one history, each once, in the order they were
//!   typed. Nothing is ever taken out of it, so there is nothing to ask.
//!
//! `~/.claude`'s own file is written whenever anything is, and what it held before a sync changed it
//! is kept under `~/.baton/backups`. A file is replaced only while it is still what the sync read: one
//! a Claude Code saved meanwhile is left as it is, for the next sync.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use baton_core::clock::{Ms, iso};
use baton_core::config::Config;
use baton_core::fsutil::{Modes, append_text, read_json_as, write_bytes_atomic, write_json_atomic};
use baton_core::json::Get;
use baton_core::paths::{Paths, profile_dir};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::merge3::merge3;
use crate::profile::can_symlink_files;
use crate::sync::{
    Apply, Brought, Doc, HOME, Held, Question, SYNC_WAIT, SyncReport, Written, forget_in, full_sync,
    questions_in, sync_doc, sync_lock, sync_settings, take_in,
};

/// The files kept in step setting by setting.
const BY_SETTING: [&str; 2] = ["settings.json", "keybindings.json"];

/// The file kept in step line by line.
const BY_LINE: &str = "CLAUDE.md";

/// The file that is every participant's lines together.
const HISTORY: &str = "history.jsonl";

/// The files a profile holds a copy of where it cannot hold a link, in the order they are reported.
pub const COPIED: [&str; 4] = ["settings.json", "CLAUDE.md", "keybindings.json", HISTORY];

/// A history larger than this is left alone: reading it at every start would cost more than it gives.
const HISTORY_MAX: u64 = 64 * 1024 * 1024;

/// What a sync did with one shared file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FileReport {
    /// The file.
    pub file: &'static str,
    /// What was done.
    pub report: SyncReport,
}

/// What a sync of the shared files did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SharedReport {
    /// Whether another baton was syncing, so that nothing was done.
    pub busy: bool,
    /// One report per file some profile holds a copy of.
    pub files: Vec<FileReport>,
}

impl SharedReport {
    /// What was done with one file; None when no profile holds a copy of it.
    pub fn of(&self, file: &str) -> Option<&SyncReport> {
        self.files
            .iter()
            .find(|own| own.file == file)
            .map(|own| &own.report)
    }
}

/// Where Baton keeps what it knows of a shared file.
fn state_file(paths: &Paths, name: &str) -> PathBuf {
    paths.root.join("shared").join(format!("{name}.json"))
}

/// `~/.claude`'s own file: where a link the operator made of it points (dotfiles), else the file.
fn home_file(paths: &Paths, name: &str) -> PathBuf {
    let file = paths.claude_dir.join(name);
    match fs::symlink_metadata(&file) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(&file).unwrap_or(file),
        _ => file,
    }
}

/// Who holds the file `name`: `~/.claude` first, then every account whose profile has a copy of it,
/// or will get one because it has nothing there and links are not allowed.
fn holders(paths: &Paths, config: &Config, name: &str, links: bool) -> Vec<(String, PathBuf)> {
    let mut out = vec![(HOME.to_owned(), home_file(paths, name))];
    for account in &config.accounts {
        let profile = profile_dir(paths, &account.id);
        if !profile.is_dir() {
            continue;
        }
        let file = profile.join(name);
        let copy = match fs::symlink_metadata(&file) {
            Ok(meta) => meta.is_file(),
            Err(_) => !links,
        };
        if copy {
            out.push((account.id.clone(), file));
        }
    }
    out
}

/// A short mark of a text, to tell whether it changed.
fn mark(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Replaces `file` with `next` when it still holds `before` (None: when it is still not there).
/// Whether it was written.
fn replace_if_same(file: &Path, before: Option<&[u8]>, next: &[u8]) -> io::Result<bool> {
    if fs::read(file).ok().as_deref() != before {
        return Ok(false);
    }
    write_bytes_atomic(file, next, Modes::default())?;
    Ok(true)
}

/// Keeps what `~/.claude`'s file held before a sync changed it.
fn keep_previous(paths: &Paths, name: &str, file: &Path, before: Option<Vec<u8>>, now: Ms) -> io::Result<()> {
    let Some(before) = before else {
        return Ok(());
    };
    if fs::read(file).ok().as_deref() == Some(before.as_slice()) {
        return Ok(());
    }
    let folder = paths.backups.join(iso(now).replace([':', '.'], "-"));
    write_bytes_atomic(&folder.join(format!("sync-{name}")), &before, Modes::default())
}

/// Whether `which` has this participant written. `~/.claude`'s file is, whenever anything is.
fn wanted(which: Apply<'_>, id: &str) -> bool {
    match which {
        Apply::Nothing => false,
        Apply::One(one) => id == one || id == HOME,
        Apply::All => true,
    }
}

// ---- CLAUDE.md --------------------------------------------------------------------------------

/// What Baton knows of the file kept in step line by line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct TextState {
    /// The version of the file.
    #[serde(default)]
    version: u32,
    /// When it last changed.
    #[serde(default)]
    at: Option<String>,
    /// The text Baton last brought everywhere.
    #[serde(default)]
    base: Option<String>,
    /// Per participant, the mark of the text Baton last saw there.
    #[serde(default)]
    seen: BTreeMap<String, String>,
    /// The question that waits for the operator, when two changed the same lines.
    #[serde(default)]
    pending: Vec<Question>,
}

/// One participant's text, as it is on disk now.
struct Text {
    /// An account's id, or [`HOME`].
    id: String,
    /// Its file.
    file: PathBuf,
    /// The file's bytes; None when it is not there.
    bytes: Option<Vec<u8>>,
    /// The bytes as text; None when there is no file, or it is not text.
    text: Option<String>,
}

impl Text {
    /// Reads a participant's file.
    fn read(id: &str, file: &Path) -> Text {
        let bytes = fs::read(file).ok();
        let text = bytes.clone().and_then(|bytes| String::from_utf8(bytes).ok());
        Text {
            id: id.to_owned(),
            file: file.to_path_buf(),
            bytes,
            text,
        }
    }

    /// Whether the file is there but is not text: it is left alone.
    fn unreadable(&self) -> bool {
        self.bytes.is_some() && self.text.is_none()
    }
}

/// A text in a few words, for a question: its size and its mark, so that two texts never look alike.
fn summary(text: &str) -> Value {
    let lines = text.lines().count();
    json!(format!(
        "{lines} line{} ({})",
        if lines == 1 { "" } else { "s" },
        &mark(text)[..6]
    ))
}

/// Saves the text's state when something other than its date changed.
fn save_text(file: &Path, before: &TextState, state: &mut TextState, now: Ms) -> io::Result<()> {
    state.version = 1;
    state.at = before.at.clone();
    if before != state {
        state.at = Some(iso(now));
        write_json_atomic(file, state)?;
    }
    Ok(())
}

/// Brings the file kept in step line by line in step among its holders.
fn sync_text(
    name: &'static str,
    state_file: &Path,
    files: &[(String, PathBuf)],
    which: Apply<'_>,
    now: Ms,
) -> io::Result<SyncReport> {
    let before: TextState = read_json_as(state_file).unwrap_or_default();
    let mut state = before.clone();
    let mut report = SyncReport::default();
    let all: Vec<Text> = files.iter().map(|(id, file)| Text::read(id, file)).collect();
    state.seen.retain(|id, _| all.iter().any(|own| own.id == *id));
    let base = state.base.clone();
    let base_mark = base.as_deref().map(mark);
    let mut merged = base.clone();
    let mut differ = false;
    for own in &all {
        if own.unreadable() {
            report.unreadable.push(own.id.clone());
            continue;
        }
        let Some(text) = &own.text else {
            continue;
        };
        let last = state.seen.get(&own.id).or(base_mark.as_ref());
        if last == Some(&mark(text)) || merged.as_deref() == Some(text.as_str()) {
            continue; // not touched since Baton last looked, or what the others have by now
        }
        let next = match (&base, &merged) {
            (_, None) => Some(text.clone()),
            (Some(base), Some(so_far)) => merge3(base, so_far, text),
            // Two texts with nothing they were both made from.
            (None, Some(_)) => None,
        };
        let Some(next) = next else {
            differ = true;
            break;
        };
        merged = Some(next);
        report.brought.push(Brought {
            label: name.to_owned(),
            from: own.id.clone(),
        });
    }
    if differ {
        // Nobody's text is written over until the operator says whose stays.
        state.pending = vec![Question {
            leaf: name.to_owned(),
            label: name.to_owned(),
            values: all
                .iter()
                .filter_map(|own| {
                    Some(Held {
                        who: own.id.clone(),
                        value: Some(summary(own.text.as_deref()?)),
                    })
                })
                .collect(),
            at: before
                .pending
                .first()
                .and_then(|known| known.at.clone())
                .or_else(|| Some(iso(now))),
        }];
        report.brought.clear();
        report.questions = state.pending.clone();
        save_text(state_file, &before, &mut state, now)?;
        return Ok(report);
    }
    state.pending.clear();
    state.base = merged.clone();
    if let Some(merged) = merged {
        let agreed = mark(&merged);
        for own in all.iter().filter(|own| !own.unreadable()) {
            if own.text.as_deref() == Some(merged.as_str()) {
                state.seen.insert(own.id.clone(), agreed.clone());
            } else if wanted(which, &own.id) {
                // A file saved since it was read is left as it is: the next sync reads it anew.
                if replace_if_same(&own.file, own.bytes.as_deref(), merged.as_bytes())? {
                    state.seen.insert(own.id.clone(), agreed.clone());
                    report.written.push(Written {
                        account: own.id.clone(),
                        settings: 1,
                    });
                }
            } else if let Some(text) = &own.text {
                // What it holds is a leftover now: it takes the merged text when its turn comes.
                state.seen.insert(own.id.clone(), mark(text));
            }
        }
    }
    save_text(state_file, &before, &mut state, now)?;
    Ok(report)
}

/// Makes the text `who` holds the one every holder gets.
fn take_text(state_file: &Path, files: &[(String, PathBuf)], who: &str, now: Ms) -> io::Result<()> {
    let before: TextState = read_json_as(state_file).unwrap_or_default();
    let mut state = before.clone();
    let all: Vec<Text> = files.iter().map(|(id, file)| Text::read(id, file)).collect();
    let Some(chosen) = all
        .iter()
        .find(|own| own.id == who)
        .and_then(|own| own.text.clone())
    else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{who} has no {BY_LINE} to take"),
        ));
    };
    state.base = Some(chosen);
    // What everyone holds now is a leftover: it takes the answer when its turn comes.
    for own in &all {
        if let Some(text) = &own.text {
            state.seen.insert(own.id.clone(), mark(text));
        }
    }
    state.pending.clear();
    save_text(state_file, &before, &mut state, now)
}

// ---- history.jsonl ----------------------------------------------------------------------------

/// The lines of one participant's history, as they are on disk now.
struct History {
    /// An account's id, or [`HOME`].
    id: String,
    /// Its file.
    file: PathBuf,
    /// The file's bytes; None when it is not there.
    bytes: Option<Vec<u8>>,
    /// Its lines; None when the file cannot be read, is not text, or is too large.
    lines: Option<Vec<String>>,
}

impl History {
    /// Reads a participant's history.
    fn read(id: &str, file: &Path) -> History {
        let small = fs::metadata(file).map_or(true, |meta| meta.len() <= HISTORY_MAX);
        let bytes = if small { fs::read(file).ok() } else { None };
        let missing = !file.exists();
        let lines = match &bytes {
            Some(bytes) => std::str::from_utf8(bytes).ok().map(|text| {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_owned)
                    .collect()
            }),
            None if missing => Some(Vec::new()),
            None => None,
        };
        History {
            id: id.to_owned(),
            file: file.to_path_buf(),
            bytes,
            lines,
        }
    }
}

/// When a line of the history was typed, and what tells it from every other: its time, its
/// conversation and its text. A line that is not what Claude Code writes is its own key, at the time
/// of the line before it.
fn history_key(line: &str, previous: f64) -> (f64, String) {
    let Some(entry) = baton_core::json::parse(line).filter(Value::is_object) else {
        return (previous, line.to_owned());
    };
    let Some(at) = entry.number("timestamp") else {
        return (previous, line.to_owned());
    };
    let key = format!(
        "{at}\u{1f}{}\u{1f}{}",
        entry.text("sessionId").unwrap_or_default(),
        entry.text("display").unwrap_or_default()
    );
    (at, key)
}

/// Makes every holder's history all of their lines, each once, in the order they were typed.
fn sync_history(files: &[(String, PathBuf)], which: Apply<'_>) -> io::Result<SyncReport> {
    let mut report = SyncReport::default();
    let all: Vec<History> = files.iter().map(|(id, file)| History::read(id, file)).collect();
    let mut known: HashSet<String> = HashSet::new();
    let mut merged: Vec<(f64, String)> = Vec::new();
    for own in &all {
        let Some(lines) = &own.lines else {
            report.unreadable.push(own.id.clone());
            continue;
        };
        let mut previous = 0.0;
        for line in lines {
            let (at, key) = history_key(line, previous);
            previous = at;
            if known.insert(key) {
                merged.push((at, line.clone()));
            }
        }
    }
    // Lines of one moment stay in the order they were met in.
    merged.sort_by(|a, b| a.0.total_cmp(&b.0));
    let merged: Vec<String> = merged.into_iter().map(|(_, line)| line).collect();
    for own in &all {
        let Some(lines) = &own.lines else {
            continue;
        };
        if *lines == merged || !wanted(which, &own.id) {
            continue;
        }
        let more = merged.len() - lines.len().min(merged.len());
        let done = if merged.starts_with(lines) {
            // Only later lines are missing: they are added, and what is there is never rewritten
            // under a Claude Code that may be adding its own.
            let ended = own
                .bytes
                .as_deref()
                .is_none_or(|bytes| bytes.is_empty() || bytes.ends_with(b"\n"));
            let mut text = if ended { String::new() } else { "\n".to_owned() };
            for line in &merged[lines.len()..] {
                text.push_str(line);
                text.push('\n');
            }
            append_text(&own.file, &text)?;
            true
        } else {
            let text: String = merged.iter().flat_map(|line| [line.as_str(), "\n"]).collect();
            replace_if_same(&own.file, own.bytes.as_deref(), text.as_bytes())?
        };
        if done {
            report.written.push(Written {
                account: own.id.clone(),
                settings: more.max(1),
            });
        }
    }
    Ok(report)
}

// ---- All of them ------------------------------------------------------------------------------

/// Brings the shared files in step among `~/.claude` and the profiles that hold copies of them, and
/// writes the profiles `which` names (and `~/.claude`'s own). `links` says whether file links are
/// allowed here (None asks the system): where they are, a profile with nothing of a file gets a link
/// when it is linked, and takes no part here.
pub fn sync_shared(
    paths: &Paths,
    config: &Config,
    which: Apply<'_>,
    now: Ms,
    links: Option<bool>,
) -> io::Result<SharedReport> {
    let Some(_lock) = sync_lock(paths, SYNC_WAIT) else {
        return Ok(SharedReport {
            busy: true,
            files: Vec::new(),
        });
    };
    let links = links.unwrap_or_else(|| can_symlink_files(&paths.tmp));
    let mut out = SharedReport::default();
    for name in COPIED {
        let files = holders(paths, config, name, links);
        // Nobody holds a copy: the profiles hold links, or there are none.
        if files.len() < 2 {
            continue;
        }
        let home = files[0].1.clone();
        let previous = if name == HISTORY {
            None
        } else {
            fs::read(&home).ok()
        };
        let report = if BY_SETTING.contains(&name) {
            sync_doc(&Doc::copied(name, state_file(paths, name), files), which, now)?
        } else if name == BY_LINE {
            sync_text(name, &state_file(paths, name), &files, which, now)?
        } else {
            sync_history(&files, which)?
        };
        keep_previous(paths, name, &home, previous, now)?;
        out.files.push(FileReport { file: name, report });
    }
    Ok(out)
}

/// The questions about the shared files, as their last sync left them.
pub fn shared_questions(paths: &Paths) -> Vec<Question> {
    let mut out: Vec<Question> = BY_SETTING
        .iter()
        .flat_map(|name| questions_in(&state_file(paths, name)))
        .collect();
    out.extend(
        read_json_as::<TextState>(&state_file(paths, BY_LINE))
            .map(|state| state.pending)
            .unwrap_or_default(),
    );
    out
}

/// Forgets what Baton saw of a removed account in the shared files.
pub fn forget_shared(paths: &Paths, id: &str) -> io::Result<()> {
    let Some(_lock) = sync_lock(paths, SYNC_WAIT) else {
        return Ok(());
    };
    for name in BY_SETTING {
        forget_in(&state_file(paths, name), id)?;
    }
    let file = state_file(paths, BY_LINE);
    if let Some(mut state) = read_json_as::<TextState>(&file)
        && state.seen.remove(id).is_some()
    {
        for question in &mut state.pending {
            question.values.retain(|held| held.who != id);
        }
        write_json_atomic(&file, &state)?;
    }
    Ok(())
}

/// One report of several: what each brought and wrote, every question, in the order given.
fn together(reports: impl IntoIterator<Item = SyncReport>) -> SyncReport {
    let mut out = SyncReport::default();
    for report in reports {
        out.brought.extend(report.brought);
        for written in report.written {
            match out.written.iter_mut().find(|own| own.account == written.account) {
                Some(own) => own.settings += written.settings,
                None => out.written.push(written),
            }
        }
        out.questions.extend(report.questions);
        for id in report.reset {
            if !out.reset.contains(&id) {
                out.reset.push(id);
            }
        }
        for id in report.unreadable {
            if !out.unreadable.contains(&id) {
                out.unreadable.push(id);
            }
        }
        out.busy |= report.busy;
    }
    out
}

/// Brings everything the accounts share in step: the shared files where profiles hold copies of
/// them, and, with full sync on, what the operator set in the profiles' `.claude.json`. One report for
/// all of it; what it names in a shared file says which file.
pub fn sync_everything(paths: &Paths, config: &Config, which: Apply<'_>, now: Ms) -> io::Result<SyncReport> {
    let shared = sync_shared(paths, config, which, now, None)?;
    let mut reports: Vec<SyncReport> = vec![SyncReport {
        busy: shared.busy,
        ..SyncReport::default()
    }];
    reports.extend(shared.files.into_iter().map(|own| {
        let mut report = own.report;
        // What is said to be written is what an account got; `~/.claude`'s own goes without saying.
        report.written.retain(|written| written.account != HOME);
        report
    }));
    if full_sync(config) {
        reports.push(sync_settings(paths, config, which, now)?);
    }
    Ok(together(reports))
}

/// Every question that waits for the operator: about the shared files, and about `.claude.json`.
pub fn every_question(paths: &Paths, config: &Config) -> Vec<Question> {
    let mut out = shared_questions(paths);
    if full_sync(config) {
        out.extend(crate::sync::read_questions(paths));
    }
    out
}

/// Answers questions, whatever file each is about: the value `who` holds (an account's id, or
/// [`HOME`]) becomes the one everybody has. Then everything is brought in step.
pub fn take_everything(
    paths: &Paths,
    config: &Config,
    asked: &[String],
    who: &str,
    now: Ms,
) -> io::Result<SyncReport> {
    let links = can_symlink_files(&paths.tmp);
    {
        let Some(_lock) = sync_lock(paths, SYNC_WAIT) else {
            return Ok(SyncReport {
                busy: true,
                ..SyncReport::default()
            });
        };
        let mut rest: Vec<String> = Vec::new();
        for question in asked {
            if question == BY_LINE {
                let files = holders(paths, config, BY_LINE, links);
                take_text(&state_file(paths, BY_LINE), &files, who, now)?;
            } else if let Some(name) = BY_SETTING.iter().find(|name| question.starts_with(**name)) {
                let files = holders(paths, config, name, links);
                let doc = Doc::copied(name, state_file(paths, name), files);
                take_in(&doc, std::slice::from_ref(question), who, now)?;
            } else {
                rest.push(question.clone());
            }
        }
        if !rest.is_empty() {
            take_in(&Doc::claude_json(paths, config), &rest, who, now)?;
        }
    }
    sync_everything(paths, config, Apply::All, now)
}

/// How a profile's copy of a shared file stands: `no-source` (`~/.claude` has none), `missing`,
/// `copy-fresh` (the same as `~/.claude`'s) or `copy-changed` (it differs: it is brought in step the
/// next time the account is used, or waits for an answer).
pub fn copy_state(paths: &Paths, name: &str, item: &str) -> &'static str {
    let Ok(source) = fs::read(home_file(paths, item)) else {
        return "no-source";
    };
    // Settings are the same when they say the same, however each file is laid out.
    let said = |bytes: &[u8]| {
        BY_SETTING
            .contains(&item)
            .then(|| baton_core::json::parse(std::str::from_utf8(bytes).ok()?))
            .flatten()
    };
    match fs::read(profile_dir(paths, name).join(item)) {
        Err(_) => "missing",
        Ok(copy) if copy == source => "copy-fresh",
        Ok(copy) if said(&copy).is_some_and(|own| Some(own) == said(&source)) => "copy-fresh",
        Ok(_) => "copy-changed",
    }
}
