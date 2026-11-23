//! Reading the JSON files Claude Code writes (`.claude.json`), as forgivingly as Claude Code itself.
//!
//! Claude Code writes them with JavaScript's JSON, which can hold half of a surrogate pair as an escape
//! (`"\ud83d"`: a text cut in the middle of an emoji). A strict reader refuses the whole file for it,
//! and a profile's `.claude.json` read as missing would be seeded over, identity and all. So such an
//! escape is read as the replacement character instead, and a file that is there but still cannot be
//! read is told apart from one that is missing: the first is never written over.
//!
//! Writing one is done as Claude Code does it ([`rewrite`]), because a Claude Code may be saving the
//! same file at that moment: under Claude Code's own lock on the file, and only when the file is still
//! what the change was worked out from.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use baton_core::fsutil::write_json_atomic;
use serde_json::Value;

/// How long a rewrite waits for a Claude Code that is saving the file.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// A lock not touched for this long was left behind. Claude Code touches the one it holds every five
/// seconds and takes one for left behind after ten; three times that leaves room for a slow moment.
const LOCK_LEFT: Duration = Duration::from_secs(30);

/// How often a rewrite starts over because the file changed under it.
const TRIES: usize = 5;

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
    read(file).1
}

/// A file's bytes (None when it is not there or cannot be read) and what they come to.
fn read(file: &Path) -> (Option<Vec<u8>>, Loaded) {
    let bytes = match fs::read(file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (None, Loaded::Missing),
        Err(_) => return (None, Loaded::Unreadable),
    };
    let loaded = match std::str::from_utf8(&bytes) {
        Err(_) => Loaded::Unreadable,
        Ok(text) if text.trim().is_empty() => Loaded::Missing,
        Ok(text) => parse(text).map_or(Loaded::Unreadable, Loaded::Json),
    };
    (Some(bytes), loaded)
}

/// The lock Claude Code takes on a file while it saves it: a folder `<file>.lock` beside it, made to
/// take the lock and removed to let it go. Baton takes the same one, so the two never save at once.
#[derive(Debug)]
struct SaveLock(PathBuf);

impl SaveLock {
    /// The folder that is the lock of `file`.
    fn folder(file: &Path) -> PathBuf {
        let mut name = file.as_os_str().to_owned();
        name.push(".lock");
        PathBuf::from(name)
    }

    /// Takes the lock, waiting up to `wait` for whoever holds it. None when it is still held then, or
    /// cannot be made (no folder for it yet).
    fn take(file: &Path, wait: Duration) -> Option<SaveLock> {
        let folder = Self::folder(file);
        let started = Instant::now();
        loop {
            match fs::create_dir(&folder) {
                Ok(()) => return Some(SaveLock(folder)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let touched = fs::metadata(&folder).and_then(|meta| meta.modified()).ok();
                    let left = touched
                        .and_then(|at| SystemTime::now().duration_since(at).ok())
                        .is_some_and(|age| age > LOCK_LEFT);
                    if left && fs::remove_dir(&folder).is_ok() {
                        continue;
                    }
                }
                Err(_) => return None,
            }
            if started.elapsed() >= wait {
                return None;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for SaveLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Changes a file Claude Code also writes. `change` is given the file as it is and returns what it
/// becomes, or None to leave it. The file is read under Claude Code's own lock, and it is replaced only
/// while it is still what `change` was given: when a save landed in between (the lock could not be had
/// in time, or was taken over), `change` is asked again with the newer file. Returns whether the file
/// was written.
pub fn rewrite(file: &Path, change: impl FnMut(&Loaded) -> Option<Value>) -> io::Result<bool> {
    rewrite_with(file, true, change)
}

/// [`rewrite`], with Claude Code's lock only when `lock` says so: it is the lock of `.claude.json`.
/// Any other file Claude Code saves is guarded by the check alone.
pub fn rewrite_with(
    file: &Path,
    lock: bool,
    mut change: impl FnMut(&Loaded) -> Option<Value>,
) -> io::Result<bool> {
    let _lock = lock.then(|| SaveLock::take(file, LOCK_WAIT)).flatten();
    for _ in 0..TRIES {
        let (before, loaded) = read(file);
        let Some(next) = change(&loaded) else {
            return Ok(false);
        };
        if fs::read(file).ok() != before {
            continue;
        }
        write_json_atomic(file, &next)?;
        return Ok(true);
    }
    Err(io::Error::other(format!(
        "{} keeps changing; nothing was written",
        file.display()
    )))
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

    #[test]
    fn a_rewrite_is_worked_out_from_the_file_as_it_is_when_it_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        // Nothing to change: nothing written, no lock left.
        assert!(!rewrite(&file, |_| None).unwrap());
        assert!(!file.exists());
        assert!(
            rewrite(&file, |loaded| (*loaded == Loaded::Missing)
                .then(|| json!({ "a": 1 })))
            .unwrap()
        );
        assert_eq!(load(&file), Loaded::Json(json!({ "a": 1 })));
        // A save that lands while the change is worked out: the change is worked out again from it.
        let mut asked = 0;
        let wrote = rewrite(&file, |loaded| {
            asked += 1;
            if asked == 1 {
                fs::write(&file, r#"{"a":1,"theirs":true}"#).unwrap();
            }
            let mut next = loaded.clone().json()?;
            next["ours"] = json!(true);
            Some(next)
        })
        .unwrap();
        assert!(wrote);
        assert_eq!(asked, 2);
        assert_eq!(
            load(&file),
            Loaded::Json(json!({ "a": 1, "theirs": true, "ours": true }))
        );
        assert!(!SaveLock::folder(&file).exists(), "the lock is let go");
        // A file that never stays the same is an error, and is left as the other writer has it.
        let mut n = 0;
        let failed = rewrite(&file, |_| {
            n += 1;
            fs::write(&file, format!(r#"{{"n":{n}}}"#)).unwrap();
            Some(json!({ "ours": true }))
        });
        assert!(failed.is_err());
        assert_eq!(load(&file), Loaded::Json(json!({ "n": TRIES })));
    }

    #[test]
    fn a_rewrite_takes_the_lock_claude_code_takes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        fs::write(&file, "{}").unwrap();
        let folder = SaveLock::folder(&file);
        assert_eq!(folder, dir.path().join(".claude.json.lock"));
        // Held by a Claude Code that is saving: not ours within the wait.
        fs::create_dir(&folder).unwrap();
        assert!(SaveLock::take(&file, Duration::from_millis(60)).is_none());
        assert!(folder.exists(), "a lock in use is never removed");
        fs::remove_dir(&folder).unwrap();
        // While Baton rewrites, the lock is there; afterwards it is gone.
        let mut seen = false;
        rewrite(&file, |_| {
            seen = folder.exists();
            Some(json!({ "a": 2 }))
        })
        .unwrap();
        assert!(seen && !folder.exists());
        // No folder to make it in (a profile that is not there yet): the rewrite goes on without.
        assert!(SaveLock::take(&dir.path().join("none").join(".claude.json"), Duration::ZERO).is_none());
    }
}
