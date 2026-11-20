//! The environment a command runs with, as a value: the process's own, or one a test made.
//!
//! Baton reads its settings from it (`BATON_HOME`, `NO_COLOR`, ...) and hands a changed copy to every
//! `claude` it starts (`CLAUDE_CONFIG_DIR` set to the account's profile). On Windows a variable is found
//! whatever the case of its name, as the system itself does.

use std::ffi::{OsStr, OsString};
use std::process::Command;

/// A set of environment variables. Names keep the case they were given.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Env {
    /// The variables, in the order they were added.
    vars: Vec<(String, OsString)>,
}

/// Whether two variable names are the same variable on this system.
fn same_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

impl Env {
    /// An empty environment.
    pub fn new() -> Env {
        Env::default()
    }

    /// The environment of this process. A variable whose name is not valid Unicode is left out.
    pub fn from_process() -> Env {
        let mut env = Env::new();
        for (name, value) in std::env::vars_os() {
            if let Ok(name) = name.into_string() {
                env.vars.push((name, value));
            }
        }
        env
    }

    /// An environment of these variables.
    pub fn from_pairs<I, K, V>(pairs: I) -> Env
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<OsString>,
    {
        let mut env = Env::new();
        for (name, value) in pairs {
            env.set(name, value);
        }
        env
    }

    /// The value of `name`, when it is set and valid Unicode. An empty value is returned as it is.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.get_os(name).and_then(OsStr::to_str)
    }

    /// The value of `name`, when it is set to something other than the empty string.
    pub fn some(&self, name: &str) -> Option<&str> {
        self.get(name).filter(|value| !value.is_empty())
    }

    /// Whether `name` is set to something other than the empty string.
    pub fn has(&self, name: &str) -> bool {
        self.get_os(name).is_some_and(|value| !value.is_empty())
    }

    /// The value of `name` as the system holds it.
    pub fn get_os(&self, name: &str) -> Option<&OsStr> {
        self.vars
            .iter()
            .find(|(key, _)| same_name(key, name))
            .map(|(_, value)| value.as_os_str())
    }

    /// Sets `name`, replacing the value it had.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<OsString>) -> &mut Env {
        let name = name.into();
        let value = value.into();
        match self.vars.iter_mut().find(|(key, _)| same_name(key, &name)) {
            Some(entry) => entry.1 = value,
            None => self.vars.push((name, value)),
        }
        self
    }

    /// A copy with `name` set.
    pub fn with(mut self, name: impl Into<String>, value: impl Into<OsString>) -> Env {
        self.set(name, value);
        self
    }

    /// Removes `name`.
    pub fn remove(&mut self, name: &str) -> &mut Env {
        self.vars.retain(|(key, _)| !same_name(key, name));
        self
    }

    /// Removes every variable whose name is `name` in any case, on every system.
    pub fn remove_any_case(&mut self, name: &str) -> &mut Env {
        self.vars.retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        self
    }

    /// Keeps only the variables whose name passes `keep`.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) -> &mut Env {
        self.vars.retain(|(key, _)| keep(key));
        self
    }

    /// The names of the variables.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.vars.iter().map(|(key, _)| key.as_str())
    }

    /// The variables.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &OsStr)> {
        self.vars
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_os_str()))
    }

    /// Makes `command` run with exactly this environment.
    pub fn apply(&self, command: &mut Command) {
        command.env_clear();
        for (name, value) in &self.vars {
            command.env(name, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_and_remove() {
        let mut env = Env::new();
        env.set("A", "1").set("B", "");
        assert_eq!(env.get("A"), Some("1"));
        assert_eq!(env.get("B"), Some(""));
        assert_eq!(env.some("B"), None);
        assert!(env.has("A"));
        assert!(!env.has("B"));
        assert!(!env.has("C"));
        env.set("A", "2");
        assert_eq!(env.get("A"), Some("2"));
        assert_eq!(env.names().count(), 2);
        env.remove("A");
        assert_eq!(env.get("A"), None);
    }

    #[test]
    fn names_are_found_in_any_case_where_the_system_does() {
        let env = Env::from_pairs([("Path", "x")]);
        assert_eq!(env.get("PATH").is_some(), cfg!(windows));
        let mut env = Env::from_pairs([("Claude_Config_Dir", "x"), ("OTHER", "y")]);
        env.remove_any_case("CLAUDE_CONFIG_DIR");
        assert_eq!(env.names().collect::<Vec<_>>(), ["OTHER"]);
    }
}
