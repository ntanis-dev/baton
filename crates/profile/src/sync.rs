//! Full sync: what the operator sets in Claude Code's `.claude.json` is the same on every account.
//!
//! A profile's `.claude.json` is its own, because it holds the account's identity. But it also holds
//! what the operator set up: the theme and other preferences, the MCP servers, and for every folder its
//! trust, its allowed tools and its MCP approvals. With the policy's `fullSync` on (the default), those
//! are kept in step between the accounts, in both directions:
//!
//! - **What is kept in step.** The keys Baton knows as the operator's own settings
//!   ([`SEED_KEYS`], [`SEED_PROJECT_KEYS`]), and any the operator adds in `config.json` (`syncKeys`,
//!   `syncProjectKeys`). Everything else in the file (the identity first, then Claude Code's caches and
//!   counters) stays per account. Credentials are never read.
//! - **Who takes part.** Every account, and `~/.claude.json`, which is only ever read: what changes
//!   there reaches the accounts, never the other way round.
//! - **How a change is told from a leftover.** `~/.baton/sync.json` keeps what the accounts agree on and,
//!   for every participant, what Baton last saw there. A value that differs from what was last seen is
//!   a change made there, and it goes to the others; a value that was only left behind takes the
//!   agreed one. A lost entry of a list or of a map of servers is a removal, and goes to the others too.
//! - **What one account cannot take back.** A grant: a `has...` flag (a folder's trust, a notice
//!   already answered), an item of an `allowed...` list, the count of a folder's entry. An account
//!   that lost one (its onboarding starting over, say) gets it back from the others; it is gone only
//!   when no participant holds it any more.
//! - **When two differ.** Counters and versions take the higher one; a grant stays granted. Anything
//!   else is a question for the operator: nobody's value is written over until it is answered
//!   ([`take`]).
//! - **When.** Whenever a profile is linked: an account added or signed in, `baton` starting, and a
//!   conversation moving to another account, so what was set on the way is there on arrival. Only the
//!   profile being linked is written; the others catch up when their turn comes.
//!
//! A participant that suddenly lacks most of what it had is taken as reset (Claude Code starting its
//! file afresh), not as the operator removing everything: it gets the agreed settings back.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use baton_core::clock::{Ms, iso};
use baton_core::config::{Config, effective_policy};
use baton_core::fsutil::{read_json_as, write_json_atomic};
use baton_core::json::{Object, compact};
use baton_core::oslock::OsLock;
use baton_core::paths::{Paths, global_config_file};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::claude_json::{Loaded, load, rewrite, rewrite_with};
use crate::profile::{SEED_KEYS, SEED_PROJECT_KEYS, normalize_project_key};

/// The name `~/.claude.json` takes part under (never an account's id).
pub const HOME: &str = "~";

/// What is never kept in step, whatever the config says: who the account is, and the folders' entries
/// as a whole (their settings are kept in step key by key).
const NEVER: [&str; 4] = ["oauthAccount", "userID", "machineID", "projects"];

/// How long a sync waits for another baton to finish its own.
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// Whether full sync is on.
pub fn full_sync(config: &Config) -> bool {
    effective_policy(config).full_sync
}

/// `~/.baton/sync.json`.
pub fn sync_file(paths: &Paths) -> PathBuf {
    paths.root.join("sync.json")
}

/// The keys that are kept in step.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Keys {
    /// Of the file itself.
    top: Vec<String>,
    /// Of a folder's entry.
    project: Vec<String>,
}

/// The keys kept in step: Baton's own lists and what the operator added.
fn keys_of(config: &Config) -> Keys {
    let join = |own: &[&str], added: &[String]| {
        let mut out: Vec<String> = own.iter().map(|key| (*key).to_owned()).collect();
        for key in added {
            if !NEVER.contains(&key.as_str()) && !out.contains(key) {
                out.push(key.clone());
            }
        }
        out
    };
    Keys {
        top: join(&SEED_KEYS, &config.sync_keys),
        project: join(&SEED_PROJECT_KEYS, &config.sync_project_keys),
    }
}

/// A file that is kept in step leaf by leaf: every profile's `.claude.json`, or a file of `~/.claude`
/// that the profiles hold copies of ([`crate::shared`]).
#[derive(Clone, Debug)]
pub(crate) struct Doc {
    /// Its name: `.claude.json`, `settings.json`, `keybindings.json`.
    pub(crate) name: &'static str,
    /// Where Baton keeps what it knows of it.
    pub(crate) state: PathBuf,
    /// The keys that take part; None for every key of the file.
    keys: Option<Keys>,
    /// The participants and their files, [`HOME`] first.
    pub(crate) files: Vec<(String, PathBuf)>,
    /// Whether it is `.claude.json`: the home's is only read, a grant stays granted, and a participant
    /// that lost most of what it had was reset. A copied file is one file of the operator's, written
    /// everywhere, where what was taken out was taken out.
    claude_json: bool,
}

impl Doc {
    /// Every profile's `.claude.json`, with `~/.claude.json`.
    pub(crate) fn claude_json(paths: &Paths, config: &Config) -> Doc {
        let mut files = vec![(HOME.to_owned(), paths.claude_json.clone())];
        for account in &config.accounts {
            files.push((account.id.clone(), global_config_file(paths, &account.id)));
        }
        Doc {
            name: ".claude.json",
            state: sync_file(paths),
            keys: Some(keys_of(config)),
            files,
            claude_json: true,
        }
    }

    /// A file of `~/.claude` that these participants hold copies of.
    pub(crate) fn copied(name: &'static str, state: PathBuf, files: Vec<(String, PathBuf)>) -> Doc {
        Doc {
            name,
            state,
            keys: None,
            files,
            claude_json: false,
        }
    }

    /// The name a leaf of this file has in a question: the leaf's own for `.claude.json`, with the
    /// file's name before it for any other.
    fn asked(&self, leaf: &str) -> String {
        if self.claude_json {
            leaf.to_owned()
        } else {
            format!("{}{leaf}", self.name)
        }
    }

    /// The leaf a question's name stands for, when the question is about this file.
    fn leaf_of<'a>(&self, asked: &'a str) -> Option<&'a str> {
        if self.claude_json {
            asked.starts_with('[').then_some(asked)
        } else {
            asked.strip_prefix(self.name).filter(|leaf| leaf.starts_with('['))
        }
    }

    /// A leaf in words, with the file's name when it is not `.claude.json`.
    fn label(&self, leaf: &Leaf, spellings: &BTreeMap<String, String>) -> String {
        let own = leaf.label(spellings);
        if self.claude_json {
            own
        } else {
            format!("{} · {own}", self.name)
        }
    }
}

/// One thing that is kept in step: a key's value, one entry of a key's map, or one item of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Leaf {
    /// The folder it belongs to, as [`normalize_project_key`] writes it; None for the file itself.
    project: Option<String>,
    /// The key.
    key: String,
    /// The entry of the key's map.
    entry: Option<String>,
    /// The item of the list (the key's own, or the entry's), as canonical JSON.
    item: Option<String>,
}

impl Leaf {
    /// The leaf's name in `sync.json`.
    fn id(&self) -> String {
        compact(&(&self.project, &self.key, &self.entry, &self.item))
    }

    /// The leaf of a name.
    fn parse(id: &str) -> Option<Leaf> {
        let (project, key, entry, item) = serde_json::from_str(id).ok()?;
        Some(Leaf {
            project,
            key,
            entry,
            item,
        })
    }

    /// Whether it records something granted or answered, which stays so once it is: everything of a
    /// folder's entry, and the `has...` flags.
    fn grants(&self) -> bool {
        self.project.is_some() || self.key.starts_with("has")
    }

    /// Whether it is granted for good as far as one participant goes: a `has...` flag, or an item of an
    /// `allowed...` list. It is taken back only when nobody holds it any more.
    fn one_way(&self) -> bool {
        self.key.starts_with("has") || (self.item.is_some() && self.key.starts_with("allowed"))
    }

    /// The leaf in words for the operator: `theme`, `mcpServers: github`, `E:/work · allowedTools`.
    fn label(&self, spellings: &BTreeMap<String, String>) -> String {
        let mut out = String::new();
        if let Some(folder) = &self.project {
            out.push_str(spellings.get(folder).unwrap_or(folder));
            out.push_str(" · ");
        }
        out.push_str(&self.key);
        if let Some(entry) = &self.entry {
            out.push_str(": ");
            out.push_str(entry);
        }
        if let Some(item) = &self.item {
            out.push_str(": ");
            out.push_str(item);
        }
        out
    }
}

/// A value with its maps in key order, as one line: two values are the same when this is.
fn canon(value: &Value) -> String {
    /// The value with every map sorted.
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key.clone(), sorted(value)))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    compact(&sorted(value))
}

/// A short mark of a value, to tell whether it changed.
fn mark(value: &Value) -> String {
    Sha256::digest(canon(value).as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether a value is what Claude Code writes before anything was set: nothing, false, 0, no text.
fn is_stock(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(on) => !on,
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::String(text) => text.is_empty(),
        Value::Array(_) | Value::Object(_) => false,
    }
}

/// The leaves of a config, by name.
type Leaves = BTreeMap<String, Value>;

/// Adds the leaves of one key's value. With `grants`, a grant that was never given is no value.
fn flatten_key(out: &mut Leaves, project: Option<&str>, key: &str, value: &Value, grants: bool) {
    let leaf = |entry: Option<&str>, item: Option<&Value>| Leaf {
        project: project.map(str::to_owned),
        key: key.to_owned(),
        entry: entry.map(str::to_owned),
        item: item.map(canon),
    };
    match value {
        Value::Null => {}
        Value::Object(entries) => {
            for (name, entry) in entries {
                match entry {
                    Value::Null => {}
                    Value::Array(items) => {
                        for item in items {
                            out.insert(leaf(Some(name), Some(item)).id(), item.clone());
                        }
                    }
                    other => {
                        out.insert(leaf(Some(name), None).id(), other.clone());
                    }
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                out.insert(leaf(None, Some(item)).id(), item.clone());
            }
        }
        scalar => {
            let own = leaf(None, None);
            // A grant that was never given is no value at all.
            if !(grants && own.grants() && is_stock(scalar)) {
                out.insert(own.id(), scalar.clone());
            }
        }
    }
}

/// The leaves of a file, and how it spells each folder.
fn flatten(config: &Object, doc: &Doc) -> (Leaves, BTreeMap<String, String>) {
    let mut out = Leaves::new();
    let mut spellings = BTreeMap::new();
    let Some(keys) = &doc.keys else {
        for (key, value) in config {
            flatten_key(&mut out, None, key, value, false);
        }
        return (out, spellings);
    };
    for key in &keys.top {
        if let Some(value) = config.get(key) {
            flatten_key(&mut out, None, key, value, true);
        }
    }
    if let Some(Value::Object(folders)) = config.get("projects") {
        for (path, entry) in folders {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let folder = normalize_project_key(path);
            spellings.entry(folder.clone()).or_insert_with(|| path.clone());
            for key in &keys.project {
                if let Some(value) = entry.get(key) {
                    flatten_key(&mut out, Some(&folder), key, value, true);
                }
            }
        }
    }
    (out, spellings)
}

/// The map at `key`, made when it is not there (or is something else).
fn ensure_object<'a>(map: &'a mut Object, key: &str) -> Option<&'a mut Object> {
    let slot = map.entry(key.to_owned()).or_insert(Value::Null);
    if !slot.is_object() {
        *slot = Value::Object(Object::new());
    }
    slot.as_object_mut()
}

/// Adds an item to the list at `key`, or takes it out.
fn edit_list(root: &mut Object, key: &str, item: &str, value: Option<&Value>) {
    if value.is_none() && !root.get(key).is_some_and(Value::is_array) {
        return;
    }
    let slot = root.entry(key.to_owned()).or_insert(Value::Null);
    if !slot.is_array() {
        *slot = Value::Array(Vec::new());
    }
    let Some(items) = slot.as_array_mut() else {
        return;
    };
    let at = items.iter().position(|own| canon(own) == item);
    match (value, at) {
        (Some(value), None) => items.push(value.clone()),
        (None, Some(at)) => {
            items.remove(at);
        }
        _ => {}
    }
}

/// Sets one leaf of a `.claude.json` to `value`, or takes it out. Nothing else of the file is touched,
/// and the order of what stays is kept.
fn edit(config: &mut Object, leaf: &Leaf, value: Option<&Value>, spellings: &BTreeMap<String, String>) {
    let root = match &leaf.project {
        None => config,
        Some(folder) => {
            let own = config
                .get("projects")
                .and_then(Value::as_object)
                .and_then(|folders| folders.keys().find(|path| normalize_project_key(path) == *folder))
                .cloned();
            if own.is_none() && value.is_none() {
                return;
            }
            let spelling = own.unwrap_or_else(|| spellings.get(folder).unwrap_or(folder).clone());
            let Some(entry) =
                ensure_object(config, "projects").and_then(|folders| ensure_object(folders, &spelling))
            else {
                return;
            };
            entry
        }
    };
    match (&leaf.entry, &leaf.item) {
        (None, None) => match value {
            Some(value) => {
                root.insert(leaf.key.clone(), value.clone());
            }
            None => {
                root.shift_remove(&leaf.key);
            }
        },
        (None, Some(item)) => edit_list(root, &leaf.key, item, value),
        (Some(entry), item) => {
            if value.is_none() && !root.get(&leaf.key).is_some_and(Value::is_object) {
                return;
            }
            let Some(map) = ensure_object(root, &leaf.key) else {
                return;
            };
            match (item, value) {
                (Some(item), _) => edit_list(map, entry, item, value),
                (None, Some(value)) => {
                    map.insert(entry.clone(), value.clone());
                }
                (None, None) => {
                    map.shift_remove(entry);
                }
            }
        }
    }
}

/// What one participant holds for a leaf the accounts differ on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Held {
    /// The participant: an account's id, or [`HOME`].
    pub who: String,
    /// Its value; None when it has none.
    #[serde(default)]
    pub value: Option<Value>,
}

/// A setting two participants changed in different ways: nobody's value is written over until the
/// operator says whose stays.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Question {
    /// The setting's name in `sync.json`: what [`take`] is given.
    pub leaf: String,
    /// The setting in words.
    pub label: String,
    /// What every participant holds.
    #[serde(default)]
    pub values: Vec<Held>,
    /// When it was first noticed.
    #[serde(default)]
    pub at: Option<String>,
}

/// `~/.baton/sync.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct State {
    /// The version of the file.
    #[serde(default)]
    version: u32,
    /// When it last changed.
    #[serde(default)]
    at: Option<String>,
    /// What the accounts agree on, by leaf.
    #[serde(default)]
    base: BTreeMap<String, Value>,
    /// How a folder is spelled, by its normalised form.
    #[serde(default)]
    spellings: BTreeMap<String, String>,
    /// Per participant, the mark of what Baton last saw there, by leaf.
    #[serde(default)]
    seen: BTreeMap<String, BTreeMap<String, String>>,
    /// The questions that wait for the operator.
    #[serde(default)]
    pending: Vec<Question>,
}

/// The lock a sync holds: one baton at a time reads and writes `sync.json`. Waits up to `wait` for
/// another baton; None when it still syncs then. The lock is the operating system's, so a baton that
/// died holds none, and one that takes long keeps its own.
fn lock(paths: &Paths, wait: Duration) -> Option<OsLock> {
    OsLock::take(&paths.run.join("sync.lock"), wait).ok().flatten()
}

/// One participant, as it is on disk now.
#[derive(Debug)]
struct Party {
    /// An account's id, or [`HOME`].
    id: String,
    /// Its file.
    file: PathBuf,
    /// The file's content; None when there is no file yet.
    config: Option<Object>,
    /// Whether the file is there but cannot be read: it is left alone.
    unreadable: bool,
    /// Its leaves.
    leaves: Leaves,
}

impl Party {
    /// Whether a file read now is what this participant was read as.
    fn is(&self, loaded: &Loaded) -> bool {
        match (loaded, &self.config) {
            (Loaded::Missing, None) => !self.unreadable,
            (Loaded::Json(Value::Object(now)), Some(then)) => now == then,
            _ => false,
        }
    }
}

/// Reads a participant.
fn party(id: &str, file: &Path, doc: &Doc, spellings: &mut BTreeMap<String, String>) -> Party {
    let (config, unreadable) = match load(file) {
        Loaded::Json(Value::Object(config)) => (Some(config), false),
        Loaded::Missing => (None, false),
        Loaded::Json(_) | Loaded::Unreadable => (None, true),
    };
    let mut leaves = Leaves::new();
    if let Some(config) = &config {
        let (own, spelled) = flatten(config, doc);
        leaves = own;
        for (folder, path) in spelled {
            spellings.entry(folder).or_insert(path);
        }
    }
    Party {
        id: id.to_owned(),
        file: file.to_path_buf(),
        config,
        unreadable,
        leaves,
    }
}

/// A text of digits and dots as numbers, to tell the later of two versions.
fn version(value: &Value) -> Option<Vec<u64>> {
    let text = value.as_str()?;
    if !text.contains('.') {
        return None;
    }
    text.split('.').map(|part| part.parse().ok()).collect()
}

/// Whose value stays when a participant and the others both changed a leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    /// The participant's.
    Theirs,
    /// What the accounts agreed on meanwhile.
    Agreed,
}

/// Settles what needs no question: something stays over nothing, the higher counter, the later
/// version. None when only the operator can say.
fn settle(theirs: Option<&Value>, agreed: Option<&Value>) -> Option<Side> {
    let (theirs, agreed) = match (theirs, agreed) {
        (Some(theirs), Some(agreed)) => (theirs, agreed),
        (Some(_), None) => return Some(Side::Theirs),
        (None, _) => return Some(Side::Agreed),
    };
    if let (Some(a), Some(b)) = (theirs.as_f64(), agreed.as_f64()) {
        return Some(if a > b { Side::Theirs } else { Side::Agreed });
    }
    if let (Some(a), Some(b)) = (version(theirs), version(agreed)) {
        return Some(if a > b { Side::Theirs } else { Side::Agreed });
    }
    None
}

/// Whether a change one participant made takes back something granted: a grant that is gone there
/// ([`Leaf::one_way`]), or the count of a folder's entry that went down.
fn takes_back(id: &str, theirs: Option<&Value>, agreed: Option<&Value>) -> bool {
    let Some(leaf) = Leaf::parse(id) else {
        return false;
    };
    match (theirs, agreed) {
        (None, Some(_)) => leaf.one_way(),
        (Some(theirs), Some(agreed)) => {
            leaf.grants() && matches!((theirs.as_f64(), agreed.as_f64()), (Some(a), Some(b)) if a < b)
        }
        _ => false,
    }
}

/// Which profiles a sync writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Apply<'a> {
    /// None: only look at what changed.
    Nothing,
    /// The profile of this account: the one about to be used.
    One(&'a str),
    /// Every account's.
    All,
}

/// A change one participant made that the others now agree on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Brought {
    /// The setting in words.
    pub label: String,
    /// Who changed it: an account's id, or [`HOME`].
    pub from: String,
}

/// A profile whose `.claude.json` a sync changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Written {
    /// The account's id.
    pub account: String,
    /// How many settings were set or taken out.
    pub settings: usize,
}

/// What a sync did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SyncReport {
    /// The changes the accounts now agree on.
    pub brought: Vec<Brought>,
    /// The profiles that were written.
    pub written: Vec<Written>,
    /// The settings that wait for the operator.
    pub questions: Vec<Question>,
    /// The participants taken as reset: they lacked most of what they had.
    pub reset: Vec<String>,
    /// The accounts whose `.claude.json` is there but cannot be read: left alone.
    pub unreadable: Vec<String>,
    /// Whether another baton was syncing, so that nothing was done.
    pub busy: bool,
}

/// Folds what one participant changed into what the accounts agree on. Returns the leaves on which it
/// differs from the others in a way only the operator can settle.
fn collect(state: &mut State, party: &Party, report: &mut SyncReport, doc: &Doc) -> BTreeSet<String> {
    let mut blocked = BTreeSet::new();
    if party.config.is_none() {
        return blocked;
    }
    let mut seen = state.seen.remove(&party.id).unwrap_or_default();
    // A participant that lacks most of what it had was reset, not emptied on purpose.
    let gone = seen
        .keys()
        .filter(|leaf| !party.leaves.contains_key(*leaf))
        .count();
    if doc.claude_json && gone >= 5 && gone * 2 > seen.len() {
        seen.clear();
        report.reset.push(party.id.clone());
    }
    let ids: BTreeSet<&String> = party.leaves.keys().chain(seen.keys()).collect();
    let mut next = seen.clone();
    for id in ids {
        let theirs = party.leaves.get(id);
        let agreed = state.base.get(id);
        let (theirs_mark, agreed_mark) = (theirs.map(mark), agreed.map(mark));
        let last = seen.get(id);
        if theirs_mark.as_ref() == last {
            continue; // not touched since Baton last looked
        }
        let remember = |next: &mut BTreeMap<String, String>| match &theirs_mark {
            Some(mark) => {
                next.insert(id.clone(), mark.clone());
            }
            None => {
                next.remove(id);
            }
        };
        if theirs_mark == agreed_mark {
            remember(&mut next);
            continue;
        }
        // The others did not move meanwhile: the change stands, unless it takes back a grant, which
        // no participant does alone. Otherwise both moved.
        let side = if last == agreed_mark.as_ref() {
            Some(if doc.claude_json && takes_back(id, theirs, agreed) {
                Side::Agreed
            } else {
                Side::Theirs
            })
        } else {
            settle(theirs, agreed)
        };
        match side {
            Some(Side::Theirs) => {
                match theirs {
                    Some(value) => {
                        state.base.insert(id.clone(), value.clone());
                    }
                    None => {
                        state.base.remove(id);
                    }
                }
                remember(&mut next);
                if let Some(leaf) = Leaf::parse(id) {
                    report.brought.push(Brought {
                        label: doc.label(&leaf, &state.spellings),
                        from: party.id.clone(),
                    });
                }
            }
            // Its own value is a leftover now: it takes the agreed one when its turn comes.
            Some(Side::Agreed) => remember(&mut next),
            None => {
                blocked.insert(id.clone());
            }
        }
    }
    state.seen.insert(party.id.clone(), next);
    blocked
}

/// Brings one account's `.claude.json` in step with what the accounts agree on, leaving alone the
/// leaves that wait for the operator. Returns how many settings were set or taken out; None when the
/// file is no longer what this sync read (a Claude Code saved it meanwhile): nothing is written then,
/// and what was saved is for the next sync to fold in.
fn apply(
    state: &mut State,
    party: &Party,
    blocked: Option<&BTreeSet<String>>,
    doc: &Doc,
    create: bool,
) -> io::Result<Option<usize>> {
    let mut config = party.config.clone().unwrap_or_default();
    let before = state.seen.get(&party.id).cloned();
    let mut seen = state.seen.remove(&party.id).unwrap_or_default();
    let ids: BTreeSet<&String> = state.base.keys().chain(party.leaves.keys()).collect();
    let mut changed = 0;
    for id in ids {
        if blocked.is_some_and(|blocked| blocked.contains(id)) {
            continue;
        }
        let agreed = state.base.get(id);
        let agreed_mark = agreed.map(mark);
        if agreed_mark != party.leaves.get(id).map(mark)
            && let Some(leaf) = Leaf::parse(id)
        {
            edit(&mut config, &leaf, agreed, &state.spellings);
            changed += 1;
        }
        match agreed_mark {
            Some(mark) => {
                seen.insert(id.clone(), mark);
            }
            None => {
                seen.remove(id);
            }
        }
    }
    // A holder with no file of its own gets one, even when there is nothing to put in it yet: a copy
    // of a file that says nothing is still a copy.
    let create = create && party.config.is_none();
    if changed > 0 || create {
        let mut moved = false;
        // Claude Code's own lock is on its `.claude.json` alone.
        rewrite_with(&party.file, doc.claude_json, |loaded| {
            moved = !party.is(loaded);
            (!moved).then(|| Value::Object(config.clone()))
        })?;
        if moved {
            if let Some(before) = before {
                state.seen.insert(party.id.clone(), before);
            }
            return Ok(None);
        }
    }
    state.seen.insert(party.id.clone(), seen);
    Ok(Some(if create { changed.max(1) } else { changed }))
}

/// Every participant as it is on disk: the home's first, then the accounts in their order.
fn parties(doc: &Doc, state: &mut State) -> Vec<Party> {
    doc.files
        .iter()
        .map(|(id, file)| party(id, file, doc, &mut state.spellings))
        .collect()
}

/// Whether a leaf belongs to a key that is kept in step.
fn kept(id: &str, doc: &Doc) -> bool {
    let Some(keys) = &doc.keys else {
        return true;
    };
    Leaf::parse(id).is_some_and(|leaf| match leaf.project {
        Some(_) => keys.project.contains(&leaf.key),
        None => keys.top.contains(&leaf.key),
    })
}

/// Saves the state when something other than its date changed.
fn save(doc: &Doc, before: &State, state: &mut State, now: Ms) -> io::Result<()> {
    state.version = 1;
    state.at = before.at.clone();
    if before != state {
        state.at = Some(iso(now));
        write_json_atomic(&doc.state, state)?;
    }
    Ok(())
}

/// Brings the accounts' settings in step: folds in what every participant changed since Baton last
/// looked, notes what only the operator can settle, and writes the profiles `apply` names.
pub fn sync_settings(paths: &Paths, config: &Config, apply: Apply<'_>, now: Ms) -> io::Result<SyncReport> {
    sync_within(paths, config, apply, now, LOCK_WAIT)
}

/// How often a sync starts over because a Claude Code saved a profile it was about to write.
const ROUNDS: usize = 3;

/// [`sync_settings`], waiting at most `wait` for another baton that is syncing.
fn sync_within(
    paths: &Paths,
    config: &Config,
    which: Apply<'_>,
    now: Ms,
    wait: Duration,
) -> io::Result<SyncReport> {
    let Some(_lock) = lock(paths, wait) else {
        return Ok(SyncReport {
            busy: true,
            ..SyncReport::default()
        });
    };
    sync_doc(&Doc::claude_json(paths, config), which, now)
}

/// Brings one file in step among its participants. The caller holds the sync lock.
pub(crate) fn sync_doc(doc: &Doc, which: Apply<'_>, now: Ms) -> io::Result<SyncReport> {
    let mut merged = SyncReport::default();
    for round in 1..=ROUNDS {
        let (report, moved) = sync_once(doc, which, now)?;
        // What an earlier round brought and wrote stays said.
        merged.brought.extend(report.brought);
        merged.written.extend(report.written);
        merged = SyncReport {
            brought: merged.brought,
            written: merged.written,
            ..report
        };
        if !moved || round == ROUNDS {
            break;
        }
    }
    Ok(merged)
}

/// One round of a sync, and whether a file it was to write had been saved by a Claude Code since it
/// was read: that file was left as it is, and another round reads it anew.
fn sync_once(doc: &Doc, which: Apply<'_>, now: Ms) -> io::Result<(SyncReport, bool)> {
    let mut report = SyncReport::default();
    let before: State = read_json_as(&doc.state).unwrap_or_default();
    let mut state = before.clone();
    // A key that is no longer kept in step is forgotten, not taken out of anyone's file.
    state.base.retain(|id, _| kept(id, doc));
    for seen in state.seen.values_mut() {
        seen.retain(|id, _| kept(id, doc));
    }
    let all = parties(doc, &mut state);
    state.seen.retain(|id, _| all.iter().any(|party| party.id == *id));
    let mut blocked: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for party in &all {
        if party.unreadable {
            report.unreadable.push(party.id.clone());
            continue;
        }
        let own = collect(&mut state, party, &mut report, doc);
        if !own.is_empty() {
            blocked.insert(party.id.clone(), own);
        }
    }
    // A grant nobody holds any more was taken back everywhere: only then is it gone.
    if doc.claude_json && all.iter().all(|party| !party.unreadable) {
        state.base.retain(|id, _| {
            !Leaf::parse(id).is_some_and(|leaf| leaf.one_way())
                || all.iter().any(|party| party.leaves.contains_key(id))
        });
    }
    let mut applied: BTreeSet<&str> = BTreeSet::new();
    let mut moved = false;
    // A copied file that somebody has is made for every holder that has none.
    let create = !doc.claude_json && all.iter().any(|party| party.config.is_some());
    for party in all.iter().filter(|party| !party.unreadable) {
        // `~/.claude.json` is only ever read; a copied file's original is written whenever anything is.
        let home = party.id == HOME;
        let wanted = match which {
            Apply::Nothing => false,
            Apply::One(id) => party.id == id || (home && !doc.claude_json),
            Apply::All => !(home && doc.claude_json),
        };
        if !wanted {
            continue;
        }
        let Some(settings) = self::apply(&mut state, party, blocked.get(&party.id), doc, create)? else {
            moved = true;
            continue;
        };
        applied.insert(party.id.as_str());
        if settings > 0 {
            report.written.push(Written {
                account: party.id.clone(),
                settings,
            });
        }
    }
    // What everyone holds now: the agreed value where a profile was just brought in step.
    let holds = |party: &Party, id: &String| {
        let own = blocked.get(&party.id).is_some_and(|blocked| blocked.contains(id));
        if applied.contains(party.id.as_str()) && !own {
            state.base.get(id)
        } else {
            party.leaves.get(id)
        }
        .cloned()
    };
    let open: BTreeSet<&String> = blocked.values().flatten().collect();
    let pending: Vec<Question> = open
        .into_iter()
        .filter_map(|id| {
            let leaf = Leaf::parse(id)?;
            Some(Question {
                leaf: doc.asked(id),
                label: doc.label(&leaf, &state.spellings),
                values: all
                    .iter()
                    .filter(|party| party.config.is_some() || applied.contains(party.id.as_str()))
                    .map(|party| Held {
                        who: party.id.clone(),
                        value: holds(party, id),
                    })
                    .collect(),
                at: before
                    .pending
                    .iter()
                    .find(|known| doc.leaf_of(&known.leaf) == Some(id.as_str()))
                    .and_then(|known| known.at.clone())
                    .or_else(|| Some(iso(now))),
            })
        })
        .collect();
    state.pending = pending;
    report.questions = state.pending.clone();
    save(doc, &before, &mut state, now)?;
    Ok((report, moved))
}

/// The settings that wait for the operator, as the last sync left them.
pub fn read_questions(paths: &Paths) -> Vec<Question> {
    read_json_as::<State>(&sync_file(paths))
        .map(|state| state.pending)
        .unwrap_or_default()
}

/// Answers questions about `.claude.json`: for each of these leaves, the value `who` holds (an
/// account's id, or [`HOME`]) becomes the one every account has. Then every profile is brought in step.
pub fn take(paths: &Paths, config: &Config, leaves: &[String], who: &str, now: Ms) -> io::Result<SyncReport> {
    let Some(_lock) = lock(paths, LOCK_WAIT) else {
        return Ok(SyncReport {
            busy: true,
            ..SyncReport::default()
        });
    };
    let doc = Doc::claude_json(paths, config);
    take_in(&doc, leaves, who, now)?;
    sync_doc(&doc, Apply::All, now)
}

/// Makes the value `who` holds the agreed one for each of these questions that is about `doc`; an
/// error when `who` holds no such file. The caller holds the sync lock, and brings the file in step
/// afterwards.
pub(crate) fn take_in(doc: &Doc, asked: &[String], who: &str, now: Ms) -> io::Result<()> {
    let leaves: Vec<&str> = asked.iter().filter_map(|asked| doc.leaf_of(asked)).collect();
    let before: State = read_json_as(&doc.state).unwrap_or_default();
    let mut state = before.clone();
    let all = parties(doc, &mut state);
    let Some(chosen) = all.iter().find(|party| party.id == who && party.config.is_some()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{who} has no settings to take"),
        ));
    };
    for id in leaves {
        match chosen.leaves.get(id) {
            Some(value) => {
                state.base.insert(id.to_owned(), value.clone());
            }
            None => {
                state.base.remove(id);
            }
        }
        // What everyone holds now is a leftover: it takes the answer when its turn comes.
        for party in all.iter().filter(|party| party.config.is_some()) {
            let seen = state.seen.entry(party.id.clone()).or_default();
            match party.leaves.get(id) {
                Some(value) => {
                    seen.insert(id.to_owned(), mark(value));
                }
                None => {
                    seen.remove(id);
                }
            }
        }
        state
            .pending
            .retain(|question| doc.leaf_of(&question.leaf) != Some(id));
    }
    save(doc, &before, &mut state, now)
}

/// The questions about a file, as its last sync left them.
pub(crate) fn questions_in(state: &Path) -> Vec<Question> {
    read_json_as::<State>(state)
        .map(|state| state.pending)
        .unwrap_or_default()
}

/// Forgets what Baton saw of one participant in a file. The caller holds the sync lock.
pub(crate) fn forget_in(state_file: &Path, id: &str) -> io::Result<()> {
    let Some(mut state) = read_json_as::<State>(state_file) else {
        return Ok(());
    };
    if state.seen.remove(id).is_none() {
        return Ok(());
    }
    for question in &mut state.pending {
        question.values.retain(|held| held.who != id);
    }
    write_json_atomic(state_file, &state)
}

/// Takes the sync lock for `wait` at most; None while another baton syncs.
pub(crate) fn sync_lock(paths: &Paths, wait: Duration) -> Option<OsLock> {
    lock(paths, wait)
}

/// How long a sync waits for another baton to finish its own.
pub(crate) const SYNC_WAIT: Duration = LOCK_WAIT;

/// Forgets what Baton saw in a removed account's profile.
pub fn forget_account(paths: &Paths, id: &str) -> io::Result<()> {
    let Some(_lock) = lock(paths, LOCK_WAIT) else {
        return Ok(());
    };
    forget_in(&sync_file(paths), id)
}

/// Something granted in `.claude.json` that the sync never takes back by itself (a `has...` flag, an
/// item of an `allowed...` list), and who holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Grant {
    /// Its name in `sync.json`: what [`revoke`] is given.
    pub leaf: String,
    /// It in words: `hasCompletedOnboarding`, `E:/work · allowedTools: "Bash(git:*)"`.
    pub label: String,
    /// Who holds it: accounts' ids, and [`HOME`].
    pub holders: Vec<String>,
}

/// The grants any account or `~/.claude.json` holds: the file's own first, then folder by folder.
pub fn grants(paths: &Paths, config: &Config) -> Vec<Grant> {
    let doc = Doc::claude_json(paths, config);
    let mut state: State = read_json_as(&doc.state).unwrap_or_default();
    let all = parties(&doc, &mut state);
    let mut held: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for party in &all {
        for (id, value) in &party.leaves {
            if !is_stock(value) && Leaf::parse(id).is_some_and(|leaf| leaf.one_way()) {
                held.entry(id.clone()).or_default().push(party.id.clone());
            }
        }
    }
    let mut out: Vec<(Leaf, Grant)> = held
        .into_iter()
        .filter_map(|(id, holders)| {
            let leaf = Leaf::parse(&id)?;
            let label = leaf.label(&state.spellings);
            Some((
                leaf,
                Grant {
                    leaf: id,
                    label,
                    holders,
                },
            ))
        })
        .collect();
    out.sort_by(|(a, _), (b, _)| {
        (a.project.is_some(), &a.project, &a.key, &a.entry, &a.item).cmp(&(
            b.project.is_some(),
            &b.project,
            &b.key,
            &b.entry,
            &b.item,
        ))
    });
    out.into_iter().map(|(_, grant)| grant).collect()
}

/// A grant taken away, and from whom.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Revoked {
    /// It in words.
    pub label: String,
    /// Whose file it was taken out of: accounts' ids, and [`HOME`].
    pub from: Vec<String>,
}

/// What [`revoke`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RevokeReport {
    /// The grants taken away.
    pub revoked: Vec<Revoked>,
    /// The accounts whose `.claude.json` is there but cannot be read: left alone, so that one keeps
    /// what it holds.
    pub unreadable: Vec<String>,
    /// Whether another baton was syncing, so that nothing was done.
    pub busy: bool,
}

/// Takes grants away from every account and from `~/.claude.json` at once: the one thing the sync
/// lets nobody do alone. Each file is changed under Claude Code's own lock, and only where it holds
/// the grant; nothing else in it is touched. `leaves` are [`Grant::leaf`]s; any other is passed over.
/// The sync then knows the grants as gone, so that none of them comes back from a file that held it.
pub fn revoke(paths: &Paths, config: &Config, leaves: &[String], now: Ms) -> io::Result<RevokeReport> {
    let Some(_lock) = lock(paths, LOCK_WAIT) else {
        return Ok(RevokeReport {
            busy: true,
            ..RevokeReport::default()
        });
    };
    let doc = Doc::claude_json(paths, config);
    let before: State = read_json_as(&doc.state).unwrap_or_default();
    let mut state = before.clone();
    let all = parties(&doc, &mut state);
    let wanted: Vec<(String, Leaf)> = leaves
        .iter()
        .filter_map(|id| Some((id.clone(), Leaf::parse(id).filter(Leaf::one_way)?)))
        .collect();
    let mut from: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut report = RevokeReport::default();
    for party in &all {
        if party.unreadable {
            report.unreadable.push(party.id.clone());
            continue;
        }
        let mut took: Vec<String> = Vec::new();
        // Asked again with the newer file when Claude Code saved meanwhile: what it holds then counts.
        let wrote = rewrite(&party.file, |loaded| {
            took.clear();
            let Loaded::Json(Value::Object(own)) = loaded else {
                return None;
            };
            let (held, _) = flatten(own, &doc);
            let mut next = own.clone();
            for (id, leaf) in &wanted {
                if held.get(id).is_some_and(|value| !is_stock(value)) {
                    edit(&mut next, leaf, None, &state.spellings);
                    took.push(id.clone());
                }
            }
            (!took.is_empty()).then_some(Value::Object(next))
        })?;
        if wrote {
            for id in took {
                from.entry(id).or_default().push(party.id.clone());
            }
        }
    }
    for (id, leaf) in &wanted {
        state.base.remove(id);
        // Nobody holds it now. A file that could not be read keeps what Baton last saw there: once it
        // can be, the grant it still holds is a leftover there, and the next sync takes it out.
        for party in all.iter().filter(|party| !party.unreadable) {
            if let Some(seen) = state.seen.get_mut(&party.id) {
                seen.remove(id);
            }
        }
        state
            .pending
            .retain(|question| doc.leaf_of(&question.leaf) != Some(id.as_str()));
        if let Some(holders) = from.remove(id) {
            report.revoked.push(Revoked {
                label: leaf.label(&state.spellings),
                from: holders,
            });
        }
    }
    // With full sync never used there is nothing the sync knows to put right.
    if doc.state.exists() {
        save(&doc, &before, &mut state, now)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::config::normalize_config;
    use baton_core::paths::baton_paths;
    use serde_json::json;
    use std::fs;

    const T0: Ms = 1_780_000_000_000;

    /// A home with three accounts, and its config.
    fn home() -> (tempfile::TempDir, Paths, Config) {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let config = normalize_config(&json!({ "accounts": [
            { "id": "a", "name": "A" }, { "id": "b", "name": "B" }, { "id": "c", "name": "C" }
        ] }));
        (dir, paths, config)
    }

    fn put(file: &Path, value: &Value) {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, serde_json::to_string_pretty(value).unwrap()).unwrap();
    }

    fn profile(paths: &Paths, id: &str) -> Value {
        load(&global_config_file(paths, id)).json().unwrap_or(Value::Null)
    }

    fn change(paths: &Paths, id: &str, change: impl FnOnce(&mut Value)) {
        let mut value = profile(paths, id);
        change(&mut value);
        put(&global_config_file(paths, id), &value);
    }

    fn all(paths: &Paths, config: &Config) -> SyncReport {
        sync_settings(paths, config, Apply::All, T0).unwrap()
    }

    #[test]
    fn leaves_have_names_and_words() {
        let leaf = Leaf {
            project: Some("e:/work".into()),
            key: "allowedTools".into(),
            entry: None,
            item: Some("\"Bash(git:*)\"".into()),
        };
        assert_eq!(Leaf::parse(&leaf.id()), Some(leaf.clone()));
        let spellings = BTreeMap::from([("e:/work".to_owned(), "E:/Work".to_owned())]);
        assert_eq!(leaf.label(&spellings), "E:/Work · allowedTools: \"Bash(git:*)\"");
        let server = Leaf {
            project: None,
            key: "mcpServers".into(),
            entry: Some("github".into()),
            item: None,
        };
        assert_eq!(server.label(&spellings), "mcpServers: github");
        assert!(leaf.grants() && !server.grants());
        assert!(
            Leaf {
                project: None,
                key: "hasSeenTasksHint".into(),
                entry: None,
                item: None
            }
            .grants()
        );
        assert_eq!(Leaf::parse("nonsense"), None);
        assert_eq!(
            canon(&json!({ "b": 1, "a": { "d": 2, "c": [3] } })),
            r#"{"a":{"c":[3],"d":2},"b":1}"#
        );
        assert_eq!(mark(&json!({ "b": 1, "a": 2 })), mark(&json!({ "a": 2, "b": 1 })));
        assert_eq!(mark(&json!(1)).len(), 16);
    }

    #[test]
    fn a_new_account_gets_what_the_others_have_and_never_who_they_are() {
        let (_dir, paths, config) = home();
        put(
            &paths.claude_json,
            &json!({
                "theme": "dark", "userID": "home-user", "oauthAccount": { "emailAddress": "home@example.invalid" },
                "numStartups": 40, "mcpServers": { "files": { "command": "files", "args": ["-y", "x"] } },
                "projects": { "E:/Work": { "hasTrustDialogAccepted": true, "allowedTools": ["Read"], "lastCost": 3 } }
            }),
        );
        put(
            &global_config_file(&paths, "a"),
            &json!({
                "theme": "dark", "userID": "user-a", "oauthAccount": { "emailAddress": "a@example.invalid" },
                "mcpServers": { "files": { "args": ["-y", "x"], "command": "files" }, "github": { "command": "gh" } },
                "projects": {
                    "E:\\Work": { "hasTrustDialogAccepted": false, "allowedTools": ["Read", "Bash(git:*)"] },
                    "E:/Other": { "hasTrustDialogAccepted": true, "allowedTools": [] }
                }
            }),
        );
        let report = all(&paths, &config);
        assert!(report.questions.is_empty(), "{:?}", report.questions);
        assert!(!report.busy);
        // b and c had no file: they get the settings, and nothing else.
        let b = profile(&paths, "b");
        assert_eq!(b["theme"], "dark");
        assert_eq!(
            b["mcpServers"],
            json!({ "files": { "command": "files", "args": ["-y", "x"] }, "github": { "command": "gh" } })
        );
        let work = &b["projects"]["E:/Work"];
        assert_eq!(work["hasTrustDialogAccepted"], true);
        let mut tools: Vec<&str> = work["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        tools.sort_unstable();
        assert_eq!(tools, ["Bash(git:*)", "Read"]);
        assert_eq!(
            work.as_object().unwrap().len(),
            2,
            "nothing but the settings: {work}"
        );
        assert_eq!(
            b["projects"]["E:/Other"],
            json!({ "hasTrustDialogAccepted": true })
        );
        for never in ["userID", "oauthAccount", "numStartups"] {
            assert!(b.get(never).is_none(), "{never} is not carried over");
        }
        assert_eq!(profile(&paths, "c"), b);
        // a keeps who it is, its own spelling of the folder, and gets the trust the home has.
        let a = profile(&paths, "a");
        assert_eq!(a["userID"], "user-a");
        assert_eq!(a["oauthAccount"]["emailAddress"], "a@example.invalid");
        assert_eq!(a["projects"]["E:\\Work"]["hasTrustDialogAccepted"], true);
        assert!(a["projects"].get("E:/Work").is_none());
        // ~/.claude.json is only ever read.
        assert_eq!(
            load(&paths.claude_json).json().unwrap()["mcpServers"],
            json!({ "files": { "command": "files", "args": ["-y", "x"] } })
        );
        // Nothing more to do the second time, and nothing is written.
        let before = fs::read_to_string(global_config_file(&paths, "b")).unwrap();
        let again = all(&paths, &config);
        assert!(again.written.is_empty() && again.brought.is_empty(), "{again:?}");
        assert_eq!(
            fs::read_to_string(global_config_file(&paths, "b")).unwrap(),
            before
        );
    }

    #[test]
    fn what_changes_on_one_account_is_on_the_next_one_when_its_turn_comes() {
        let (_dir, paths, config) = home();
        put(
            &paths.claude_json,
            &json!({ "theme": "dark", "mcpServers": { "files": { "command": "files" } } }),
        );
        all(&paths, &config);
        change(&paths, "a", |a| {
            a["theme"] = json!("light");
            a["editorMode"] = json!("vim");
            a["mcpServers"]["github"] = json!({ "command": "gh" });
            a["mcpServers"].as_object_mut().unwrap().remove("files");
            a["projects"] = json!({ "/work": { "hasTrustDialogAccepted": true, "allowedTools": ["Read"] } });
        });
        // The conversation moves to b: only b is written.
        let c_before = fs::read_to_string(global_config_file(&paths, "c")).unwrap();
        let report = sync_settings(&paths, &config, Apply::One("b"), T0 + 1).unwrap();
        assert_eq!(
            report.written,
            vec![Written {
                account: "b".into(),
                settings: 6
            }]
        );
        assert!(report.brought.iter().all(|brought| brought.from == "a"));
        let b = profile(&paths, "b");
        assert_eq!(
            (b["theme"].as_str(), b["editorMode"].as_str()),
            (Some("light"), Some("vim"))
        );
        assert_eq!(b["mcpServers"], json!({ "github": { "command": "gh" } }));
        assert_eq!(
            b["projects"]["/work"],
            json!({ "hasTrustDialogAccepted": true, "allowedTools": ["Read"] })
        );
        assert_eq!(
            fs::read_to_string(global_config_file(&paths, "c")).unwrap(),
            c_before
        );
        // A server taken out on b is taken out on c and a when their turn comes.
        change(&paths, "b", |b| b["mcpServers"] = json!({}));
        sync_settings(&paths, &config, Apply::One("c"), T0 + 2).unwrap();
        let c = profile(&paths, "c");
        assert_eq!(c["theme"], "light");
        assert_eq!(c["mcpServers"], json!({}));
        assert_eq!(c["projects"]["/work"]["allowedTools"], json!(["Read"]));
        sync_settings(&paths, &config, Apply::One("a"), T0 + 3).unwrap();
        assert_eq!(profile(&paths, "a")["mcpServers"], json!({}));
        // What the home sets later reaches the accounts; the home itself still says dark for the theme.
        let mut home = load(&paths.claude_json).json().unwrap();
        home["verbose"] = json!(true);
        put(&paths.claude_json, &home);
        all(&paths, &config);
        assert_eq!(profile(&paths, "a")["verbose"], true);
        assert_eq!(profile(&paths, "a")["theme"], "light");
        assert_eq!(load(&paths.claude_json).json().unwrap()["theme"], "dark");
    }

    #[test]
    fn two_accounts_that_differ_are_a_question_and_nobody_is_written_over() {
        let (_dir, paths, config) = home();
        put(&paths.claude_json, &json!({ "theme": "dark" }));
        all(&paths, &config);
        change(&paths, "a", |a| a["theme"] = json!("light"));
        change(&paths, "b", |b| b["theme"] = json!("dark-daltonized"));
        let report = all(&paths, &config);
        assert_eq!(report.questions.len(), 1);
        let question = &report.questions[0];
        assert_eq!(question.label, "theme");
        assert_eq!(
            question.values,
            vec![
                Held {
                    who: HOME.into(),
                    value: Some(json!("dark"))
                },
                Held {
                    who: "a".into(),
                    value: Some(json!("light"))
                },
                Held {
                    who: "b".into(),
                    value: Some(json!("dark-daltonized"))
                },
                Held {
                    who: "c".into(),
                    value: Some(json!("light"))
                },
            ],
            "the first to change it reaches the account that did not; the other keeps its own"
        );
        assert_eq!(profile(&paths, "b")["theme"], "dark-daltonized");
        assert_eq!(read_questions(&paths), report.questions);
        // It stays a question, first noticed then, until it is answered.
        let later = sync_settings(&paths, &config, Apply::All, T0 + 60_000).unwrap();
        assert_eq!(later.questions[0].at, Some(iso(T0)));
        let answered = take(
            &paths,
            &config,
            std::slice::from_ref(&question.leaf),
            "b",
            T0 + 120_000,
        )
        .unwrap();
        assert!(answered.questions.is_empty());
        for id in ["a", "b", "c"] {
            assert_eq!(profile(&paths, id)["theme"], "dark-daltonized", "{id}");
        }
        assert!(read_questions(&paths).is_empty());
        assert!(take(&paths, &config, &[], "nobody", T0).is_err());
    }

    #[test]
    fn counters_versions_and_grants_need_no_question() {
        let (_dir, paths, config) = home();
        put(
            &paths.claude_json,
            &json!({ "lastReleaseNotesSeen": "2.1.300", "projects": { "/w": { "projectOnboardingSeenCount": 1 } } }),
        );
        all(&paths, &config);
        change(&paths, "a", |a| {
            a["lastReleaseNotesSeen"] = json!("2.1.326");
            a["hasSeenTasksHint"] = json!(true);
            a["projects"]["/w"]["projectOnboardingSeenCount"] = json!(4);
        });
        change(&paths, "b", |b| {
            b["lastReleaseNotesSeen"] = json!("2.1.9");
            b["hasSeenTasksHint"] = json!(false);
            b["projects"]["/w"]["projectOnboardingSeenCount"] = json!(2);
            b["projects"]["/w"]["hasTrustDialogAccepted"] = json!(true);
        });
        let report = all(&paths, &config);
        assert!(report.questions.is_empty(), "{:?}", report.questions);
        for id in ["a", "b", "c"] {
            let own = profile(&paths, id);
            assert_eq!(own["lastReleaseNotesSeen"], "2.1.326", "{id}");
            assert_eq!(own["hasSeenTasksHint"], true, "{id}");
            assert_eq!(own["projects"]["/w"]["projectOnboardingSeenCount"], 4, "{id}");
            assert_eq!(own["projects"]["/w"]["hasTrustDialogAccepted"], true, "{id}");
        }
        assert_eq!(settle(Some(&json!("x")), None), Some(Side::Theirs));
        assert_eq!(settle(None, Some(&json!("x"))), Some(Side::Agreed));
        assert_eq!(settle(Some(&json!("dark")), Some(&json!("light"))), None);
        assert_eq!(version(&json!("2.1.326")), Some(vec![2, 1, 326]));
        assert_eq!(version(&json!("dark")), None);
    }

    #[test]
    fn a_grant_one_account_lost_comes_back_and_is_gone_only_when_nobody_holds_it() {
        let (_dir, paths, config) = home();
        let granted = json!({
            "hasTrustDialogAccepted": true, "allowedTools": ["Edit", "Read"], "projectOnboardingSeenCount": 4
        });
        put(
            &paths.claude_json,
            &json!({ "hasCompletedOnboarding": true, "projects": { "/w": granted } }),
        );
        all(&paths, &config);
        // a's onboarding starts over: its flags are off again, a tool is gone, its count is lower.
        change(&paths, "a", |a| {
            a["hasCompletedOnboarding"] = json!(false);
            a["projects"]["/w"] = json!({
                "hasTrustDialogAccepted": false, "allowedTools": ["Read"], "projectOnboardingSeenCount": 1
            });
        });
        // The conversation moves to b: nothing is taken from it.
        let report = sync_settings(&paths, &config, Apply::One("b"), T0 + 1).unwrap();
        assert!(
            report.brought.is_empty() && report.written.is_empty(),
            "{report:?}"
        );
        assert!(report.questions.is_empty());
        assert_eq!(profile(&paths, "b")["projects"]["/w"], granted);
        // When a's turn comes it has them again.
        let report = sync_settings(&paths, &config, Apply::One("a"), T0 + 2).unwrap();
        assert_eq!(
            report.written,
            vec![Written {
                account: "a".into(),
                settings: 4
            }]
        );
        let a = profile(&paths, "a");
        assert_eq!(a["hasCompletedOnboarding"], true);
        assert_eq!(a["projects"]["/w"]["hasTrustDialogAccepted"], true);
        assert_eq!(a["projects"]["/w"]["projectOnboardingSeenCount"], 4);
        let mut tools: Vec<&str> = a["projects"]["/w"]["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        tools.sort_unstable();
        assert_eq!(tools, ["Edit", "Read"]);
        // Taken back by all but one, it still stands; taken back by everyone, the home too, it is gone.
        let take_back = |value: &mut Value| value["projects"]["/w"]["allowedTools"] = json!(["Read"]);
        for id in ["a", "b"] {
            change(&paths, id, take_back);
        }
        let mut home = load(&paths.claude_json).json().unwrap();
        take_back(&mut home);
        put(&paths.claude_json, &home);
        all(&paths, &config);
        assert_eq!(
            profile(&paths, "a")["projects"]["/w"]["allowedTools"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        for id in ["a", "b", "c"] {
            change(&paths, id, take_back);
        }
        all(&paths, &config);
        all(&paths, &config);
        for id in ["a", "b", "c"] {
            assert_eq!(
                profile(&paths, id)["projects"]["/w"]["allowedTools"],
                json!(["Read"]),
                "{id}"
            );
            assert_eq!(
                profile(&paths, id)["projects"]["/w"]["hasTrustDialogAccepted"],
                true
            );
        }
    }

    #[test]
    fn grants_are_listed_and_taken_away_from_everyone_at_once_for_good() {
        let (_dir, paths, config) = home();
        put(
            &paths.claude_json,
            &json!({
                "hasCompletedOnboarding": true, "theme": "dark", "oauthAccount": { "emailAddress": "me" },
                "projects": { "/w": { "hasTrustDialogAccepted": true, "allowedTools": ["Edit", "Read"] } }
            }),
        );
        all(&paths, &config);
        change(&paths, "b", |b| {
            b["projects"]["/old"] = json!({ "hasTrustDialogAccepted": true });
        });
        all(&paths, &config);
        let listed = grants(&paths, &config);
        let labels: Vec<&str> = listed.iter().map(|grant| grant.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "hasCompletedOnboarding",
                "/old · hasTrustDialogAccepted",
                "/w · allowedTools: \"Edit\"",
                "/w · allowedTools: \"Read\"",
                "/w · hasTrustDialogAccepted",
            ]
        );
        assert_eq!(listed[0].holders, ["~", "a", "b", "c"]);
        assert_eq!(listed[1].holders, ["a", "b", "c"], "only the accounts sync");
        // Taken from every account and from the home, and from nothing else.
        let wanted = vec![listed[2].leaf.clone(), listed[4].leaf.clone()];
        let report = revoke(&paths, &config, &wanted, T0 + 1).unwrap();
        assert_eq!(
            report.revoked,
            vec![
                Revoked {
                    label: "/w · allowedTools: \"Edit\"".into(),
                    from: vec!["~".into(), "a".into(), "b".into(), "c".into()]
                },
                Revoked {
                    label: "/w · hasTrustDialogAccepted".into(),
                    from: vec!["~".into(), "a".into(), "b".into(), "c".into()]
                },
            ]
        );
        let home = load(&paths.claude_json).json().unwrap();
        assert_eq!(home["projects"]["/w"], json!({ "allowedTools": ["Read"] }));
        assert_eq!(
            (
                home["theme"].as_str(),
                home["oauthAccount"]["emailAddress"].as_str()
            ),
            (Some("dark"), Some("me"))
        );
        // No sync brings them back, nor does an account that was away meanwhile.
        all(&paths, &config);
        all(&paths, &config);
        for id in ["a", "b", "c"] {
            assert_eq!(
                profile(&paths, id)["projects"]["/w"],
                json!({ "allowedTools": ["Read"] }),
                "{id}"
            );
            assert_eq!(
                profile(&paths, id)["projects"]["/old"]["hasTrustDialogAccepted"],
                true,
                "{id}"
            );
        }
        assert_eq!(grants(&paths, &config).len(), 3);
        // Granted again later, it goes everywhere as before.
        change(&paths, "c", |c| {
            c["projects"]["/w"]["hasTrustDialogAccepted"] = json!(true)
        });
        all(&paths, &config);
        assert_eq!(
            profile(&paths, "a")["projects"]["/w"]["hasTrustDialogAccepted"],
            true
        );
        // Nothing to take: nothing is written, and what is not a grant is passed over.
        let theme = "[null,\"theme\",null,null]".to_owned();
        let none = revoke(&paths, &config, &[theme, "nonsense".into()], T0 + 2).unwrap();
        assert_eq!(none, RevokeReport::default());
        assert_eq!(profile(&paths, "a")["theme"], "dark");
    }

    #[test]
    fn a_profile_claude_code_saved_meanwhile_is_not_written_over() {
        let (_dir, paths, config) = home();
        put(&paths.claude_json, &json!({ "theme": "dark" }));
        all(&paths, &config);
        change(&paths, "b", |b| b["theme"] = json!("light"));
        // A sync reads everyone and works out that a takes b's theme...
        let doc = Doc::claude_json(&paths, &config);
        let mut state: State = read_json_as(&sync_file(&paths)).unwrap();
        let everyone = parties(&doc, &mut state);
        let mut report = SyncReport::default();
        for party in &everyone {
            collect(&mut state, party, &mut report, &doc);
        }
        // ...and before it writes a's file, the Claude Code running there saves it.
        change(&paths, "a", |a| a["numStartups"] = json!(7));
        let a = everyone.iter().find(|party| party.id == "a").unwrap();
        let seen = state.seen.get("a").cloned();
        assert_eq!(apply(&mut state, a, None, &doc, false).unwrap(), None);
        assert_eq!(state.seen.get("a").cloned(), seen);
        assert_eq!(
            profile(&paths, "a"),
            json!({ "theme": "dark", "numStartups": 7 }),
            "left as Claude Code saved it"
        );
        // The next sync reads it anew: it is in step, and what was saved is kept.
        let report = all(&paths, &config);
        assert!(report.written.iter().any(|written| written.account == "a"));
        assert_eq!(
            profile(&paths, "a"),
            json!({ "theme": "light", "numStartups": 7 })
        );
    }

    #[test]
    fn a_profile_that_was_reset_gets_its_settings_back_and_takes_nobodys_away() {
        let (_dir, paths, config) = home();
        put(
            &paths.claude_json,
            &json!({
                "theme": "dark", "editorMode": "vim", "verbose": true, "diffTool": "auto",
                "mcpServers": { "one": { "command": "1" }, "two": { "command": "2" } }
            }),
        );
        all(&paths, &config);
        // Claude Code starts a's file afresh; an unreadable file is left exactly as it is.
        put(
            &global_config_file(&paths, "a"),
            &json!({ "userID": "a", "theme": "light" }),
        );
        fs::write(global_config_file(&paths, "b"), "{ half").unwrap();
        let report = all(&paths, &config);
        assert_eq!(report.reset, vec!["a".to_owned()]);
        assert_eq!(report.unreadable, vec!["b".to_owned()]);
        assert_eq!(
            fs::read_to_string(global_config_file(&paths, "b")).unwrap(),
            "{ half"
        );
        let a = profile(&paths, "a");
        assert_eq!(a["mcpServers"].as_object().unwrap().len(), 2);
        assert_eq!(
            (a["editorMode"].as_str(), a["userID"].as_str()),
            (Some("vim"), Some("a"))
        );
        assert_eq!(profile(&paths, "c")["mcpServers"].as_object().unwrap().len(), 2);
        // Its one setting that differs is not taken for a change either: it is asked about.
        assert_eq!(report.questions.len(), 1);
        assert_eq!(a["theme"], "light");
    }

    #[test]
    fn the_operator_adds_keys_but_never_who_an_account_is() {
        let (_dir, paths, mut config) = home();
        config.sync_keys = vec!["diffSidebarOpen".into(), "userID".into(), "projects".into()];
        config.sync_project_keys = vec!["ignorePatterns".into()];
        put(&paths.claude_json, &json!({ "theme": "dark" }));
        put(
            &global_config_file(&paths, "a"),
            &json!({ "userID": "user-a", "diffSidebarOpen": true, "projects": { "/w": { "ignorePatterns": ["dist"], "lastCost": 2 } } }),
        );
        all(&paths, &config);
        let b = profile(&paths, "b");
        assert_eq!(b["diffSidebarOpen"], true);
        assert_eq!(b["projects"], json!({ "/w": { "ignorePatterns": ["dist"] } }));
        assert!(b.get("userID").is_none());
        // A key that is no longer kept in step is forgotten, and stays where it is.
        config.sync_keys.clear();
        change(&paths, "a", |a| a["diffSidebarOpen"] = json!(false));
        all(&paths, &config);
        assert_eq!(profile(&paths, "b")["diffSidebarOpen"], true);
        assert!(full_sync(&config));
    }

    #[test]
    fn one_baton_syncs_at_a_time_and_a_removed_account_is_forgotten() {
        let (_dir, paths, config) = home();
        put(&paths.claude_json, &json!({ "theme": "dark" }));
        all(&paths, &config);
        let held = lock(&paths, Duration::ZERO).unwrap();
        let report = sync_within(&paths, &config, Apply::All, T0, Duration::ZERO).unwrap();
        assert!(report.busy && report.written.is_empty());
        drop(held);
        assert!(!all(&paths, &config).busy);
        let seen = |paths: &Paths| read_json_as::<State>(&sync_file(paths)).unwrap().seen;
        assert!(seen(&paths).contains_key("a"));
        forget_account(&paths, "a").unwrap();
        assert!(!seen(&paths).contains_key("a"));
        forget_account(&paths, "a").unwrap();
    }

    #[test]
    fn edits_touch_one_leaf_and_keep_the_order() {
        let mut config = json!({ "z": 1, "mcpServers": { "b": 1, "a": 2 }, "y": 2 })
            .as_object()
            .cloned()
            .unwrap();
        let spellings = BTreeMap::new();
        let leaf = |key: &str, entry: Option<&str>| Leaf {
            project: None,
            key: key.into(),
            entry: entry.map(str::to_owned),
            item: None,
        };
        edit(&mut config, &leaf("mcpServers", Some("b")), None, &spellings);
        edit(
            &mut config,
            &leaf("mcpServers", Some("c")),
            Some(&json!(3)),
            &spellings,
        );
        edit(&mut config, &leaf("z", None), None, &spellings);
        edit(
            &mut config,
            &leaf("theme", None),
            Some(&json!("dark")),
            &spellings,
        );
        assert_eq!(
            compact(&config),
            r#"{"mcpServers":{"a":2,"c":3},"y":2,"theme":"dark"}"#
        );
        // Taking out what is not there makes nothing.
        let folder = Leaf {
            project: Some("/w".into()),
            key: "allowedTools".into(),
            entry: None,
            item: Some("\"Read\"".into()),
        };
        edit(&mut config, &folder, None, &spellings);
        assert!(config.get("projects").is_none());
        edit(&mut config, &folder, Some(&json!("Read")), &spellings);
        edit(&mut config, &folder, Some(&json!("Read")), &spellings);
        assert_eq!(config["projects"], json!({ "/w": { "allowedTools": ["Read"] } }));
    }
}
