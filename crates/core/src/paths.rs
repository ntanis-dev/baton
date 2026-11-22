//! Where everything lives.
//!
//! "home" is the user's home directory (`BATON_HOME` overrides it, which the tests use). Baton's accounts
//! are only the ones the operator adds, each a Claude Code profile in `~/.baton/accounts/<id>`; Baton
//! never interacts with the default Claude Code account (`~/.claude` without `CLAUDE_CONFIG_DIR`):
//! `~/.claude` is only the shared base the profiles link to (projects, memory, settings, ...), and
//! `~/.claude.json` the source of the non-identity state a new profile is seeded with. Baton's own state
//! is in `~/.baton`.

use std::path::{Component, Path, PathBuf};

use crate::env::Env;

/// Whether `id` can name a session or a wrapper: a letter or digit, then up to 127 letters, digits,
/// `_` or `-`.
pub fn is_id(id: &str) -> bool {
    is_word(id, 128)
}

/// Whether `id` can be an account's stable id (its profile folder's name): as [`is_id`], at most 64
/// characters.
pub fn is_account_id(id: &str) -> bool {
    is_word(id, 64)
}

/// A letter or digit, then letters, digits, `_` or `-`, at most `max` characters in all.
fn is_word(id: &str, max: usize) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= max
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// A path made absolute against the working folder, with `.` and `..` resolved by name (nothing on
/// disk is looked at).
pub fn resolve(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The home folder Baton works under: `BATON_HOME`, else the user's own.
pub fn resolve_home(env: &Env) -> PathBuf {
    match env.some("BATON_HOME") {
        Some(home) => resolve(home),
        None => resolve(std::env::home_dir().unwrap_or_else(|| PathBuf::from("."))),
    }
}

/// Why a build made while developing Baton does not run: nothing says where its home is, so it would
/// work on the developer's own accounts and `~/.claude`. Such a build (`dev`: one with debug
/// assertions, what `cargo run` and `cargo test` make) runs only with `BATON_HOME` set, whatever it is
/// set to; an installed build (`cargo install`, a release) is never held back. None when it may run.
pub fn dev_refusal(dev: bool, env: &Env) -> Option<String> {
    if !dev || env.some("BATON_HOME").is_some() {
        return None;
    }
    Some(
        "baton: this is a development build. It runs only with BATON_HOME set, so that it never changes \
         your own accounts and ~/.claude by accident. Point BATON_HOME at a scratch folder, or install a \
         release build (cargo install --locked --path crates/cli)."
            .to_owned(),
    )
}

/// Every folder and file Baton knows by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// The user's home folder.
    pub home: PathBuf,
    /// `~/.baton`.
    pub root: PathBuf,
    /// The accounts and the policy.
    pub config: PathBuf,
    /// What `baton install` did.
    pub install: PathBuf,
    /// The profiles, one folder per account id.
    pub accounts: PathBuf,
    /// The latest reading per account.
    pub usage: PathBuf,
    /// The last refusal per account and window.
    pub rejected: PathBuf,
    /// Per-conversation state.
    pub sessions: PathBuf,
    /// The running session wrappers.
    pub wrappers: PathBuf,
    /// Append-only readings and events.
    pub history: PathBuf,
    /// Backups of what Baton replaced.
    pub backups: PathBuf,
    /// The wrappers' logs.
    pub logs: PathBuf,
    /// Scratch files.
    pub tmp: PathBuf,
    /// The files Baton's locks are taken on ([`crate::oslock`]).
    pub run: PathBuf,
    /// The shared base every profile links to (never signed in to by Baton).
    pub claude_dir: PathBuf,
    /// The seed of a new profile's `.claude.json` (never its identity, never its sign-in).
    pub claude_json: PathBuf,
}

/// The paths under `home`.
pub fn baton_paths(home: impl Into<PathBuf>) -> Paths {
    let home = home.into();
    let root = home.join(".baton");
    Paths {
        config: root.join("config.json"),
        install: root.join("install.json"),
        accounts: root.join("accounts"),
        usage: root.join("usage"),
        rejected: root.join("rejected"),
        sessions: root.join("sessions"),
        wrappers: root.join("wrappers"),
        history: root.join("history"),
        backups: root.join("backups"),
        logs: root.join("logs"),
        tmp: root.join("tmp"),
        run: root.join("run"),
        claude_dir: home.join(".claude"),
        claude_json: home.join(".claude.json"),
        root,
        home,
    }
}

/// The paths for an environment.
pub fn paths_for(env: &Env) -> Paths {
    baton_paths(resolve_home(env))
}

/// An account's profile folder.
pub fn profile_dir(paths: &Paths, id: &str) -> PathBuf {
    paths.accounts.join(id)
}

/// The environment Claude Code runs with under an account's profile: its `CLAUDE_CONFIG_DIR`, whatever
/// was set before. Baton never runs claude without one.
pub fn profile_env(paths: &Paths, id: &str, env: &Env) -> Env {
    let mut out = env.clone();
    out.remove_any_case("CLAUDE_CONFIG_DIR");
    out.set("CLAUDE_CONFIG_DIR", profile_dir(paths, id).into_os_string());
    out
}

/// Claude Code keeps a profile's global config (onboarding, trust, MCP servers, the signed-in account's
/// identity) in `<CLAUDE_CONFIG_DIR>/.claude.json`.
pub fn global_config_file(paths: &Paths, id: &str) -> PathBuf {
    profile_dir(paths, id).join(".claude.json")
}

/// A path as text for comparing: resolved, and in lower case where the file system ignores case.
fn key(path: &Path) -> String {
    let text = resolve(path).to_string_lossy().into_owned();
    if cfg!(windows) { text.to_lowercase() } else { text }
}

/// Whether two paths name the same place (by name; nothing on disk is looked at).
pub fn same_path(a: impl AsRef<Path>, b: impl AsRef<Path>) -> bool {
    key(a.as_ref()) == key(b.as_ref())
}

/// Whether `child` is `parent` or somewhere inside it.
pub fn is_inside(child: impl AsRef<Path>, parent: impl AsRef<Path>) -> bool {
    let sep = std::path::MAIN_SEPARATOR;
    let with_sep = |path: &Path| {
        let mut text = key(path);
        if !text.ends_with(sep) {
            text.push(sep);
        }
        text
    };
    with_sep(child.as_ref()).starts_with(&with_sep(parent.as_ref()))
}

/// Paths written into commands Claude Code runs through Git Bash need forward slashes (Git Bash eats
/// unquoted backslashes).
pub fn forward(path: impl AsRef<Path>) -> String {
    let text = path.as_ref().to_string_lossy().into_owned();
    if std::path::MAIN_SEPARATOR == '/' {
        text
    } else {
        text.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

/// The profile (its account id) a Claude Code process runs under, from its `CLAUDE_CONFIG_DIR`: a folder
/// in `~/.baton/accounts` is its name; anything else (plain claude without `CLAUDE_CONFIG_DIR`,
/// `~/.claude`, any other folder) is not Baton's: None. A rename changes only the label, so a running
/// claude keeps resolving to the same account.
pub fn account_from_env(env: &Env, paths: &Paths) -> Option<String> {
    let dir = env.some("CLAUDE_CONFIG_DIR")?;
    let resolved = resolve(dir);
    let parent = resolved.parent()?;
    if !same_path(parent, &paths.accounts) {
        return None;
    }
    let name = resolved.file_name()?.to_str()?;
    is_account_id(name).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_development_build_runs_only_where_its_home_is_named() {
        let named = Env::from_pairs([("BATON_HOME", "scratch")]);
        let unnamed = Env::from_pairs([("HOME", "/home/me")]);
        assert_eq!(dev_refusal(true, &named), None);
        assert_eq!(dev_refusal(false, &unnamed), None, "an installed build");
        let refusal = dev_refusal(true, &unnamed).unwrap();
        assert!(
            refusal.contains("BATON_HOME") && !refusal.contains("  "),
            "{refusal}"
        );
    }

    #[test]
    fn ids() {
        assert!(is_id("6f0b2d4e-1c3a-4b5d-8e9f-0a1b2c3d4e5f"));
        assert!(is_id("w_1"));
        assert!(!is_id(""));
        assert!(!is_id("-x"));
        assert!(!is_id("a/b"));
        assert!(!is_id("a.b"));
        assert!(is_id(&"a".repeat(128)));
        assert!(!is_id(&"a".repeat(129)));
        assert!(is_account_id(&"a".repeat(64)));
        assert!(!is_account_id(&"a".repeat(65)));
    }

    #[test]
    fn everything_hangs_off_the_home() {
        let paths = baton_paths("/home/op");
        assert_eq!(paths.root, Path::new("/home/op").join(".baton"));
        assert_eq!(paths.config, paths.root.join("config.json"));
        assert_eq!(paths.claude_dir, Path::new("/home/op").join(".claude"));
        assert_eq!(profile_dir(&paths, "work"), paths.accounts.join("work"));
        assert_eq!(
            global_config_file(&paths, "work"),
            paths.accounts.join("work").join(".claude.json")
        );
    }

    #[test]
    fn baton_home_overrides_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::from_pairs([("BATON_HOME", dir.path().as_os_str().to_owned())]);
        assert!(same_path(resolve_home(&env), dir.path()));
        assert!(same_path(paths_for(&env).root, dir.path().join(".baton")));
    }

    #[test]
    fn the_profile_env_replaces_any_config_dir() {
        let paths = baton_paths(resolve("home"));
        let env = Env::from_pairs([("Claude_Config_Dir", "elsewhere"), ("KEEP", "1")]);
        let out = profile_env(&paths, "work", &env);
        assert_eq!(out.get("KEEP"), Some("1"));
        assert_eq!(
            out.names()
                .filter(|n| n.eq_ignore_ascii_case("CLAUDE_CONFIG_DIR"))
                .count(),
            1
        );
        assert_eq!(
            out.get_os("CLAUDE_CONFIG_DIR"),
            Some(profile_dir(&paths, "work").as_os_str())
        );
    }

    #[test]
    fn resolving_and_comparing_paths() {
        let base = resolve("a/b");
        assert_eq!(resolve("a/./b/../b"), base);
        assert!(same_path("a/b", "a/b/../b"));
        assert_eq!(same_path("a/B", "a/b"), cfg!(windows));
        assert!(is_inside("a/b/c", "a/b"));
        assert!(is_inside("a/b", "a/b"));
        assert!(!is_inside("a/bc", "a/b"));
        assert!(!is_inside("a", "a/b"));
    }

    #[test]
    fn the_account_of_a_config_dir() {
        let paths = baton_paths(resolve("home"));
        let at = |dir: PathBuf| {
            account_from_env(
                &Env::from_pairs([("CLAUDE_CONFIG_DIR", dir.into_os_string())]),
                &paths,
            )
        };
        assert_eq!(at(paths.accounts.join("work")), Some("work".to_owned()));
        assert_eq!(at(paths.accounts.join("work").join("deeper")), None);
        assert_eq!(at(paths.claude_dir.clone()), None);
        assert_eq!(at(paths.accounts.join("not an id")), None);
        assert_eq!(account_from_env(&Env::new(), &paths), None);
    }

    #[test]
    fn forward_slashes() {
        let path = Path::new("a").join("b").join("c.js");
        assert_eq!(forward(&path), "a/b/c.js");
    }
}
