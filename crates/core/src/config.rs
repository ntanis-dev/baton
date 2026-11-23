//! `~/.baton/config.json`: the accounts the operator added, in priority order, with their marks and
//! threshold overrides, and the policy settings. There is no built-in account: Baton never interacts
//! with the default Claude Code account.
//!
//! An account has a stable id and a name. The id keys everything on disk (its profile folder
//! `~/.baton/accounts/<id>`, `usage/<id>.json`, the history, the sessions, the copies) and is what the
//! hooks resolve from `CLAUDE_CONFIG_DIR`; the name is a label the operator can change at any time
//! (`names`: any printable text).

use std::collections::BTreeMap;
use std::fs;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::fsutil::{is_busy, sleep_ms, write_json_durable};
use crate::json::{Get, Object, num};
use crate::names::{clean_name, id_from_name, same_name, valid_name};
use crate::paths::{Paths, is_account_id};
use crate::window::PerWindow;

/// The version written into config.json.
pub const CONFIG_VERSION: u32 = 1;

/// How an account is used when new work is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mark {
    /// Used first while it has room.
    #[serde(rename = "preferred")]
    Preferred,
    /// Used by weekly urgency.
    #[serde(rename = "normal")]
    Normal,
    /// Used only when every other account is blocked.
    #[serde(rename = "last-resort")]
    LastResort,
}

/// Every mark, in the order they are offered.
pub const MARKS: [Mark; 3] = [Mark::Preferred, Mark::Normal, Mark::LastResort];

impl Mark {
    /// The mark as written: `preferred`, `normal`, `last-resort`.
    pub fn as_str(self) -> &'static str {
        match self {
            Mark::Preferred => "preferred",
            Mark::Normal => "normal",
            Mark::LastResort => "last-resort",
        }
    }

    /// The mark of a word.
    pub fn parse(word: &str) -> Option<Mark> {
        MARKS.into_iter().find(|m| m.as_str() == word)
    }

    /// The mark as people read it, wherever it is shown: `preferred`, `normal`, `last resort`.
    pub fn words(self) -> &'static str {
        match self {
            Mark::Preferred => "preferred",
            Mark::Normal => "normal",
            Mark::LastResort => "last resort",
        }
    }
}

/// How new work is placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    /// By mark, reserve and weekly urgency.
    #[serde(rename = "smart")]
    Smart,
    /// The first open account in the operator's order.
    #[serde(rename = "order")]
    Order,
}

/// Every mode.
pub const MODES: [Mode; 2] = [Mode::Smart, Mode::Order];

impl Mode {
    /// The mode as written: `smart`, `order`.
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Smart => "smart",
            Mode::Order => "order",
        }
    }

    /// The mode of a word.
    pub fn parse(word: &str) -> Option<Mode> {
        MODES.into_iter().find(|m| m.as_str() == word)
    }
}

/// How the session wrapper ends `claude` at a turn end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EndMethod {
    /// Keys on Windows, a signal elsewhere.
    #[serde(rename = "auto")]
    Auto,
    /// Two Ctrl+C typed into the console.
    #[serde(rename = "keys")]
    Keys,
    /// SIGTERM, then SIGKILL.
    #[serde(rename = "signal")]
    Signal,
    /// Ended at once.
    #[serde(rename = "kill")]
    Kill,
}

/// Every end method.
pub const END_METHODS: [EndMethod; 4] = [
    EndMethod::Auto,
    EndMethod::Keys,
    EndMethod::Signal,
    EndMethod::Kill,
];

impl EndMethod {
    /// The method as written.
    pub fn as_str(self) -> &'static str {
        match self {
            EndMethod::Auto => "auto",
            EndMethod::Keys => "keys",
            EndMethod::Signal => "signal",
            EndMethod::Kill => "kill",
        }
    }

    /// The method of a word.
    pub fn parse(word: &str) -> Option<EndMethod> {
        END_METHODS.into_iter().find(|m| m.as_str() == word)
    }
}

/// Every number the policy uses, with its default. Percentages are of a window's limit; times as named.
#[derive(Clone, Debug, PartialEq)]
pub struct Policy {
    /// How new work is placed.
    pub mode: Mode,
    /// 5-hour threshold: an account at or above it is blocked.
    pub five_hour: f64,
    /// Weekly threshold.
    pub weekly: f64,
    /// 5-hour headroom an account needs to take new work.
    pub start_reserve: f64,
    /// A planned hop when headroom is under this many times the next turn's cost.
    pub plan_factor: f64,
    /// Turns in the moving average of a session's cost.
    pub cost_samples: f64,
    /// No planned or compaction hop sooner after arriving.
    pub min_stay_minutes: f64,
    /// A 5-hour refusal resetting this soon waits in place.
    pub wait_in_place_minutes: f64,
    /// All blocked: the current account waits in place up to this.
    pub wait_in_place_max_hours: f64,
    /// Compaction rebalance: blocked within this at the burn rate...
    pub rebalance_horizon_minutes: f64,
    /// ...and another account has this much more effective headroom.
    pub rebalance_margin: f64,
    /// Readings used for a session's burn rate.
    pub burn_window_minutes: f64,
    /// A refusal with no known reset blocks at most this long.
    pub unknown_rejection_hours: f64,
    /// Before an automatic hop.
    pub countdown_seconds: f64,
    /// Refusals right after a resume before Baton backs off.
    pub max_reblocks: f64,
    /// How long Baton backs off then.
    pub reblock_backoff_minutes: f64,
    /// Wait after a turn ends for the last status line reading.
    pub settle_ms: f64,
    /// Rebalance at the cheap moment after a compaction.
    pub compaction_rebalance: bool,
    /// A running baton instance asks every signed-in account's usage this often (0: only on request).
    pub usage_poll_minutes: f64,
    /// Add Baton's usage after the operator's own status line.
    pub status_line_usage: bool,
    /// Keep what the operator sets (preferences, MCP servers, each folder's trust and approvals) the
    /// same on every account, in both directions.
    pub full_sync: bool,
    /// The history's months that ended longer ago than this many days are removed (0: never).
    pub history_days: f64,
    /// The records of conversations not seen for this many days are removed.
    pub session_days: f64,
    /// The wrappers' logs not written for this many days are removed (0: never).
    pub log_days: f64,
    /// How many backups are kept, the newest ones (0: all of them).
    pub backups_kept: f64,
}

impl Default for Policy {
    fn default() -> Policy {
        Policy {
            mode: Mode::Smart,
            five_hour: 90.0,
            weekly: 95.0,
            start_reserve: 20.0,
            plan_factor: 1.5,
            cost_samples: 5.0,
            min_stay_minutes: 30.0,
            wait_in_place_minutes: 20.0,
            wait_in_place_max_hours: 24.0,
            rebalance_horizon_minutes: 60.0,
            rebalance_margin: 20.0,
            burn_window_minutes: 60.0,
            unknown_rejection_hours: 5.0,
            countdown_seconds: 10.0,
            max_reblocks: 3.0,
            reblock_backoff_minutes: 15.0,
            settle_ms: 800.0,
            compaction_rebalance: true,
            usage_poll_minutes: 30.0,
            status_line_usage: false,
            full_sync: true,
            history_days: 180.0,
            session_days: 30.0,
            log_days: 14.0,
            backups_kept: 50.0,
        }
    }
}

/// The policy's settings by name, in the order they are shown and written.
pub const POLICY_KEYS: [&str; 25] = [
    "mode",
    "fiveHour",
    "weekly",
    "startReserve",
    "planFactor",
    "costSamples",
    "minStayMinutes",
    "waitInPlaceMinutes",
    "waitInPlaceMaxHours",
    "rebalanceHorizonMinutes",
    "rebalanceMargin",
    "burnWindowMinutes",
    "unknownRejectionHours",
    "countdownSeconds",
    "maxReblocks",
    "reblockBackoffMinutes",
    "settleMs",
    "compactionRebalance",
    "usagePollMinutes",
    "statusLineUsage",
    "fullSync",
    "historyDays",
    "sessionDays",
    "logDays",
    "backupsKept",
];

/// The settings that are true or false.
const BOOLEANS: [&str; 3] = ["compactionRebalance", "statusLineUsage", "fullSync"];

/// The range each number may take.
fn number_limits(key: &str) -> Option<(f64, f64)> {
    Some(match key {
        "fiveHour" | "weekly" => (1.0, 100.0),
        "startReserve" | "planFactor" | "rebalanceMargin" | "maxReblocks" => (0.0, 100.0),
        "costSamples" => (1.0, 100.0),
        "minStayMinutes" | "waitInPlaceMinutes" | "rebalanceHorizonMinutes" | "reblockBackoffMinutes" => {
            (0.0, 10080.0)
        }
        "waitInPlaceMaxHours" | "unknownRejectionHours" => (0.0, 168.0),
        "burnWindowMinutes" => (1.0, 10080.0),
        "countdownSeconds" => (0.0, 600.0),
        "settleMs" => (0.0, 60000.0),
        "usagePollMinutes" => (0.0, 1440.0),
        "historyDays" => (0.0, 3650.0),
        "sessionDays" => (1.0, 365.0),
        "logDays" => (0.0, 365.0),
        "backupsKept" => (0.0, 1000.0),
        _ => return None,
    })
}

impl Policy {
    /// The value of the setting `key`, as JSON; None for a name that is not a setting.
    pub fn get(&self, key: &str) -> Option<Value> {
        Some(match key {
            "mode" => json!(self.mode.as_str()),
            "compactionRebalance" => json!(self.compaction_rebalance),
            "statusLineUsage" => json!(self.status_line_usage),
            "fullSync" => json!(self.full_sync),
            _ => num(self.number(key)?),
        })
    }

    /// The number setting `key`.
    fn number(&self, key: &str) -> Option<f64> {
        Some(match key {
            "fiveHour" => self.five_hour,
            "weekly" => self.weekly,
            "startReserve" => self.start_reserve,
            "planFactor" => self.plan_factor,
            "costSamples" => self.cost_samples,
            "minStayMinutes" => self.min_stay_minutes,
            "waitInPlaceMinutes" => self.wait_in_place_minutes,
            "waitInPlaceMaxHours" => self.wait_in_place_max_hours,
            "rebalanceHorizonMinutes" => self.rebalance_horizon_minutes,
            "rebalanceMargin" => self.rebalance_margin,
            "burnWindowMinutes" => self.burn_window_minutes,
            "unknownRejectionHours" => self.unknown_rejection_hours,
            "countdownSeconds" => self.countdown_seconds,
            "maxReblocks" => self.max_reblocks,
            "reblockBackoffMinutes" => self.reblock_backoff_minutes,
            "settleMs" => self.settle_ms,
            "usagePollMinutes" => self.usage_poll_minutes,
            "historyDays" => self.history_days,
            "sessionDays" => self.session_days,
            "logDays" => self.log_days,
            "backupsKept" => self.backups_kept,
            _ => return None,
        })
    }

    /// Sets `key` to a value [`check_policy_value`] returned. Anything else is ignored.
    pub fn set(&mut self, key: &str, value: &Value) {
        match (key, value) {
            ("mode", Value::String(word)) => {
                if let Some(mode) = Mode::parse(word) {
                    self.mode = mode;
                }
            }
            ("compactionRebalance", Value::Bool(on)) => self.compaction_rebalance = *on,
            ("statusLineUsage", Value::Bool(on)) => self.status_line_usage = *on,
            ("fullSync", Value::Bool(on)) => self.full_sync = *on,
            (_, Value::Number(n)) => {
                let Some(n) = n.as_f64() else { return };
                let slot = match key {
                    "fiveHour" => &mut self.five_hour,
                    "weekly" => &mut self.weekly,
                    "startReserve" => &mut self.start_reserve,
                    "planFactor" => &mut self.plan_factor,
                    "costSamples" => &mut self.cost_samples,
                    "minStayMinutes" => &mut self.min_stay_minutes,
                    "waitInPlaceMinutes" => &mut self.wait_in_place_minutes,
                    "waitInPlaceMaxHours" => &mut self.wait_in_place_max_hours,
                    "rebalanceHorizonMinutes" => &mut self.rebalance_horizon_minutes,
                    "rebalanceMargin" => &mut self.rebalance_margin,
                    "burnWindowMinutes" => &mut self.burn_window_minutes,
                    "unknownRejectionHours" => &mut self.unknown_rejection_hours,
                    "countdownSeconds" => &mut self.countdown_seconds,
                    "maxReblocks" => &mut self.max_reblocks,
                    "reblockBackoffMinutes" => &mut self.reblock_backoff_minutes,
                    "settleMs" => &mut self.settle_ms,
                    "usagePollMinutes" => &mut self.usage_poll_minutes,
                    "historyDays" => &mut self.history_days,
                    "sessionDays" => &mut self.session_days,
                    "logDays" => &mut self.log_days,
                    "backupsKept" => &mut self.backups_kept,
                    _ => return,
                };
                *slot = n;
            }
            _ => {}
        }
    }

    /// Every setting as a JSON object, in [`POLICY_KEYS`] order.
    pub fn to_json(&self) -> Object {
        POLICY_KEYS
            .iter()
            .filter_map(|key| Some(((*key).to_owned(), self.get(key)?)))
            .collect()
    }
}

/// A config that cannot be read, or a setting that cannot take a value.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct ConfigError(pub String);

/// A value as a number the way a loose reader takes it: text that is a number, true as 1, false, null
/// and the empty string as 0. NaN for anything else.
pub fn loose_f64(value: &Value) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                0.0
            } else {
                text.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Null => 0.0,
        Value::Array(_) | Value::Object(_) => f64::NAN,
    }
}

/// A value as the word it would print as.
fn word_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Checks a value for the policy setting `key` and returns it as it is kept: a mode's word, true or
/// false, or a number in the setting's range.
pub fn check_policy_value(key: &str, value: &Value) -> Result<Value, ConfigError> {
    if key == "mode" {
        return match value.as_str().and_then(Mode::parse) {
            Some(mode) => Ok(json!(mode.as_str())),
            None => Err(ConfigError(format!(
                "mode is one of {}.",
                MODES.map(Mode::as_str).join(", ")
            ))),
        };
    }
    if BOOLEANS.contains(&key) {
        let word = word_of(value).to_lowercase();
        if ["true", "on", "yes", "1"].contains(&word.as_str()) {
            return Ok(json!(true));
        }
        if ["false", "off", "no", "0"].contains(&word.as_str()) {
            return Ok(json!(false));
        }
        return Err(ConfigError(format!("{key} is true or false.")));
    }
    let Some((lo, hi)) = number_limits(key) else {
        return Err(ConfigError(format!("Unknown setting {key}.")));
    };
    let n = loose_f64(value);
    if !n.is_finite() || n < lo || n > hi {
        return Err(ConfigError(format!("{key} takes a number from {lo} to {hi}.")));
    }
    Ok(num(n))
}

/// One of the operator's accounts.
#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    /// The stable id: the profile folder's name.
    pub id: String,
    /// The label.
    pub name: String,
    /// How it is used when new work is placed.
    pub mark: Mark,
    /// Its own 5-hour threshold, when it has one.
    pub five_hour: Option<f64>,
    /// Its own weekly threshold, when it has one.
    pub weekly: Option<f64>,
}

/// What config.json held that this Baton does not know (a note the operator wrote in, a setting of a
/// later Baton): kept as it was and written back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unknown {
    /// Keys of the file itself.
    pub top: Object,
    /// Settings of the policy.
    pub policy: Object,
    /// Keys of the accounts' entries, by account id.
    pub accounts: BTreeMap<String, Object>,
}

/// The keys of config.json this Baton knows.
const TOP_KEYS: [&str; 7] = [
    "version",
    "accounts",
    "policy",
    "endMethod",
    "claudePath",
    "syncKeys",
    "syncProjectKeys",
];

/// The keys of an account's entry this Baton knows (`lastResort`: how a mark was once written).
const ACCOUNT_KEYS: [&str; 6] = ["id", "name", "mark", "fiveHour", "weekly", "lastResort"];

/// config.json, read.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// The version of the file as it was read: one above [`CONFIG_VERSION`] was written by a later
    /// Baton, and is never saved over.
    pub version: u32,
    /// The accounts, in priority order.
    pub accounts: Vec<Account>,
    /// The policy settings the operator changed, by name, each checked.
    pub policy: Object,
    /// How the wrapper ends claude.
    pub end_method: EndMethod,
    /// Where Claude Code is, when the operator said so.
    pub claude_path: Option<String>,
    /// More keys of Claude Code's `.claude.json` to keep in step between accounts, beyond the ones
    /// Baton knows as the operator's own settings (`syncKeys`).
    pub sync_keys: Vec<String>,
    /// The same for the keys of a folder's entry there (`syncProjectKeys`).
    pub sync_project_keys: Vec<String>,
    /// What the file held that this Baton does not know.
    pub unknown: Unknown,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            version: CONFIG_VERSION,
            accounts: Vec::new(),
            policy: Object::new(),
            end_method: EndMethod::Auto,
            claude_path: None,
            sync_keys: Vec::new(),
            sync_project_keys: Vec::new(),
            unknown: Unknown::default(),
        }
    }
}

/// The entries of `map` whose key is not one of `known`.
fn unknown_of(map: &Object, known: &[&str]) -> Object {
    map.iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// An account as config.json holds it, or None when the entry cannot be one.
fn normalize_account(raw: &Value) -> Option<Account> {
    if !raw.is_object() {
        return None;
    }
    let raw_name = raw.text("name");
    // Without an id (edited by hand): the name when it can be one, else one made from it.
    let id = match (raw.text("id"), raw_name) {
        (Some(id), _) if is_account_id(id) => id.to_owned(),
        (_, Some(name)) if is_account_id(name) => name.to_owned(),
        (_, Some(name)) if valid_name(name) => id_from_name(name),
        _ => return None,
    };
    let name = match raw_name {
        Some(name) if valid_name(name) => clean_name(name),
        _ => id.clone(),
    };
    let mark = raw
        .text("mark")
        .and_then(Mark::parse)
        .unwrap_or(if raw.truthy("lastResort") {
            Mark::LastResort
        } else {
            Mark::Normal
        });
    let threshold = |key: &str| {
        let value = raw.at(key);
        if value.is_null() {
            return None;
        }
        // A threshold is a percentage; anything else falls back to the policy's.
        Some(loose_f64(value)).filter(|n| (1.0..=100.0).contains(n))
    };
    Some(Account {
        id,
        name,
        mark,
        five_hour: threshold("fiveHour"),
        weekly: threshold("weekly"),
    })
}

/// A config from whatever config.json held: what is missing or invalid falls back to its default.
pub fn normalize_config(raw: &Value) -> Config {
    let mut accounts: Vec<Account> = Vec::new();
    let mut unknown = Unknown::default();
    for entry in raw
        .at("accounts")
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        let Some(account) = normalize_account(entry) else {
            continue;
        };
        // Ids name folders, and two that differ only in case are one folder on most disks.
        if !accounts
            .iter()
            .any(|a| a.id.eq_ignore_ascii_case(&account.id) || same_name(&a.name, &account.name))
        {
            let extra = entry
                .as_object()
                .map(|entry| unknown_of(entry, &ACCOUNT_KEYS))
                .unwrap_or_default();
            if !extra.is_empty() {
                unknown.accounts.insert(account.id.clone(), extra);
            }
            accounts.push(account);
        }
    }
    let mut policy = Object::new();
    if let Some(settings) = raw.at("policy").as_object() {
        for (key, value) in settings {
            // An out-of-range setting falls back to its default; one this Baton does not know is
            // kept as it is.
            if let Ok(checked) = check_policy_value(key, value) {
                policy.insert(key.clone(), checked);
            } else if !POLICY_KEYS.contains(&key.as_str()) {
                unknown.policy.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(top) = raw.as_object() {
        unknown.top = unknown_of(top, &TOP_KEYS);
    }
    Config {
        version: raw
            .at("version")
            .as_u64()
            .and_then(|version| u32::try_from(version).ok())
            .unwrap_or(CONFIG_VERSION),
        accounts,
        policy,
        end_method: raw
            .text("endMethod")
            .and_then(EndMethod::parse)
            .unwrap_or(EndMethod::Auto),
        claude_path: raw.text("claudePath").map(str::to_owned),
        sync_keys: names(raw.at("syncKeys")),
        sync_project_keys: names(raw.at("syncProjectKeys")),
        unknown,
    }
}

/// The names in a list of config.json, without repeats; anything that is not a name is left out.
fn names(list: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in list.as_array().into_iter().flatten().filter_map(Value::as_str) {
        let name = name.trim();
        if !name.is_empty() && !out.iter().any(|known| known == name) {
            out.push(name.to_owned());
        }
    }
    out
}

/// config.json as written, or None when there is none. One that is there but cannot be read or is not
/// a config is an error: read as empty, the next change would save an empty config over every account.
/// A read that fails the way Windows fails one while another process replaces the file (or a virus
/// scanner holds it) is retried briefly first.
fn read_config_file(paths: &Paths) -> Result<Option<Value>, ConfigError> {
    let file = &paths.config;
    let shown = file.display();
    let mut attempt = 0u64;
    let text = loop {
        match fs::read_to_string(file) {
            Ok(text) => break text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) if attempt < 6 && is_busy(&error) => {
                sleep_ms(5 * (attempt + 1));
                attempt += 1;
            }
            Err(error) => return Err(ConfigError(format!("Cannot read {shown}: {error}"))),
        }
    };
    // An editor's byte order mark is not part of the JSON.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    let raw: Value = serde_json::from_str(text).map_err(|error| {
        ConfigError(format!(
            "{shown} is not valid JSON ({error}); fix it, or move it away to start over."
        ))
    })?;
    let accounts = raw.at("accounts");
    if !raw.is_object() || !(accounts.is_null() && raw.get("accounts").is_none() || accounts.is_array()) {
        return Err(ConfigError(format!(
            "{shown} is not a Baton config; fix it, or move it away to start over."
        )));
    }
    Ok(Some(raw))
}

/// Reads config.json (what is missing or invalid falls back to its default). A file that cannot be
/// read at all reads as no accounts: for showing only, never for saving back.
pub fn load_config(paths: &Paths) -> Config {
    load_config_strict(paths).unwrap_or_default()
}

/// Reads config.json to change it and save it back: a file that is there but cannot be read is an
/// error instead.
pub fn load_config_strict(paths: &Paths) -> Result<Config, ConfigError> {
    Ok(match read_config_file(paths)? {
        Some(raw) => normalize_config(&raw),
        None => Config::default(),
    })
}

/// Why config.json cannot be used, in words for the operator; None when it can (or is not there yet).
/// Whoever shows the accounts asks this first: a file that cannot be read reads as no accounts.
pub fn config_problem(paths: &Paths) -> Option<String> {
    load_config_strict(paths).err().map(|error| error.0)
}

/// Whether two account ids are the same: they name folders, so their case does not tell them apart.
pub fn same_id(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// How [`find_account`] looks an account up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    /// By its name (ignoring case), else by its id: the command line.
    Name,
    /// By its id only: the menu, the wrapper.
    Id,
}

/// An account by its name or id.
pub fn find_account<'a>(config: &'a Config, key: &str, by: By) -> Option<&'a Account> {
    let by_id = || config.accounts.iter().find(|a| same_id(&a.id, key));
    if by == By::Id {
        return by_id();
    }
    config
        .accounts
        .iter()
        .find(|a| same_name(&a.name, key))
        .or_else(by_id)
}

/// The name of the account `id`; the id itself when there is no such account.
pub fn name_of(config: &Config, id: &str) -> String {
    config
        .accounts
        .iter()
        .find(|a| same_id(&a.id, id))
        .map_or_else(|| id.to_owned(), |a| a.name.clone())
}

/// The config as config.json holds it.
pub fn config_json(config: &Config) -> Value {
    let accounts: Vec<Value> = config
        .accounts
        .iter()
        .map(|a| {
            let mut out = Object::new();
            out.insert("id".to_owned(), json!(a.id));
            out.insert("name".to_owned(), json!(a.name));
            out.insert("mark".to_owned(), json!(a.mark.as_str()));
            if let Some(n) = a.five_hour {
                out.insert("fiveHour".to_owned(), num(n));
            }
            if let Some(n) = a.weekly {
                out.insert("weekly".to_owned(), num(n));
            }
            out.extend(config.unknown.accounts.get(&a.id).cloned().unwrap_or_default());
            Value::Object(out)
        })
        .collect();
    let mut policy = config.policy.clone();
    for (key, value) in &config.unknown.policy {
        policy.entry(key.clone()).or_insert_with(|| value.clone());
    }
    let mut out = Object::new();
    out.insert("version".to_owned(), json!(config.version.max(CONFIG_VERSION)));
    out.insert("accounts".to_owned(), Value::Array(accounts));
    out.insert("policy".to_owned(), Value::Object(policy));
    if config.end_method != EndMethod::Auto {
        out.insert("endMethod".to_owned(), json!(config.end_method.as_str()));
    }
    if let Some(path) = config.claude_path.as_deref().filter(|p| !p.is_empty()) {
        out.insert("claudePath".to_owned(), json!(path));
    }
    if !config.sync_keys.is_empty() {
        out.insert("syncKeys".to_owned(), json!(config.sync_keys));
    }
    if !config.sync_project_keys.is_empty() {
        out.insert("syncProjectKeys".to_owned(), json!(config.sync_project_keys));
    }
    out.extend(config.unknown.top.clone());
    Value::Object(out)
}

/// Where the config as it was before the last save is kept.
pub fn config_backup(paths: &Paths) -> std::path::PathBuf {
    paths.root.join("config.json.bak")
}

/// Writes config.json, keeping the file as it was beside it (`config.json.bak`). A config a later
/// Baton wrote is not saved over: this one does not know everything that is in it.
pub fn save_config(paths: &Paths, config: &Config) -> io::Result<()> {
    if config.version > CONFIG_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} was written by a later Baton (version {}, this one writes {CONFIG_VERSION}); update Baton to change it.",
                paths.config.display(),
                config.version
            ),
        ));
    }
    if paths.config.exists() {
        fs::copy(&paths.config, config_backup(paths))?;
    }
    write_json_durable(&paths.config, &config_json(config))
}

/// The policy in force: the defaults with the operator's changes.
pub fn effective_policy(config: &Config) -> Policy {
    let mut policy = Policy::default();
    for (key, value) in &config.policy {
        policy.set(key, value);
    }
    policy
}

/// An account's thresholds: its own, else the policy's.
pub fn thresholds(account: &Account, policy: &Policy) -> PerWindow<f64> {
    PerWindow {
        five_hour: account.five_hour.unwrap_or(policy.five_hour),
        seven_day: account.weekly.unwrap_or(policy.weekly),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::baton_paths;

    #[test]
    fn policy_values_are_checked() {
        assert_eq!(check_policy_value("mode", &json!("order")), Ok(json!("order")));
        assert_eq!(
            check_policy_value("mode", &json!("fast")).unwrap_err().0,
            "mode is one of smart, order."
        );
        assert_eq!(
            check_policy_value("statusLineUsage", &json!("ON")),
            Ok(json!(true))
        );
        assert_eq!(check_policy_value("statusLineUsage", &json!(0)), Ok(json!(false)));
        assert_eq!(
            check_policy_value("compactionRebalance", &json!("maybe"))
                .unwrap_err()
                .0,
            "compactionRebalance is true or false."
        );
        assert_eq!(check_policy_value("startReserve", &json!("25")), Ok(json!(25)));
        assert_eq!(check_policy_value("planFactor", &json!(1.5)), Ok(json!(1.5)));
        assert_eq!(
            check_policy_value("fiveHour", &json!(0)).unwrap_err().0,
            "fiveHour takes a number from 1 to 100."
        );
        assert_eq!(
            check_policy_value("settleMs", &json!("soon")).unwrap_err().0,
            "settleMs takes a number from 0 to 60000."
        );
        assert_eq!(
            check_policy_value("nope", &json!(1)).unwrap_err().0,
            "Unknown setting nope."
        );
    }

    #[test]
    fn every_setting_has_a_default_a_name_and_a_range_or_kind() {
        let policy = Policy::default();
        let all = policy.to_json();
        assert_eq!(all.len(), POLICY_KEYS.len());
        assert_eq!(all["mode"], json!("smart"));
        assert_eq!(all["fiveHour"], json!(90));
        assert_eq!(all["planFactor"], json!(1.5));
        assert_eq!(all["statusLineUsage"], json!(false));
        assert_eq!(all["fullSync"], json!(true));
        for key in POLICY_KEYS {
            let value = policy.get(key).unwrap();
            assert_eq!(check_policy_value(key, &value).unwrap(), value, "{key}");
        }
    }

    #[test]
    fn a_config_is_normalized() {
        let config = normalize_config(&json!({
            "accounts": [
                { "id": "work", "name": " Work account ", "mark": "preferred", "fiveHour": 85 },
                { "name": "spare" },
                { "name": "Jane Doe", "lastResort": true, "weekly": "90" },
                { "name": "WORK ACCOUNT" },
                { "id": "work", "name": "Other" },
                { "name": 7 },
                "nonsense"
            ],
            "policy": { "mode": "order", "weekly": 500, "startReserve": 25, "nope": 1 },
            "endMethod": "kill",
            "claudePath": "C:/claude.exe"
        }));
        assert_eq!(
            config.accounts,
            vec![
                Account {
                    id: "work".into(),
                    name: "Work account".into(),
                    mark: Mark::Preferred,
                    five_hour: Some(85.0),
                    weekly: None
                },
                Account {
                    id: "spare".into(),
                    name: "spare".into(),
                    mark: Mark::Normal,
                    five_hour: None,
                    weekly: None
                },
                Account {
                    id: "jane-doe".into(),
                    name: "Jane Doe".into(),
                    mark: Mark::LastResort,
                    five_hour: None,
                    weekly: Some(90.0)
                },
            ]
        );
        assert_eq!(
            Value::Object(config.policy.clone()),
            json!({ "mode": "order", "startReserve": 25 })
        );
        assert_eq!(config.end_method, EndMethod::Kill);
        assert_eq!(config.claude_path.as_deref(), Some("C:/claude.exe"));
        assert_eq!(Value::Object(config.unknown.policy.clone()), json!({ "nope": 1 }));
        let policy = effective_policy(&config);
        assert_eq!(policy.mode, Mode::Order);
        assert_eq!(policy.start_reserve, 25.0);
        assert_eq!(policy.weekly, 95.0);
        assert_eq!(
            thresholds(&config.accounts[0], &policy),
            PerWindow {
                five_hour: 85.0,
                seven_day: 95.0
            }
        );
        assert_eq!(
            thresholds(&config.accounts[2], &policy),
            PerWindow {
                five_hour: 90.0,
                seven_day: 90.0
            }
        );
    }

    #[test]
    fn accounts_are_found_by_name_then_id() {
        let config = normalize_config(&json!({ "accounts": [
            { "id": "a1", "name": "Work" },
            { "id": "work", "name": "Spare" }
        ] }));
        assert_eq!(find_account(&config, "WORK", By::Name).unwrap().id, "a1");
        assert_eq!(find_account(&config, "work", By::Id).unwrap().id, "work");
        assert_eq!(find_account(&config, "a1", By::Name).unwrap().name, "Work");
        assert!(
            find_account(&config, "Spare", By::Id).is_none(),
            "a name is not an id"
        );
        assert_eq!(
            find_account(&config, "WORK", By::Id).unwrap().id,
            "work",
            "an id names a folder: in any case"
        );
        assert_eq!(name_of(&config, "work"), "Spare");
        assert_eq!(name_of(&config, "gone"), "gone");
    }

    #[test]
    fn a_config_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        assert_eq!(load_config(&paths), Config::default());
        assert_eq!(load_config_strict(&paths), Ok(Config::default()));
        let mut config = normalize_config(&json!({
            "accounts": [{ "id": "work", "name": "Work", "mark": "preferred", "fiveHour": 85 }, { "id": "spare", "name": "Spare" }],
            "policy": { "startReserve": 25 }
        }));
        save_config(&paths, &config).unwrap();
        assert_eq!(
            fs::read_to_string(&paths.config).unwrap(),
            "{\n  \"version\": 1,\n  \"accounts\": [\n    {\n      \"id\": \"work\",\n      \"name\": \"Work\",\n      \"mark\": \"preferred\",\n      \"fiveHour\": 85\n    },\n    {\n      \"id\": \"spare\",\n      \"name\": \"Spare\",\n      \"mark\": \"normal\"\n    }\n  ],\n  \"policy\": {\n    \"startReserve\": 25\n  }\n}\n"
        );
        assert_eq!(load_config(&paths), config);
        config.end_method = EndMethod::Keys;
        config.claude_path = Some("claude".into());
        config.sync_keys = vec!["diffSidebarOpen".into()];
        config.sync_project_keys = vec!["ignorePatterns".into()];
        save_config(&paths, &config).unwrap();
        assert_eq!(load_config_strict(&paths), Ok(config));
    }

    #[test]
    fn a_config_that_cannot_be_read_is_an_error_only_when_strict() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        fs::create_dir_all(&paths.root).unwrap();
        fs::write(&paths.config, "{ nope").unwrap();
        assert_eq!(load_config(&paths), Config::default());
        let error = load_config_strict(&paths).unwrap_err().0;
        assert!(error.contains("is not valid JSON"), "{error}");
        assert!(
            error.ends_with("fix it, or move it away to start over."),
            "{error}"
        );
        for not_a_config in ["[]", "3", "{\"accounts\": {}}"] {
            fs::write(&paths.config, not_a_config).unwrap();
            let error = load_config_strict(&paths).unwrap_err().0;
            assert!(error.contains("is not a Baton config"), "{not_a_config}: {error}");
        }
        assert!(config_problem(&paths).unwrap().contains("is not a Baton config"));
        fs::write(&paths.config, "{}").unwrap();
        assert_eq!(load_config_strict(&paths), Ok(Config::default()));
        assert_eq!(config_problem(&paths), None);
        // A byte order mark an editor put first is not part of the JSON.
        let marked = format!("{}{}", '\u{FEFF}', r#"{"accounts": [{ "name": "work" }]}"#);
        fs::write(&paths.config, marked).unwrap();
        assert_eq!(load_config(&paths).accounts.len(), 1);
    }

    #[test]
    fn a_save_keeps_what_this_baton_does_not_know_and_the_file_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        fs::create_dir_all(&paths.root).unwrap();
        let written = json!({
            "version": 1,
            "note": "the spare is my partner's",
            "accounts": [
                { "id": "work", "name": "Work", "mark": "normal", "fiveHour": 400, "team": "infra" },
                { "id": "WORK", "name": "Shouting" },
                { "id": "spare", "name": "Spare", "weekly": 0 }
            ],
            "policy": { "mode": "order", "laterSetting": [1, 2], "weekly": 500 }
        });
        fs::write(&paths.config, serde_json::to_string(&written).unwrap()).unwrap();
        let mut config = load_config_strict(&paths).unwrap();
        // Ids that differ only in case are one folder: the first stays. A threshold that is no
        // percentage falls back to the policy's.
        let ids: Vec<&str> = config.accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["work", "spare"]);
        assert_eq!(
            (config.accounts[0].five_hour, config.accounts[1].weekly),
            (None, None)
        );
        assert_eq!(find_account(&config, "SPARE", By::Id).unwrap().id, "spare");
        assert_eq!(name_of(&config, "WORK"), "Work");
        config.accounts[1].name = "Other".into();
        save_config(&paths, &config).unwrap();
        let read = |file: &std::path::Path| -> Value {
            serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
        };
        let saved = read(&paths.config);
        assert_eq!(saved["note"], "the spare is my partner's");
        assert_eq!(saved["accounts"][0]["team"], "infra");
        assert_eq!(saved["accounts"][1]["name"], "Other");
        assert_eq!(
            saved["policy"],
            json!({ "mode": "order", "laterSetting": [1, 2] })
        );
        assert_eq!(
            read(&config_backup(&paths)),
            written,
            "the file as it was before the save"
        );
        // What a later Baton wrote is read, and never saved over.
        let later_text = r#"{ "version": 2, "accounts": [{ "name": "work" }] }"#;
        fs::write(&paths.config, later_text).unwrap();
        let later = load_config_strict(&paths).unwrap();
        assert_eq!((later.version, later.accounts.len()), (2, 1));
        let refused = save_config(&paths, &later).unwrap_err().to_string();
        assert!(
            refused.contains("written by a later Baton (version 2"),
            "{refused}"
        );
        assert_eq!(fs::read_to_string(&paths.config).unwrap(), later_text);
    }
}
