//! Where the stand-in programs are: the fake `claude` executables of this crate (`src/bin`), built on
//! first use into the folder the running test was built into, and the real `baton`.
//!
//! Cargo builds a package's programs for that package's own tests only, so a test in another crate
//! asks here: the first call in a test process runs `cargo build -p baton-testkit --bins` (the cargo
//! that runs the tests, the same target folder and profile, found from the running test's own path);
//! every later call is a path join.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use baton_core::claude_path::Launcher;

/// A stand-in claude for `baton doctor`: its version, and whether it is signed in.
pub const CLAUDE_STUB: &str = "claude-stub";

/// The lead wrapper's `claude`: plays a scenario of hooks, status lines and keys.
pub const FAKE_CLAUDE: &str = "fake-claude";
/// The usage poller's `claude`: `auth status` and the stream-json answer to "/usage".
pub const FAKE_USAGE_CLAUDE: &str = "fake-usage-claude";
/// Both in one, for a whole baton instance.
pub const FAKE_CLAUDE_BOTH: &str = "fake-claude-both";
/// `claude auth status` and `claude auth login`, for the menu's sign-in.
pub const FAKE_LOGIN: &str = "fake-login";
/// A program that reads the console in raw mode and logs what it gets.
pub const RAW_READER: &str = "raw-reader";

/// The folder the running test was built into (`target/debug`): the one above `deps`.
fn profile_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("the running test's own path");
    let mut dir = exe.parent().expect("the running test's folder").to_path_buf();
    if dir
        .file_name()
        .is_some_and(|name| name == "deps" || name == "examples")
    {
        dir.pop();
    }
    dir
}

/// The workspace this crate belongs to.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root")
        .to_path_buf()
}

/// Builds `targets` of `package` into the running test's own profile folder and returns that folder.
fn build(package: &str, targets: &[&str]) -> Result<PathBuf, String> {
    let dir = profile_dir();
    let target_dir = dir.parent().ok_or("the profile folder has no parent")?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .arg("build")
        .args(["-p", package])
        .args(targets)
        .arg("--manifest-path")
        .arg(workspace_root().join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target_dir);
    match dir.file_name().and_then(|name| name.to_str()) {
        Some("debug") | None => {}
        Some("release") => {
            command.arg("--release");
        }
        Some(profile) => {
            command.args(["--profile", profile]);
        }
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run cargo: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo build -p {package} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(dir)
}

/// The path of a program `name` in `dir`.
fn program(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(path.is_file(), "{} was not built", path.display());
    path
}

/// The path of one of this crate's stand-in programs (by name: [`FAKE_CLAUDE`], ...), built on first
/// use.
pub fn bin(name: &str) -> PathBuf {
    static BUILT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    let dir = BUILT.get_or_init(|| build("baton-testkit", &["--bins"]));
    match dir {
        Ok(dir) => program(dir, name),
        Err(error) => panic!("{error}"),
    }
}

/// A launcher that runs one of this crate's stand-in programs as `claude`.
pub fn launcher(name: &str) -> Launcher {
    Launcher::program(bin(name))
}

/// The path of the real `baton` program, built on first use (the hooks and the status line a fake
/// `claude` runs are Baton's own).
pub fn baton() -> PathBuf {
    static BUILT: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    let dir = BUILT.get_or_init(|| build("baton", &["--bin", "baton"]));
    match dir {
        Ok(dir) => program(dir, "baton"),
        Err(error) => panic!("{error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stand_ins_are_built_where_the_test_was() {
        let path = bin(FAKE_USAGE_CLAUDE);
        assert_eq!(path.parent().unwrap(), profile_dir());
        assert_eq!(launcher(FAKE_LOGIN).command, bin(FAKE_LOGIN));
        for name in [FAKE_CLAUDE, FAKE_CLAUDE_BOTH, RAW_READER] {
            assert!(bin(name).is_file());
        }
        let said = Command::new(path)
            .args(["auth", "status", "--json"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&said.stdout).trim(),
            r#"{"loggedIn":true,"authMethod":"claude.ai"}"#
        );
    }
}
