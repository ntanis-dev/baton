//! Account management: the operations the CLI and the menu share. Each takes the paths and plain
//! arguments, changes config.json (and the profile folder for add and remove) and returns data; none
//! prints.
//!
//! Accounts are named by their name (ignoring case, else their id) from the CLI; [`By::Id`] names them
//! by id (the menu). Every result carries both. Values that come as the operator typed them (a
//! position, a threshold, a policy setting) are taken as JSON: text or a number, as the caller has it.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use baton_core::config::{
    Account, By, Config, MARKS, Mark, check_policy_value, find_account, load_config_strict, loose_f64,
    save_config,
};
use baton_core::json::{Object, opt_num};
use baton_core::names::{clean_name, id_from_name, name_problem, same_name};
use baton_core::oslock::OsLock;
use baton_core::paths::{Paths, profile_dir};
use serde::{Serialize, Serializer};
use serde_json::{Value, json};
use thiserror::Error;

use crate::copies::forget_copies;
use crate::profile::{LinkEntry, LinkOptions, link_profile, unlink_profile};
use crate::sync::forget_account;

/// Why an account operation did not happen.
#[derive(Debug, Error)]
pub enum AccountError {
    /// What was asked for cannot be done: the message says why, in words for the operator.
    #[error("{0}")]
    Invalid(String),
    /// The change could not be written.
    #[error("{0}")]
    Io(#[from] io::Error),
}

impl AccountError {
    /// Whether the operator asked for something that cannot be done (as opposed to a failed write).
    pub fn is_invalid(&self) -> bool {
        matches!(self, AccountError::Invalid(_))
    }
}

/// An error for the operator.
fn invalid(message: impl Into<String>) -> AccountError {
    AccountError::Invalid(message.into())
}

/// How long a change of config.json waits for another baton to finish its own.
const CHANGE_WAIT: Duration = Duration::from_secs(5);

/// config.json to change and save back, and the lock that makes this baton the only one changing it
/// until the lock is dropped: two that read it together would each save over the other's change. A
/// file that cannot be read stops the change instead of being saved over as an empty config.
fn load_for_change(paths: &Paths) -> Result<(OsLock, Config), AccountError> {
    let lock = OsLock::take(&paths.run.join("config.lock"), CHANGE_WAIT)?.ok_or_else(|| {
        invalid("Another baton is changing the accounts or the policy; try again in a moment.")
    })?;
    let config = load_config_strict(paths).map_err(|error| invalid(error.0))?;
    Ok((lock, config))
}

/// The place in the config of the account `key` names.
fn need(config: &Config, key: &str, by: By) -> Result<usize, AccountError> {
    let found = find_account(config, key, by)
        .and_then(|account| config.accounts.iter().position(|a| a.id == account.id));
    found.ok_or_else(|| {
        invalid(if config.accounts.is_empty() {
            format!("No account named {key}: Baton has no accounts yet (baton account add <name>).")
        } else {
            format!("No account named {key}.")
        })
    })
}

/// A name is any printable text (`names`), unique ignoring case. `own` is the place of the account
/// being renamed, which may keep its name.
fn check_name(config: &Config, name: &str, own: Option<usize>) -> Result<String, AccountError> {
    if let Some(problem) = name_problem(name) {
        return Err(invalid(problem));
    }
    let clean = clean_name(name);
    let taken = config
        .accounts
        .iter()
        .enumerate()
        .find(|(at, a)| Some(*at) != own && same_name(&a.name, &clean));
    if let Some((_, taken)) = taken {
        return Err(invalid(format!("{} is the name of another account.", taken.name)));
    }
    Ok(clean)
}

/// A new account's id: from its name at creation (`names::id_from_name`: lower case letters, digits,
/// hyphens and underscores), made unique among the ids and the folders in ~/.baton/accounts. It never
/// changes after; folders, files, hook arguments and environment variables use it, never the name.
fn new_id(paths: &Paths, config: &Config, name: &str) -> String {
    let base = id_from_name(name);
    let used = |id: &str| {
        config
            .accounts
            .iter()
            .any(|a| a.id.to_lowercase() == id.to_lowercase())
            || profile_dir(paths, id).exists()
    };
    if !used(&base) {
        return base;
    }
    let mut n = 2u64;
    loop {
        let id = format!("{base}-{n}");
        if !used(&id) {
            return id;
        }
        n += 1;
    }
}

/// A mark by its word.
fn check_mark(mark: &str) -> Result<Mark, AccountError> {
    Mark::parse(mark).ok_or_else(|| {
        invalid(format!(
            "A mark is one of {}.",
            MARKS.map(Mark::as_str).join(", ")
        ))
    })
}

/// A threshold override: null clears it, else a percentage from 1 to 100.
fn check_threshold(value: &Value, what: &str) -> Result<Option<f64>, AccountError> {
    if value.is_null() {
        return Ok(None);
    }
    let n = loose_f64(value);
    if !n.is_finite() || !(1.0..=100.0).contains(&n) {
        return Err(invalid(format!("{what} takes a percentage from 1 to 100.")));
    }
    Ok(Some(n))
}

/// A position as the operator gives it (from 1) as a place in a list `len` long (from 0); None when it
/// is not a whole number in `1..=max`.
fn place(position: &Value, max: usize) -> Option<usize> {
    let at = loose_f64(position) - 1.0;
    (at.is_finite() && at.fract() == 0.0 && at >= 0.0 && at < max as f64).then_some(at as usize)
}

/// An account as the results show it: every field, a threshold it does not override as null.
pub fn account_json(account: &Account) -> Value {
    json!({
        "id": account.id,
        "name": account.name,
        "mark": account.mark.as_str(),
        "fiveHour": opt_num(account.five_hour),
        "weekly": opt_num(account.weekly),
    })
}

/// Serializes an account the way [`account_json`] writes it.
fn ser_account<S: Serializer>(account: &Account, serializer: S) -> Result<S::Ok, S::Error> {
    account_json(account).serialize(serializer)
}

/// How an account is added.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AddOptions {
    /// Its mark's word; None is `normal`.
    pub mark: Option<String>,
    /// Its place in the order, from 1; None is last.
    pub position: Option<Value>,
    /// Whether its profile shares files by symbolic links; None asks the system.
    pub file_links: Option<bool>,
}

/// An account that was added.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Added {
    /// The account.
    #[serde(serialize_with = "ser_account")]
    pub account: Account,
    /// Its place in the order, from 1.
    pub position: usize,
    /// Its profile folder.
    pub profile: PathBuf,
    /// Whether that folder was there before.
    pub existed: bool,
    /// What linking its profile did.
    pub links: Vec<LinkEntry>,
}

/// Adds an account and links its profile.
pub fn add_account(paths: &Paths, name: &str, options: &AddOptions) -> Result<Added, AccountError> {
    let (_lock, mut config) = load_for_change(paths)?;
    let name = check_name(&config, name, None)?;
    let account = Account {
        id: new_id(paths, &config, &name),
        name,
        mark: check_mark(options.mark.as_deref().unwrap_or("normal"))?,
        five_hour: None,
        weekly: None,
    };
    let count = config.accounts.len();
    let at = match options.position.as_ref().filter(|position| !position.is_null()) {
        None => count,
        Some(position) => place(position, count + 1)
            .ok_or_else(|| invalid(format!("--position takes 1 to {}.", count + 1)))?,
    };
    config.accounts.insert(at, account.clone());
    let profile = profile_dir(paths, &account.id);
    let existed = profile.exists();
    let links = link_profile(
        paths,
        &account.id,
        &LinkOptions {
            file_links: options.file_links,
            ..LinkOptions::default()
        },
    )?;
    save_config(paths, &config)?;
    Ok(Added {
        account,
        position: at + 1,
        profile,
        existed,
        links,
    })
}

/// An account that was renamed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Renamed {
    /// Its id, which did not change.
    pub id: String,
    /// The name it had.
    pub from: String,
    /// The name it has now.
    pub to: String,
}

/// A rename changes only the label: the id, the profile folder (which a running claude may use as
/// CLAUDE_CONFIG_DIR), the files and the history stay, and everything shows the new name from then on.
pub fn rename_account(paths: &Paths, key: &str, new_name: &str, by: By) -> Result<Renamed, AccountError> {
    let (_lock, mut config) = load_for_change(paths)?;
    let at = need(&config, key, by)?;
    let to = check_name(&config, new_name, Some(at))?;
    let account = &mut config.accounts[at];
    let from = std::mem::replace(&mut account.name, to.clone());
    let id = account.id.clone();
    save_config(paths, &config)?;
    Ok(Renamed { id, from, to })
}

/// An account that was removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Removed {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its profile folder, which stays.
    pub profile: PathBuf,
    /// What was unlinked from that folder.
    pub unlinked: Vec<String>,
}

/// Removing an account takes it out of Baton: never listed, picked, polled or launched under again.
/// Its profile folder (with its sign-in) stays. The last account cannot go.
pub fn remove_account(paths: &Paths, key: &str, by: By) -> Result<Removed, AccountError> {
    let (_lock, mut config) = load_for_change(paths)?;
    let at = need(&config, key, by)?;
    if config.accounts.len() == 1 {
        return Err(invalid(format!(
            "{} is the only account and Baton needs one: add another (baton account add <name>) before removing it.",
            config.accounts[at].name
        )));
    }
    let account = config.accounts.remove(at);
    let unlinked = unlink_profile(paths, &account.id)?;
    forget_copies(paths, &account.id)?;
    forget_account(paths, &account.id)?;
    save_config(paths, &config)?;
    Ok(Removed {
        profile: profile_dir(paths, &account.id),
        id: account.id,
        name: account.name,
        unlinked,
    })
}

/// The accounts' names in their new order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Order {
    /// The names, first to last.
    pub order: Vec<String>,
}

/// The order of a config's accounts, by name.
fn order_of(config: &Config) -> Order {
    Order {
        order: config.accounts.iter().map(|a| a.name.clone()).collect(),
    }
}

/// New order: the accounts given first, in that order; the rest keep theirs.
pub fn order_accounts<S: AsRef<str>>(paths: &Paths, keys: &[S], by: By) -> Result<Order, AccountError> {
    let (_lock, mut config) = load_for_change(paths)?;
    let first = keys
        .iter()
        .map(|key| need(&config, key.as_ref(), by))
        .collect::<Result<Vec<usize>, _>>()?;
    if first.iter().enumerate().any(|(n, at)| first[..n].contains(at)) {
        return Err(invalid("An account appears twice."));
    }
    let rest = (0..config.accounts.len()).filter(|at| !first.contains(at));
    config.accounts = first
        .iter()
        .copied()
        .chain(rest)
        .map(|at| config.accounts[at].clone())
        .collect();
    save_config(paths, &config)?;
    Ok(order_of(&config))
}

/// Moves an account to a place in the order (from 1).
pub fn move_account(paths: &Paths, key: &str, position: &Value, by: By) -> Result<Order, AccountError> {
    let (_lock, mut config) = load_for_change(paths)?;
    let from = need(&config, key, by)?;
    let count = config.accounts.len();
    let at = place(position, count).ok_or_else(|| invalid(format!("A position is 1 to {count}.")))?;
    let account = config.accounts.remove(from);
    config.accounts.insert(at, account);
    save_config(paths, &config)?;
    Ok(order_of(&config))
}

/// What to change of an account; what is None stays as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountChanges {
    /// Its mark's word.
    pub mark: Option<String>,
    /// Whether it is a last resort: true marks it so, false makes a last resort normal again.
    pub last_resort: Option<bool>,
    /// Its own 5-hour threshold; null clears it back to the policy's.
    pub five_hour: Option<Value>,
    /// Its own weekly threshold; null clears it back to the policy's.
    pub weekly: Option<Value>,
}

/// An account that was changed.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Changed {
    /// The account as it is now.
    #[serde(serialize_with = "ser_account")]
    pub account: Account,
}

/// Changes an account's mark and threshold overrides.
pub fn set_account(
    paths: &Paths,
    key: &str,
    changes: &AccountChanges,
    by: By,
) -> Result<Changed, AccountError> {
    let (_lock, mut config) = load_for_change(paths)?;
    let at = need(&config, key, by)?;
    let account = &mut config.accounts[at];
    if let Some(mark) = &changes.mark {
        account.mark = check_mark(mark)?;
    }
    match changes.last_resort {
        Some(true) => account.mark = Mark::LastResort,
        Some(false) if account.mark == Mark::LastResort => account.mark = Mark::Normal,
        _ => {}
    }
    if let Some(value) = &changes.five_hour {
        account.five_hour = check_threshold(value, "--five-hour")?;
    }
    if let Some(value) = &changes.weekly {
        account.weekly = check_threshold(value, "--weekly")?;
    }
    let account = account.clone();
    save_config(paths, &config)?;
    Ok(Changed { account })
}

/// The policy settings the operator changed, after a change.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PolicySet {
    /// Every setting that differs from its default, by name.
    pub policy: Object,
}

/// Changes policy settings: each value is checked and kept, null puts a setting back to its default.
pub fn set_policy(paths: &Paths, changes: &Object) -> Result<PolicySet, AccountError> {
    let (_lock, mut config) = load_for_change(paths)?;
    for (key, value) in changes {
        if value.is_null() {
            config.policy.shift_remove(key);
        } else {
            let checked = check_policy_value(key, value).map_err(|error| invalid(error.0))?;
            config.policy.insert(key.clone(), checked);
        }
    }
    save_config(paths, &config)?;
    Ok(PolicySet {
        policy: config.policy,
    })
}
