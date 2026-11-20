//! Test support for Baton: temporary homes that are never the real one, an environment that points
//! at them, helpers to write files and wait for things, and stand-ins for the `claude` executable
//! (the `fake_*` modules, each built as a program of its own and found through [`fixtures`]).
//!
//! Everything here runs inside tests, where a failure to set the stage should stop the test at once.
#![allow(clippy::expect_used, clippy::unwrap_used)]

pub mod fake_claude;
pub mod fake_login;
pub mod fake_usage;
pub mod fixtures;
pub mod raw_reader;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use baton_core::env::Env;
use baton_core::paths::{Paths, baton_paths};
use serde_json::{Value, json};

/// One hour, in milliseconds.
pub const H: i64 = 3_600_000;
/// One minute, in milliseconds.
pub const M: i64 = 60_000;

/// A temporary home: removed again when dropped.
#[derive(Debug)]
pub struct TestHome {
    /// The folder that holds everything; removed when this value is dropped.
    pub root: tempfile::TempDir,
    /// The home folder inside it.
    pub home: PathBuf,
    /// Baton's paths under that home.
    pub paths: Paths,
}

/// A temporary home with an empty `~/.claude`.
pub fn temp_home() -> TestHome {
    temp_home_with(true)
}

/// A temporary home, with or without `~/.claude`.
pub fn temp_home_with(claude: bool) -> TestHome {
    let root = tempfile::Builder::new()
        .prefix("baton-test-")
        .tempdir()
        .expect("a temporary folder");
    // The real path: on macOS the temporary folder is behind a link, and Windows may hand out a short name.
    let base = fs::canonicalize(root.path()).expect("the temporary folder's real path");
    let home = strip_verbatim(base).join("home");
    fs::create_dir_all(&home).expect("the test home");
    if claude {
        fs::create_dir_all(home.join(".claude")).expect("the test home's .claude");
    }
    let paths = baton_paths(&home);
    TestHome { root, home, paths }
}

/// A path without the `\\?\` prefix Windows puts on canonical paths.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with("UNC") => PathBuf::from(plain),
        _ => path,
    }
}

/// A temporary home with these accounts added (config version 1, no profiles linked): what most
/// tests start from. Each account's id is its name.
pub fn accounts_home(names: &[&str]) -> TestHome {
    let accounts: Vec<Value> = names
        .iter()
        .map(|name| json!({ "id": name, "name": name, "mark": "normal" }))
        .collect();
    accounts_home_of(accounts)
}

/// A temporary home with the four accounts most tests use: acct1 to acct4.
pub fn four_accounts_home() -> TestHome {
    accounts_home(&["acct1", "acct2", "acct3", "acct4"])
}

/// A temporary home with these accounts as config.json holds them.
pub fn accounts_home_of(accounts: Vec<Value>) -> TestHome {
    let home = temp_home();
    write_json(
        &home.paths.config,
        &json!({ "version": 1, "accounts": accounts, "policy": {} }),
    );
    home
}

/// Writes `content` to `file`, creating its folder.
pub fn write(file: &Path, content: &str) {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).expect("the file's folder");
    }
    fs::write(file, content).expect("the file");
}

/// Writes `value` to `file` as indented JSON, creating its folder.
pub fn write_json(file: &Path, value: &Value) {
    write(file, &serde_json::to_string_pretty(value).expect("JSON"));
}

/// The environment a test runs Baton with: this process's, with the home pointed at `home`, and
/// nothing that says it runs inside Claude Code, a session wrapper or another Ferry.
pub fn test_env(home: &Path) -> Env {
    let mut env = Env::from_process();
    for name in ["CLAUDE_CONFIG_DIR", "FERRY_HOME"] {
        env.remove_any_case(name);
    }
    for name in [
        "BATON_WRAPPER",
        "CLAUDE_CODE_SESSION_ID",
        "BATON_CLAUDE",
        "BATON_GLYPHS",
        "NO_COLOR",
        "FORCE_COLOR",
    ] {
        env.remove(name);
    }
    for name in ["BATON_HOME", "USERPROFILE", "HOME"] {
        env.set(name, home.as_os_str());
    }
    env
}

/// [`test_env`] with more variables set.
pub fn test_env_with(home: &Path, extra: &[(&str, &str)]) -> Env {
    let mut env = test_env(home);
    for (name, value) in extra {
        env.set(*name, *value);
    }
    env
}

/// Waits until `check` returns something, trying every 20 ms for up to 15 seconds.
pub fn until<T>(check: impl FnMut() -> Option<T>) -> T {
    until_for(Duration::from_secs(15), check)
}

/// Waits until `check` returns something, trying every 20 ms for up to `timeout`.
pub fn until_for<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(start.elapsed() <= timeout, "Timed out waiting");
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use baton_core::config::load_config;

    #[test]
    fn a_test_home_is_its_own() {
        let home = four_accounts_home();
        assert!(home.home.join(".claude").is_dir());
        assert_eq!(load_config(&home.paths).accounts.len(), 4);
        assert!(!temp_home_with(false).home.join(".claude").exists());
        let env = test_env_with(&home.home, &[("EXTRA", "1")]);
        assert_eq!(baton_core::paths::paths_for(&env), home.paths);
        assert_eq!(env.get("CLAUDE_CONFIG_DIR"), None);
        assert_eq!(env.get("EXTRA"), Some("1"));
        let mut tries = 0;
        assert_eq!(
            until(|| {
                tries += 1;
                (tries == 3).then_some(tries)
            }),
            3
        );
    }
}
