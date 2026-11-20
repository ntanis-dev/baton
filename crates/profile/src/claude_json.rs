//! Reading the JSON files Claude Code writes (`.claude.json`), as forgivingly as Claude Code itself.
//!
//! Claude Code writes them with JavaScript's JSON, which can hold half of a surrogate pair as an escape
//! (`"\ud83d"`: a text cut in the middle of an emoji). A strict reader refuses the whole file for it,
//! and a profile's `.claude.json` read as missing would be seeded over, identity and all. So such an
//! escape is read as the replacement character instead, and a file that is there but still cannot be
//! read is told apart from one that is missing: the first is never written over.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

/// What reading a JSON file came to.
#[derive(Clone, Debug, PartialEq)]
pub enum Loaded {
    /// There is no such file (or it is empty).
    Missing,
    /// The file is there but is not JSON, or could not be read.
    Unreadable,
    /// The file's JSON.
    Json(Value),
}

impl Loaded {
    /// The JSON, when there is some.
    pub fn json(self) -> Option<Value> {
        match self {
            Loaded::Json(value) => Some(value),
            Loaded::Missing | Loaded::Unreadable => None,
        }
    }
}

/// Reads a JSON file Claude Code wrote.
pub fn load(file: &Path) -> Loaded {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Loaded::Missing,
        Err(_) => return Loaded::Unreadable,
    };
    let Ok(text) = String::from_utf8(bytes) else {
        return Loaded::Unreadable;
    };
    if text.trim().is_empty() {
        return Loaded::Missing;
    }
    match parse(&text) {
        Some(value) => Loaded::Json(value),
        None => Loaded::Unreadable,
    }
}

/// Parses JSON text; half a surrogate pair written as an escape reads as the replacement character.
pub fn parse(text: &str) -> Option<Value> {
    baton_core::json::parse(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_is_not_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        assert_eq!(load(&file), Loaded::Missing);
        fs::write(&file, "  \n").unwrap();
        assert_eq!(load(&file), Loaded::Missing);
        fs::write(&file, "{ half").unwrap();
        assert_eq!(load(&file), Loaded::Unreadable);
        assert_eq!(load(&file).json(), None);
        fs::write(&file, r#"{"a":1}"#).unwrap();
        assert_eq!(load(&file), Loaded::Json(json!({ "a": 1 })));
        assert_eq!(load(&file).json(), Some(json!({ "a": 1 })));
    }
}
