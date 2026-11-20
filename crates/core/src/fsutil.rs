//! Small file helpers. State files are replaced atomically (a temporary file in the same folder, then a
//! rename), so a reader never sees half a file; history files are append-only JSON lines.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::json;

pub use crate::sys::{is_alive, lower_priority};

/// Waits `ms` milliseconds.
pub fn sleep_ms(ms: u64) {
    thread::sleep(Duration::from_millis(ms));
}

/// `bytes` random bytes as lower-case hex.
pub fn random_hex(bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes * 2 {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out.truncate(bytes * 2);
    out
}

/// A file's JSON, or None when it is missing, unreadable or not JSON.
pub fn read_json(file: &Path) -> Option<Value> {
    read_json_as(file)
}

/// A file's JSON as `T`, or None when it is missing, unreadable or not a `T`.
pub fn read_json_as<T: DeserializeOwned>(file: &Path) -> Option<T> {
    let value = json::parse(&fs::read_to_string(file).ok()?)?;
    serde_json::from_value(value).ok()
}

/// A file's text, or None when it is missing or unreadable.
pub fn read_text(file: &Path) -> Option<String> {
    fs::read_to_string(file).ok()
}

/// The permissions (unix) of what [`write_file_atomic`] creates; folders that are there keep theirs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// The mode of every folder it creates.
    pub dir: Option<u32>,
    /// The mode of the file.
    pub file: Option<u32>,
}

/// Creates `dir` and its missing parents, each with `mode` when one is given (unix).
pub fn create_dirs(dir: &Path, mode: Option<u32>) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::DirBuilderExt;
        return fs::DirBuilder::new().recursive(true).mode(mode).create(dir);
    }
    let _ = mode;
    fs::create_dir_all(dir)
}

/// Whether a failed rename or read is the kind Windows gives while another process holds the file (a
/// virus scanner, an editor, a writer replacing it).
pub fn is_busy(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return true;
    }
    // ERROR_SHARING_VIOLATION and ERROR_LOCK_VIOLATION on Windows, EBUSY elsewhere.
    matches!(error.raw_os_error(), Some(code) if (cfg!(windows) && (code == 32 || code == 33)) || (cfg!(unix) && code == 16))
}

/// Replaces `file` with `text` atomically, creating its folder.
pub fn write_file_atomic(file: &Path, text: &str, modes: Modes) -> io::Result<()> {
    write_bytes_atomic(file, text.as_bytes(), modes)
}

/// Replaces `file` with `bytes` atomically, creating its folder.
pub fn write_bytes_atomic(file: &Path, bytes: &[u8], modes: Modes) -> io::Result<()> {
    let dir = match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    create_dirs(&dir, modes.dir)?;
    let name = file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.{}.tmp", std::process::id(), random_hex(4)));
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if let Some(mode) = modes.file {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let written = options.open(&tmp).and_then(|mut out| out.write_all(bytes));
    if let Err(error) = written {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    // Windows refuses a rename over a file another process has open without delete sharing (a virus
    // scanner, an editor); retry briefly.
    let mut attempt = 0u64;
    loop {
        match fs::rename(&tmp, file) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if attempt >= 6 || !is_busy(&error) {
                    let _ = fs::remove_file(&tmp);
                    return Err(error);
                }
                sleep_ms(5 * (attempt + 1));
                attempt += 1;
            }
        }
    }
}

/// Replaces `file` with `value` as indented JSON and a final newline.
pub fn write_json_atomic<T: Serialize + ?Sized>(file: &Path, value: &T) -> io::Result<()> {
    write_file_atomic(file, &(json::pretty(value) + "\n"), Modes::default())
}

/// Appends `value` to `file` as one line of JSON, creating its folder.
pub fn append_json_line<T: Serialize + ?Sized>(file: &Path, value: &T) -> io::Result<()> {
    append_text(file, &(json::compact(value) + "\n"))
}

/// Appends `text` to `file`, creating its folder.
pub fn append_text(file: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(file)?
        .write_all(text.as_bytes())
}

/// The JSON lines of `file`. A torn last line (a writer that died mid-append) is skipped, and so is a
/// file that is not there.
pub fn read_json_lines(file: &Path) -> Vec<Value> {
    let Some(text) = read_text(file) else {
        return Vec::new();
    };
    text.split('\n')
        .filter(|line| !line.trim().is_empty())
        .filter_map(json::parse)
        .collect()
}

/// Removes a file; one that is not there is fine.
pub fn remove_file(file: &Path) -> io::Result<()> {
    match fs::remove_file(file) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Removes a folder and everything in it; one that is not there is fine.
pub fn remove_tree(dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(dir) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// When `path` was last changed, in epoch milliseconds.
pub fn mtime_ms(path: &Path) -> Option<i64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_files_are_written_whole_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("deep/state.json");
        assert_eq!(read_json(&file), None);
        write_json_atomic(&file, &json!({ "b": 1, "a": [true] })).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "{\n  \"b\": 1,\n  \"a\": [\n    true\n  ]\n}\n"
        );
        assert_eq!(read_json(&file), Some(json!({ "b": 1, "a": [true] })));
        write_json_atomic(&file, &json!(2)).unwrap();
        assert_eq!(read_json(&file), Some(json!(2)));
        let left: Vec<_> = fs::read_dir(file.parent().unwrap()).unwrap().collect();
        assert_eq!(left.len(), 1, "no temporary file is left behind");
        fs::write(&file, "{ not json").unwrap();
        assert_eq!(read_json(&file), None);
    }

    #[test]
    fn json_lines_skip_a_torn_line() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("history/events.jsonl");
        assert!(read_json_lines(&file).is_empty());
        append_json_line(&file, &json!({ "n": 1 })).unwrap();
        append_json_line(&file, &json!({ "n": 2 })).unwrap();
        append_text(&file, "\n{\"n\": 3").unwrap();
        assert_eq!(read_json_lines(&file), vec![json!({ "n": 1 }), json!({ "n": 2 })]);
    }

    #[test]
    fn random_hex_has_the_length_asked_for() {
        assert_eq!(random_hex(3).len(), 6);
        assert_eq!(random_hex(20).len(), 40);
        assert!(random_hex(4).chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(random_hex(8), random_hex(8));
    }

    #[test]
    fn removing_what_is_not_there_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        remove_file(&dir.path().join("nope")).unwrap();
        remove_tree(&dir.path().join("nope")).unwrap();
        assert_eq!(mtime_ms(&dir.path().join("nope")), None);
        assert!(mtime_ms(dir.path()).is_some());
    }
}
