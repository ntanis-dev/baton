//! Whether a profile is signed in, from `claude auth status --json`, of which only loggedIn and
//! authMethod are kept: no email, no organization.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use baton_core::claude_path::Launcher;
use baton_core::env::Env;
use baton_core::json::Get;
use serde_json::Value;

use crate::proc::run_captured;

/// `claude auth status` is ended after this long.
const AUTH_TIMEOUT: Duration = Duration::from_secs(30);

/// What `claude auth status --json` said of a profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthStatus {
    /// Whether Claude Code answered at all.
    pub ok: bool,
    /// Whether the profile is signed in.
    pub logged_in: bool,
    /// How it is signed in (`claude.ai`, `none`, ...).
    pub auth_method: Option<String>,
    /// Why there is no answer, when there is none.
    pub error: Option<String>,
}

impl AuthStatus {
    /// No answer, for this reason.
    fn failed(error: impl Into<String>) -> AuthStatus {
        let error: String = error.into();
        AuthStatus {
            error: Some(error.trim().chars().take(200).collect()),
            ..AuthStatus::default()
        }
    }

    /// Whether Claude Code answered and said the profile is signed out.
    pub fn signed_out(&self) -> bool {
        self.ok && !self.logged_in
    }
}

/// Claude Code's answer, read. None when it is not one.
fn parse_auth(stdout: &str) -> Option<AuthStatus> {
    let data: Value = serde_json::from_str(stdout).ok()?;
    if data.is_null() {
        return None;
    }
    Some(AuthStatus {
        ok: true,
        logged_in: data.flag("loggedIn"),
        auth_method: data.text("authMethod").map(str::to_owned),
        error: None,
    })
}

/// Asks Claude Code whether the profile of `env` (its `CLAUDE_CONFIG_DIR`) is signed in. `low` runs it
/// at a low priority; setting `stop` calls it off. Never fails: no answer is `ok: false` with why.
pub fn auth_status_with(launcher: &Launcher, env: &Env, low: bool, stop: Option<&AtomicBool>) -> AuthStatus {
    let args: Vec<String> = ["auth", "status", "--json"].map(str::to_owned).to_vec();
    let mut command = launcher.command(&args);
    env.apply(&mut command);
    let done = match run_captured(&mut command, AUTH_TIMEOUT, stop, low) {
        Ok(done) => done,
        Err(error) => return AuthStatus::failed(error.to_string()),
    };
    if let Some(status) = parse_auth(&done.stdout) {
        return status;
    }
    if done.timed_out {
        return AuthStatus::failed("claude auth status did not answer in 30 s");
    }
    if done.stderr.trim().is_empty() {
        return AuthStatus::failed("no output");
    }
    AuthStatus::failed(done.stderr)
}

/// Asks Claude Code whether the profile of `env` is signed in, waiting for the answer.
pub fn auth_status(launcher: &Launcher, env: &Env) -> AuthStatus {
    auth_status_with(launcher, env, false, None)
}

/// The same without blocking (the menu): the answer arrives on the channel.
pub fn auth_status_async(launcher: &Launcher, env: &Env, low: bool) -> Receiver<AuthStatus> {
    let (tx, rx) = mpsc::channel();
    let (launcher, env) = (launcher.clone(), env.clone());
    thread::spawn(move || {
        let _ = tx.send(auth_status_with(&launcher, &env, low, None));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_sign_in_is_kept() {
        let said =
            parse_auth(r#"{"loggedIn":true,"authMethod":"claude.ai","email":"someone@example.invalid"}"#);
        assert_eq!(
            said,
            Some(AuthStatus {
                ok: true,
                logged_in: true,
                auth_method: Some("claude.ai".into()),
                error: None
            })
        );
        let out = parse_auth(r#"{"loggedIn":"yes"}"#).unwrap();
        assert!(out.ok && !out.logged_in && out.signed_out());
        assert_eq!(out.auth_method, None);
        assert_eq!(parse_auth(""), None);
        assert_eq!(parse_auth("null"), None);
        assert_eq!(parse_auth("not json"), None);
    }

    #[test]
    fn a_claude_that_is_not_there_is_no_answer() {
        let launcher = Launcher::program("baton-no-such-claude-anywhere");
        let status = auth_status(&launcher, &Env::new());
        assert!(!status.ok && !status.signed_out());
        assert!(status.error.is_some_and(|error| !error.is_empty()));
    }
}
