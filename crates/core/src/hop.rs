//! `baton hop` and the /hop command: ask the wrapper running a session to move it at the end of the
//! current turn (now, if it is between turns).
//!
//! The session comes from the argument, else the wrapper of the calling process (`BATON_WRAPPER`, set
//! for everything claude runs), else Claude Code's `CLAUDE_CODE_SESSION_ID`, else the only session
//! running under baton.

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::clock::{Ms, ms_of};
use crate::config::{Account, By, Config, find_account, name_of};
use crate::env::Env;
use crate::fsutil::read_json;
use crate::json::Get;
use crate::names::same_name;
use crate::paths::Paths;
use crate::policy::{hop_run, pick_hop, states_for};
use crate::sessions::{
    list_wrappers, lock_holder, read_session_file, request_hop, valid_id, wrapper_file, wrapper_session_file,
};
use crate::state::{Want, load_snapshot};

/// Why a conversation cannot be moved.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{message}")]
pub struct HopError {
    /// Why, for the operator: it may name an account.
    pub message: String,
    /// The same with no account's name, when `message` has one.
    unnamed: Option<String>,
}

impl HopError {
    /// A reason that names no account.
    fn new(message: impl Into<String>) -> HopError {
        HopError {
            message: message.into(),
            unnamed: None,
        }
    }

    /// A reason that names an account, and the same without the name.
    fn naming(message: impl Into<String>, unnamed: &str) -> HopError {
        HopError {
            message: message.into(),
            unnamed: Some(unnamed.to_owned()),
        }
    }

    /// Why, for the model (the /hop command hands Baton's answer to it): no account's name, but for
    /// one the operator typed after /hop.
    pub fn unnamed(&self) -> &str {
        self.unnamed.as_deref().unwrap_or(&self.message)
    }
}

/// The session a hop is about.
pub fn resolve_session(paths: &Paths, session: Option<&str>, env: &Env) -> Result<String, HopError> {
    if let Some(session) = session.filter(|s| !s.is_empty()) {
        if !valid_id(session) {
            return Err(HopError::new(format!("Not a session id: {session}")));
        }
        return Ok(session.to_owned());
    }
    if let Some(wrapper) = env.get("BATON_WRAPPER").filter(|w| valid_id(w)) {
        // The conversation the wrapper follows; its mapping only before the wrapper has taken it up (a
        // claude started inside the session writes the mapping too, and the wrapper does not follow
        // that one).
        let session_of = |file: std::path::PathBuf| {
            read_json(&file)
                .and_then(|record| record.text("session").filter(|s| valid_id(s)).map(str::to_owned))
        };
        let held = session_of(wrapper_file(paths, wrapper));
        if let Some(session) = held.or_else(|| session_of(wrapper_session_file(paths, wrapper))) {
            return Ok(session);
        }
    }
    if let Some(session) = env.get("CLAUDE_CODE_SESSION_ID").filter(|s| valid_id(s)) {
        return Ok(session.to_owned());
    }
    let running: Vec<String> = list_wrappers(paths)
        .iter()
        .filter_map(|w| w.word("session").map(str::to_owned))
        .collect();
    match running.as_slice() {
        [only] => Ok(only.clone()),
        [] => Err(HopError::new(
            "/hop and baton hop only work in sessions started with baton, and none is running.",
        )),
        several => Err(HopError::new(format!(
            "Several conversations run under baton; name one: {}",
            several.join(", ")
        ))),
    }
}

/// A hop that was asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hop {
    /// The conversation.
    pub session: String,
    /// The account it runs on, by name.
    pub from: String,
    /// The account it will go to, by name.
    pub to: String,
    /// The id of the account it runs on.
    #[serde(rename = "fromId")]
    pub from_id: String,
    /// The id of the account it will go to.
    #[serde(rename = "toId")]
    pub to_id: String,
    /// Whether the operator named the account.
    pub explicit: bool,
    /// Whether the conversation is between turns (it moves now).
    pub between: bool,
}

/// What a hop is asked with.
#[derive(Clone, Debug)]
pub struct HopRequest<'a> {
    /// The conversation, when named.
    pub session: Option<&'a str>,
    /// The account to move to, when named.
    pub to: Option<&'a str>,
    /// How `to` names it: by name (or id) from the command line, by id from the menu.
    pub to_by: By,
    /// The caller's environment.
    pub env: &'a Env,
    /// Who asks: `cli`, `command`, `menu`.
    pub by: &'a str,
    /// The time now.
    pub now: Ms,
}

/// The account whose name is `key` once the apostrophes are left out of both: the `/hop` command
/// cannot carry one through the shell it runs in, so `Janes account` finds `Jane's account`. Only
/// when one account's name fits, and only for a name (an id has none).
fn without_apostrophes<'a>(config: &'a Config, key: &str, by: By) -> Option<&'a Account> {
    if by != By::Name {
        return None;
    }
    let plain = |name: &str| -> String { name.chars().filter(|c| !matches!(c, '\'' | '\u{2019}')).collect() };
    let wanted = plain(key);
    let mut found = config
        .accounts
        .iter()
        .filter(|account| same_name(&plain(&account.name), &wanted));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// Asks the wrapper running a session to move it. The request carries the account's id; the result
/// names both accounts by their current names.
pub fn hop(paths: &Paths, request: &HopRequest<'_>) -> Result<Hop, HopError> {
    let id = resolve_session(paths, request.session, request.env)?;
    let Some(holder) = lock_holder(paths, &id) else {
        return Err(HopError::new(format!(
            "/hop and baton hop only work in sessions started with baton; this conversation ({id}) was not, so Baton leaves it alone."
        )));
    };
    let wrapper = holder
        .text("wrapper")
        .and_then(|w| read_json(&wrapper_file(paths, w)));
    let from = wrapper
        .as_ref()
        .and_then(|w| w.text("account"))
        .or_else(|| holder.text("account"))
        .unwrap_or_default()
        .to_owned();
    let snapshot = load_snapshot(paths, request.now, Want::default());
    let label = |id: &str| name_of(&snapshot.config, id);
    let mut target = None;
    if let Some(to) = request.to.filter(|to| !to.is_empty()) {
        let Some(account) = find_account(&snapshot.config, to, request.to_by)
            .or_else(|| without_apostrophes(&snapshot.config, to, request.to_by))
        else {
            return Err(HopError::new(format!("No account named {to}.")));
        };
        target = Some(account.id.clone());
    }
    if target.as_deref() == Some(from.as_str()) {
        return Err(HopError::naming(
            format!("The conversation already runs on {}.", label(&from)),
            "The conversation already runs on that account.",
        ));
    }
    let predicted = target.clone().or_else(|| {
        let model =
            read_session_file(paths, &id, "live.json").and_then(|live| live.word("model").map(str::to_owned));
        let states = states_for(&snapshot.states, model.as_deref(), snapshot.now);
        // As the wrapper will decide it: not back to an account its run of hops has left.
        let left: Vec<String> = wrapper
            .as_ref()
            .and_then(|w| w.at("left").as_array())
            .map(|left| left.iter().filter_map(Value::as_str).map(str::to_owned).collect())
            .unwrap_or_default();
        let arrived = wrapper.as_ref().and_then(|w| ms_of(w.at("arrivedAt")));
        let ended_well = wrapper.as_ref().is_some_and(|w| w.truthy("endedWell"));
        let left = hop_run(&left, arrived, ended_well, &snapshot.policy, snapshot.now);
        pick_hop(&states, &snapshot.policy, &from, left).account
    });
    let Some(predicted) = predicted else {
        return Err(HopError::naming(
            format!(
                "Every other account is at its limit; {id} stays on {}.",
                label(&from)
            ),
            "Every other account is at its limit; the conversation stays where it is.",
        ));
    };
    request_hop(paths, &id, target.as_deref(), request.by, request.now)
        .map_err(|error| HopError::new(format!("Could not ask for the move: {error}")))?;
    let turn = read_session_file(paths, &id, "turn.json");
    Ok(Hop {
        from: label(&from),
        to: label(&predicted),
        from_id: from,
        to_id: predicted,
        explicit: target.is_some(),
        between: turn.as_ref().and_then(|t| t.text("state")) != Some("running"),
        session: id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsutil::write_json_atomic;
    use crate::paths::baton_paths;
    use crate::sessions::{acquire_lock, write_session_file};
    use serde_json::{Value, json};

    const ID: &str = "6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f";

    fn me() -> u32 {
        std::process::id()
    }

    fn home() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        write_json_atomic(
            &paths.config,
            &json!({ "accounts": [{ "id": "work", "name": "Work" }, { "id": "spare", "name": "Spare" }] }),
        )
        .unwrap();
        (dir, paths)
    }

    fn request<'a>(env: &'a Env, session: Option<&'a str>, to: Option<&'a str>) -> HopRequest<'a> {
        HopRequest {
            session,
            to,
            to_by: By::Name,
            env,
            by: "cli",
            now: 1_780_000_000_000,
        }
    }

    #[test]
    fn which_session() {
        let (_dir, paths) = home();
        let env = Env::new();
        assert_eq!(resolve_session(&paths, Some(ID), &env), Ok(ID.to_owned()));
        assert_eq!(
            resolve_session(&paths, Some("no/id"), &env).unwrap_err().message,
            "Not a session id: no/id"
        );
        assert!(
            resolve_session(&paths, None, &env)
                .unwrap_err()
                .message
                .contains("none is running")
        );
        let in_claude = Env::from_pairs([("CLAUDE_CODE_SESSION_ID", ID)]);
        assert_eq!(resolve_session(&paths, None, &in_claude), Ok(ID.to_owned()));
        // The wrapper's own conversation wins over the session a claude inside it reports.
        write_json_atomic(
            &wrapper_file(&paths, "w1"),
            &json!({ "pid": me(), "wrapper": "w1", "session": "held" }),
        )
        .unwrap();
        write_json_atomic(
            &wrapper_session_file(&paths, "w1"),
            &json!({ "session": "mapped" }),
        )
        .unwrap();
        let wrapped = Env::from_pairs([("BATON_WRAPPER", "w1"), ("CLAUDE_CODE_SESSION_ID", ID)]);
        assert_eq!(resolve_session(&paths, None, &wrapped), Ok("held".to_owned()));
        // With nothing else to go by: the only session under baton.
        assert_eq!(resolve_session(&paths, None, &env), Ok("held".to_owned()));
        write_json_atomic(
            &wrapper_file(&paths, "w2"),
            &json!({ "pid": me(), "wrapper": "w2", "session": "other" }),
        )
        .unwrap();
        assert_eq!(
            resolve_session(&paths, None, &env).unwrap_err().message,
            "Several conversations run under baton; name one: held, other"
        );
    }

    #[test]
    fn a_name_typed_without_its_apostrophe_finds_the_account() {
        let config = crate::config::normalize_config(&json!({ "accounts": [
            { "id": "jane", "name": "Jane's account" },
            { "id": "other", "name": "Janes other" },
            { "id": "a", "name": "It's" },
            { "id": "b", "name": "Its" }
        ] }));
        let found = |key: &str| without_apostrophes(&config, key, By::Name).map(|a| a.id.as_str());
        assert_eq!(found("Janes account"), Some("jane"));
        assert_eq!(found("JANES ACCOUNT"), Some("jane"));
        assert_eq!(found("Jane\u{2019}s account"), Some("jane"));
        assert_eq!(found("Jane"), None);
        // Two names that are one without the apostrophe: neither is guessed.
        assert_eq!(found("Its"), None);
        assert_eq!(without_apostrophes(&config, "Janes account", By::Id), None);
    }

    #[test]
    fn asking_for_a_hop() {
        let (_dir, paths) = home();
        let env = Env::new();
        let not_ours = hop(&paths, &request(&env, Some(ID), None)).unwrap_err().message;
        assert!(
            not_ours.contains("was not, so Baton leaves it alone"),
            "{not_ours}"
        );
        let _held = acquire_lock(
            &paths,
            ID,
            &json!({ "pid": me(), "wrapper": "w1", "account": "work" }),
        )
        .taken()
        .unwrap();
        write_session_file(&paths, ID, "turn.json", &json!({ "state": "running" })).unwrap();
        let done = hop(&paths, &request(&env, Some(ID), None)).unwrap();
        assert_eq!(
            done,
            Hop {
                session: ID.to_owned(),
                from: "Work".into(),
                to: "Spare".into(),
                from_id: "work".into(),
                to_id: "spare".into(),
                explicit: false,
                between: false
            }
        );
        assert_eq!(
            read_session_file(&paths, ID, "hop.json").unwrap()["to"],
            json!(null)
        );
        let named = hop(&paths, &request(&env, Some(ID), Some("SPARE"))).unwrap();
        assert!(named.explicit);
        assert_eq!(
            read_session_file(&paths, ID, "hop.json").unwrap()["to"],
            json!("spare")
        );
        // On the account it already runs on: said with its name to the operator, without to the model.
        let same = hop(&paths, &request(&env, Some(ID), Some("work"))).unwrap_err();
        assert_eq!(same.message, "The conversation already runs on Work.");
        assert_eq!(same.unnamed(), "The conversation already runs on that account.");
        // The name the operator typed is theirs to read back, wherever the answer goes.
        let nobody = hop(&paths, &request(&env, Some(ID), Some("nobody"))).unwrap_err();
        assert_eq!(nobody.message, "No account named nobody.");
        assert_eq!(nobody.unnamed(), "No account named nobody.");
        // By id (the menu): an id in any case, since it names a folder; never a name.
        let by_id = |to: &'static str| {
            let asked = HopRequest {
                to_by: By::Id,
                ..request(&env, Some(ID), Some(to))
            };
            hop(&paths, &asked).unwrap_err().message
        };
        assert_eq!(by_id("WORK"), "The conversation already runs on Work.");
        assert_eq!(by_id("The spare"), "No account named The spare.");
        // The wrapper's record names the account the conversation is on now.
        write_json_atomic(
            &wrapper_file(&paths, "w1"),
            &json!({ "pid": me(), "wrapper": "w1", "account": "spare" }),
        )
        .unwrap();
        write_session_file(&paths, ID, "turn.json", &json!({ "state": "idle" })).unwrap();
        let back = hop(&paths, &request(&env, Some(ID), None)).unwrap();
        assert_eq!(
            (back.from.as_str(), back.to.as_str(), back.between),
            ("Spare", "Work", true)
        );
    }

    #[test]
    fn a_hop_in_a_run_of_hops_goes_on_to_the_next_account_not_back() {
        let (_dir, paths) = home();
        write_json_atomic(
            &paths.config,
            &json!({ "accounts": [
                { "id": "work", "name": "Work" },
                { "id": "spare", "name": "Spare" },
                { "id": "third", "name": "Third" },
            ] }),
        )
        .unwrap();
        let env = Env::new();
        let now = request(&env, None, None).now;
        let _held = acquire_lock(
            &paths,
            ID,
            &json!({ "pid": me(), "wrapper": "w1", "account": "work" }),
        )
        .taken()
        .unwrap();
        let to = |record: Value| {
            write_json_atomic(&wrapper_file(&paths, "w1"), &record).unwrap();
            hop(&paths, &request(&env, Some(ID), None)).unwrap().to
        };
        let on_spare = |left: Value, arrived: Ms, ended_well: bool| {
            json!({
                "pid": me(), "wrapper": "w1", "account": "spare",
                "left": left, "arrivedAt": crate::clock::iso(arrived), "endedWell": ended_well,
            })
        };
        // It came from Work a minute ago: on to Third, as the wrapper will decide.
        assert_eq!(to(on_spare(json!(["work"]), now - 60_000, true)), "Third");
        // No turn has ended well on Spare, however long ago it arrived.
        assert_eq!(to(on_spare(json!(["work"]), now - 3 * 3_600_000, false)), "Third");
        // Settled on Spare: Work is the best other account again.
        assert_eq!(to(on_spare(json!(["work"]), now - 3 * 3_600_000, true)), "Work");
        // Through both: back to the one it left longest ago.
        assert_eq!(
            to(on_spare(json!(["third", "work"]), now - 60_000, false)),
            "Third"
        );
        // A record from before runs of hops were kept.
        assert_eq!(
            to(json!({ "pid": me(), "wrapper": "w1", "account": "spare" })),
            "Work"
        );
    }
}
