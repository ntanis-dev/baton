//! What the tests of this crate share: a temporary home that is never the real one, and helpers to
//! write files into it. (The same as `baton-testkit`'s, kept here so these tests build on their own.)

#![allow(dead_code, reason = "each test binary uses a different part of the helpers")]

use std::fs;
use std::path::{Path, PathBuf};

use baton_core::paths::{Paths, baton_paths};
use serde_json::Value;

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

/// A path without the `\\?\` prefix Windows puts on canonical paths.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(plain) if !plain.starts_with("UNC") => PathBuf::from(plain),
        _ => path,
    }
}

/// A temporary home with an empty `~/.claude`.
pub fn temp_home() -> TestHome {
    let root = tempfile::Builder::new().prefix("baton-test-").tempdir().unwrap();
    // The real path: on macOS the temporary folder is behind a link, and Windows may hand out a short name.
    let home = strip_verbatim(fs::canonicalize(root.path()).unwrap()).join("home");
    fs::create_dir_all(home.join(".claude")).unwrap();
    let paths = baton_paths(&home);
    TestHome { root, home, paths }
}

/// Writes `content` to `file`, creating its folder.
pub fn write(file: &Path, content: &str) {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(file, content).unwrap();
}

/// Writes `value` to `file` as indented JSON, creating its folder.
pub fn write_json(file: &Path, value: &Value) {
    write(file, &serde_json::to_string_pretty(value).unwrap());
}
