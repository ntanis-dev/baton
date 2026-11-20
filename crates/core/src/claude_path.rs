//! How to start the Claude Code CLI without a shell.
//!
//! On Windows a name on PATH resolves in PATHEXT order per folder (the WinGet and native installs put a
//! real claude.exe there); an npm .cmd shim cannot be spawned without a shell, so its target is read
//! from the shim and run directly (node + cli.js, or the package's claude.exe). Elsewhere the first
//! executable `claude` on PATH.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;
use thiserror::Error;

use crate::env::Env;

/// Claude Code could not be found, or cannot be run.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct ClaudeNotFound(pub String);

/// Whether `path` is a file.
fn is_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file())
}

/// Whether `path` is a file this user may run.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        is_file(path)
    }
}

/// The first program called `name` on the environment's PATH; on Windows with each PATHEXT extension
/// in turn unless the name has one.
pub fn find_on_path(name: &str, env: &Env) -> Option<PathBuf> {
    let path = env.get("PATH").or_else(|| env.get("Path")).unwrap_or_default();
    let dirs = path
        .split(if cfg!(windows) { ';' } else { ':' })
        .map(|dir| dir.strip_prefix('"').unwrap_or(dir))
        .map(|dir| dir.strip_suffix('"').unwrap_or(dir))
        .filter(|dir| !dir.is_empty());
    if cfg!(windows) {
        let exts: Vec<String> = if Path::new(name).extension().is_some() {
            vec![String::new()]
        } else {
            env.get("PATHEXT")
                .unwrap_or(".COM;.EXE;.BAT;.CMD")
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(str::to_lowercase)
                .collect()
        };
        for dir in dirs {
            for ext in &exts {
                let candidate = Path::new(dir).join(format!("{name}{ext}"));
                if is_file(&candidate) {
                    return Some(candidate);
                }
            }
        }
        return None;
    }
    dirs.map(|dir| Path::new(dir).join(name))
        .find(|candidate| is_executable(candidate))
}

/// Whether a path ends in one of these extensions, ignoring case.
fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| exts.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

/// Whether a path is a script Node runs.
fn is_script(path: &Path) -> bool {
    has_ext(path, &["js", "cjs", "mjs"])
}

/// Whether a path is a shell shim (Windows).
fn is_shim(path: &Path) -> bool {
    cfg!(windows) && has_ext(path, &["cmd", "bat"])
}

/// The target an npm cmd shim runs: `"%dp0%\node_modules\...\cli.js"` or `...\claude.exe`.
pub fn shim_target(shim: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(shim).ok()?;
    let dir = shim.parent()?;
    let pattern = Regex::new(r#"(?i)"?%~?dp0%?\\?([^"%\r\n]+?\.(?:js|cjs|mjs|exe))"?"#).ok()?;
    let found: Vec<PathBuf> = pattern
        .captures_iter(&text)
        .filter_map(|m| m.get(1))
        .map(|m| crate::paths::resolve(dir.join(m.as_str().replace('\\', std::path::MAIN_SEPARATOR_STR))))
        .filter(|target| target.exists())
        .collect();
    if let Some(exe) = found.iter().find(|p| has_ext(p, &["exe"])) {
        return Some(exe.clone());
    }
    if let Some(script) = found.iter().find(|p| is_script(p)) {
        return Some(script.clone());
    }
    let package = dir.join("node_modules").join("@anthropic-ai").join("claude-code");
    [package.join("bin").join("claude.exe"), package.join("cli.js")]
        .into_iter()
        .find(|guess| guess.exists())
}

/// How to start Claude Code: `Command::new(command).args(prefix).args(args)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launcher {
    /// The program to run.
    pub command: PathBuf,
    /// Arguments that come before Claude Code's own.
    pub prefix: Vec<String>,
    /// Where Claude Code is.
    pub path: PathBuf,
}

impl Launcher {
    /// A launcher that runs the program at `path` itself.
    pub fn program(path: impl Into<PathBuf>) -> Launcher {
        let path = path.into();
        Launcher {
            command: path.clone(),
            prefix: Vec::new(),
            path,
        }
    }

    /// A command that runs Claude Code with these arguments.
    pub fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.command);
        command.args(&self.prefix).args(args);
        command
    }
}

/// The launcher for a resolved path: a script runs under Node, anything else runs itself.
fn launch(path: PathBuf, env: &Env) -> Result<Launcher, ClaudeNotFound> {
    if !is_script(&path) {
        return Ok(Launcher::program(path));
    }
    let node = env
        .some("BATON_NODE")
        .map(PathBuf::from)
        .or_else(|| find_on_path("node", env))
        .ok_or_else(|| {
            ClaudeNotFound(format!(
                "Claude Code at {} runs under Node, and node is not on PATH.",
                path.display()
            ))
        })?;
    Ok(Launcher {
        command: node,
        prefix: vec![path.to_string_lossy().into_owned()],
        path,
    })
}

/// A resolved path as a launcher; a shell shim is read for what it runs.
fn launch_resolved(path: PathBuf, env: &Env) -> Result<Launcher, ClaudeNotFound> {
    if is_shim(&path) {
        let target = shim_target(&path).ok_or_else(|| {
            ClaudeNotFound(format!(
                "Cannot run the shell shim {} without a shell.",
                path.display()
            ))
        })?;
        return launch(target, env);
    }
    launch(path, env)
}

/// Finds Claude Code: the path the operator configured, else `BATON_CLAUDE`, else `claude` on PATH,
/// else the native installer's location under `home`.
pub fn resolve_claude(
    configured: Option<&str>,
    env: &Env,
    home: Option<&Path>,
) -> Result<Launcher, ClaudeNotFound> {
    let explicit = configured
        .filter(|c| !c.is_empty())
        .or_else(|| env.some("BATON_CLAUDE"));
    if let Some(explicit) = explicit {
        let path = if Path::new(explicit).is_absolute() {
            Some(PathBuf::from(explicit))
        } else {
            find_on_path(explicit, env)
        };
        let Some(path) = path.filter(|path| path.exists()) else {
            return Err(ClaudeNotFound(format!("Claude Code not found at {explicit}.")));
        };
        return launch_resolved(path, env);
    }
    let mut path = find_on_path("claude", env);
    if path.is_none()
        && let Some(home) = home
    {
        // The native installer's location, when a terminal did not get it on PATH.
        let native =
            home.join(".local")
                .join("bin")
                .join(if cfg!(windows) { "claude.exe" } else { "claude" });
        if is_file(&native) {
            path = Some(native);
        }
    }
    let Some(path) = path else {
        return Err(ClaudeNotFound("Claude Code (claude) is not on PATH.".to_owned()));
    };
    launch_resolved(path, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Makes a file a program: with content, and (unix) the right to run it.
    fn program(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn exe(name: &str) -> String {
        if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        }
    }

    fn path_of(dirs: &[&Path]) -> String {
        dirs.iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(if cfg!(windows) { ";" } else { ":" })
    }

    #[test]
    fn a_program_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        program(&b.join(exe("claude")));
        fs::create_dir_all(&a).unwrap();
        let env = Env::from_pairs([("PATH", path_of(&[&a, &b]))]);
        assert_eq!(find_on_path("claude", &env), Some(b.join(exe("claude"))));
        assert_eq!(find_on_path("nothing", &env), None);
        assert_eq!(find_on_path("claude", &Env::new()), None);
        let launcher = resolve_claude(None, &env, None).unwrap();
        assert_eq!(launcher, Launcher::program(b.join(exe("claude"))));
    }

    #[test]
    fn a_configured_path_wins() {
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("own").join(exe("claude"));
        program(&own);
        let env = Env::from_pairs([("PATH", "")]);
        let own_text = own.to_string_lossy().into_owned();
        assert_eq!(resolve_claude(Some(&own_text), &env, None).unwrap().path, own);
        let by_env = Env::from_pairs([("PATH", String::new()), ("BATON_CLAUDE", own_text)]);
        assert_eq!(resolve_claude(None, &by_env, None).unwrap().path, own);
        let missing = dir.path().join("missing").to_string_lossy().into_owned();
        assert_eq!(
            resolve_claude(Some(&missing), &env, None).unwrap_err().0,
            format!("Claude Code not found at {missing}.")
        );
        assert_eq!(
            resolve_claude(None, &env, None).unwrap_err().0,
            "Claude Code (claude) is not on PATH."
        );
    }

    #[test]
    fn the_native_install_is_found_without_path() {
        let dir = tempfile::tempdir().unwrap();
        let native = dir.path().join(".local").join("bin").join(exe("claude"));
        program(&native);
        let env = Env::from_pairs([("PATH", "")]);
        assert_eq!(resolve_claude(None, &env, Some(dir.path())).unwrap().path, native);
    }

    #[test]
    fn a_script_runs_under_node() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cli.js");
        let node = dir.path().join("bin").join(exe("node"));
        program(&script);
        program(&node);
        let script_text = script.to_string_lossy().into_owned();
        let env = Env::from_pairs([("PATH", path_of(&[node.parent().unwrap()]))]);
        let launcher = resolve_claude(Some(&script_text), &env, None).unwrap();
        assert_eq!(
            launcher,
            Launcher {
                command: node,
                prefix: vec![script_text.clone()],
                path: script
            }
        );
        let no_node = Env::from_pairs([("PATH", "")]);
        assert!(
            resolve_claude(Some(&script_text), &no_node, None)
                .unwrap_err()
                .0
                .contains("node is not on PATH")
        );
    }

    #[test]
    fn what_a_shim_runs() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("claude.cmd");
        let cli = dir
            .path()
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("cli.js");
        program(&cli);
        fs::write(
            &shim,
            "@ECHO off\r\n\"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n",
        )
        .unwrap();
        assert_eq!(shim_target(&shim), Some(crate::paths::resolve(&cli)));
        // An exe named in the shim wins over a script.
        let bin = dir
            .path()
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("bin")
            .join("claude.exe");
        program(&bin);
        fs::write(&shim, "\"%~dp0\\node_modules\\@anthropic-ai\\claude-code\\cli.js\"\r\n\"%~dp0\\node_modules\\@anthropic-ai\\claude-code\\bin\\claude.exe\"\r\n").unwrap();
        assert_eq!(shim_target(&shim), Some(crate::paths::resolve(&bin)));
        // Nothing named: the package's usual places.
        fs::write(&shim, "@ECHO off\r\n").unwrap();
        assert_eq!(shim_target(&shim), Some(bin));
        assert_eq!(shim_target(&dir.path().join("none.cmd")), None);
    }
}
