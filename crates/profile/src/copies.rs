//! Two-way copies of the shared files when the OS does not allow file links (Windows without Developer
//! Mode): settings.json, CLAUDE.md and keybindings.json are copies in every profile.
//!
//! Baton remembers the hash of what it last copied into each profile (copies.json), so at every launch
//! it can tell who changed what since:
//!  - only ~/.claude changed: the profiles are refreshed from it;
//!  - only a profile changed (e.g. /config under account 2 wrote its settings.json): its version is
//!    copied back to ~/.claude (after a timestamped backup of ~/.claude's) and from there to the other
//!    profiles;
//!  - both changed: ~/.claude's version is kept everywhere, the profile's is saved as a backup, and the
//!    conflict is recorded (conflicts.json; doctor, status and the menu show it).
//!
//! When two profiles changed the same file, the first in priority order is copied back and the others
//! are conflicts.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use baton_core::clock::{Ms, iso};
use baton_core::fsutil::{Modes, read_json, write_bytes_atomic, write_json_atomic};
use baton_core::json::{Object, is_truthy};
use baton_core::oslock::OsLock;
use baton_core::paths::{Paths, is_account_id, profile_dir};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// What a conflict names when the shared base's version was kept.
pub const BASE: &str = "~/.claude";

/// The shared files that are copies where file links are not allowed.
pub const COPIED: [&str; 3] = ["settings.json", "CLAUDE.md", "keybindings.json"];

/// How many conflicts are kept.
const KEPT_CONFLICTS: usize = 50;

/// How long a copy sync waits for another baton to finish its own.
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// What Baton last copied into each profile.
pub fn copies_file(paths: &Paths) -> PathBuf {
    paths.root.join("copies.json")
}

/// The conflicts recorded so far.
pub fn conflicts_file(paths: &Paths) -> PathBuf {
    paths.root.join("conflicts.json")
}

/// The SHA-256 of some bytes, as lower-case hex.
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A file's bytes, or None when it cannot be read.
fn read(file: &Path) -> Option<Vec<u8>> {
    fs::read(file).ok()
}

/// Whether something is at `file` that is not a link.
fn is_copy(file: &Path) -> bool {
    fs::symlink_metadata(file).is_ok_and(|meta| !meta.file_type().is_symlink())
}

/// A shared file that changed in two places at once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// When it was found.
    #[serde(default)]
    pub at: Option<String>,
    /// The id of the account whose version was set aside.
    #[serde(default)]
    pub account: String,
    /// The file: `settings.json`, `CLAUDE.md` or `keybindings.json`.
    #[serde(default)]
    pub item: String,
    /// Where the version that was set aside is kept.
    #[serde(default)]
    pub backup: Option<String>,
    /// Whose version was kept: [`BASE`], or the id of another account.
    #[serde(default)]
    pub kept: String,
}

/// The conflicts recorded so far, oldest first.
pub fn read_conflicts(paths: &Paths) -> Vec<Conflict> {
    let record = read_json(&conflicts_file(paths)).unwrap_or(Value::Null);
    let Some(Value::Array(rows)) = record.get("conflicts") else {
        return Vec::new();
    };
    rows.iter()
        .filter(|row| row.is_object())
        .filter_map(|row| serde_json::from_value(row.clone()).ok())
        .collect()
}

/// Forgets every recorded conflict.
pub fn clear_conflicts(paths: &Paths) -> io::Result<()> {
    write_json_atomic(&conflicts_file(paths), &json!({ "conflicts": [] }))
}

/// Records more conflicts; the last [`KEPT_CONFLICTS`] are kept.
fn add_conflicts(paths: &Paths, list: Vec<Conflict>) -> io::Result<()> {
    if list.is_empty() {
        return Ok(());
    }
    let mut all = read_conflicts(paths);
    all.extend(list);
    let from = all.len().saturating_sub(KEPT_CONFLICTS);
    write_json_atomic(&conflicts_file(paths), &json!({ "conflicts": all[from..] }))
}

/// copies.json when it holds an object.
fn read_record(paths: &Paths) -> Option<Object> {
    match read_json(&copies_file(paths)) {
        Some(Value::Object(record)) => Some(record),
        _ => None,
    }
}

/// A copy of the object at `key` of `record`; empty when it is missing or is not one.
fn object_of(record: &Object, key: &str) -> Object {
    record
        .get(key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// The object at `key` of `parent`, made when it is missing or is not one.
fn object_at<'a>(parent: &'a mut Object, key: &str) -> Option<&'a mut Object> {
    let slot = parent.entry(key.to_owned()).or_insert(Value::Null);
    if !slot.is_object() {
        *slot = Value::Object(Object::new());
    }
    slot.as_object_mut()
}

/// Forgets a profile (baton account remove).
pub fn forget_copies(paths: &Paths, name: &str) -> io::Result<()> {
    let Some(mut record) = read_record(paths) else {
        return Ok(());
    };
    let Some(Value::Object(profiles)) = record.get_mut("profiles") else {
        return Ok(());
    };
    if !profiles.get(name).is_some_and(is_truthy) {
        return Ok(());
    }
    profiles.shift_remove(name);
    write_json_atomic(&copies_file(paths), &record)
}

/// What [`sync_copies`] did with one file of one profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CopyEntry {
    /// The profile's account id.
    pub account: String,
    /// The file.
    pub name: String,
    /// `copied`, `refreshed`, `ok`, `copied-back`, `conflict` or `no-source`.
    pub action: &'static str,
    /// More about it, when there is more to say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One entry of a report.
fn entry(account: &str, name: &str, action: &'static str, detail: Option<String>) -> CopyEntry {
    CopyEntry {
        account: account.to_owned(),
        name: name.to_owned(),
        action,
        detail,
    }
}

/// A profile's copy that changed since Baton last wrote it.
struct Changed {
    /// The profile's account id.
    profile: String,
    /// The copy's bytes.
    buffer: Vec<u8>,
    /// Their hash.
    hash: String,
}

/// Brings the profiles' copies in line. `names` are the profiles to bring in line (in priority order);
/// profiles Baton copied into before are included too. Returns one entry per file and profile.
pub fn sync_copies<S: AsRef<str>>(paths: &Paths, names: &[S], now: Ms) -> io::Result<Vec<CopyEntry>> {
    // One baton at a time: two that read the record together would each copy over the other's work.
    // Another one still at it after the wait brings the copies in line itself.
    let Some(_lock) = OsLock::take(&paths.run.join("copies.lock"), LOCK_WAIT)? else {
        return Ok(Vec::new());
    };
    let mut record = read_record(paths).unwrap_or_default();
    record.insert("version".to_owned(), json!(1));
    let mut copied = object_of(&record, "profiles");
    let mut source = object_of(&record, "source");
    let mut profiles: Vec<String> = Vec::new();
    for name in names
        .iter()
        .map(|name| name.as_ref())
        .chain(copied.keys().map(String::as_str))
    {
        if !profiles.iter().any(|p| p == name) && is_account_id(name) && profile_dir(paths, name).exists() {
            profiles.push(name.to_owned());
        }
    }
    let backup_dir = paths.backups.join(iso(now).replace([':', '.'], "-"));
    let backup = |label: String, buffer: &[u8]| -> io::Result<PathBuf> {
        let file = backup_dir.join(label);
        write_bytes_atomic(&file, buffer, Modes::default())?;
        Ok(file)
    };
    let mut report = Vec::new();
    let mut conflicts = Vec::new();
    for item in COPIED {
        let src = paths.claude_dir.join(item);
        let Some(mut src_buffer) = read(&src) else {
            report.extend(profiles.iter().map(|p| entry(p, item, "no-source", None)));
            continue;
        };
        let mut src_hash = hash(&src_buffer);
        let source_changed = source
            .get(item)
            .is_some_and(|known| !known.is_null() && known.as_str() != Some(src_hash.as_str()));
        // Profiles whose copy changed since Baton last wrote it.
        let mut changed = Vec::new();
        for p in &profiles {
            let dst = profile_dir(paths, p).join(item);
            let Some(last) = copied
                .get(p)
                .and_then(|own| own.get(item))
                .filter(|last| is_truthy(last))
            else {
                continue;
            };
            if !is_copy(&dst) {
                continue;
            }
            let Some(buffer) = read(&dst) else {
                continue;
            };
            let h = hash(&buffer);
            if last.as_str() != Some(h.as_str()) && h != src_hash {
                changed.push(Changed {
                    profile: p.clone(),
                    buffer,
                    hash: h,
                });
            }
        }
        let mut handled: Vec<String> = Vec::new();
        let mut changed = changed.into_iter();
        match changed.next() {
            Some(winner) if !source_changed => {
                let saved = backup(format!("copyback-{item}"), &src_buffer)?;
                // Through a symbolic link in ~/.claude (dotfiles) to its target.
                let target = if is_copy(&src) {
                    src.clone()
                } else {
                    fs::canonicalize(&src)?
                };
                write_bytes_atomic(&target, &winner.buffer, Modes::default())?;
                report.push(entry(
                    &winner.profile,
                    item,
                    "copied-back",
                    Some(format!("~/.claude's previous {item} is in {}", saved.display())),
                ));
                for other in changed {
                    if other.hash == winner.hash {
                        continue;
                    }
                    let saved = backup(format!("conflict-{}-{item}", other.profile), &other.buffer)?;
                    conflicts.push(Conflict {
                        at: Some(iso(now)),
                        account: other.profile.clone(),
                        item: item.to_owned(),
                        backup: Some(saved.to_string_lossy().into_owned()),
                        kept: winner.profile.clone(),
                    });
                    report.push(entry(
                        &other.profile,
                        item,
                        "conflict",
                        Some(format!(
                            "kept {}'s change; this profile's is in {}",
                            winner.profile,
                            saved.display()
                        )),
                    ));
                    handled.push(other.profile);
                }
                src_buffer = winner.buffer;
                src_hash = winner.hash;
                handled.push(winner.profile);
            }
            first => {
                for c in first.into_iter().chain(changed) {
                    let saved = backup(format!("conflict-{}-{item}", c.profile), &c.buffer)?;
                    conflicts.push(Conflict {
                        at: Some(iso(now)),
                        account: c.profile.clone(),
                        item: item.to_owned(),
                        backup: Some(saved.to_string_lossy().into_owned()),
                        kept: BASE.to_owned(),
                    });
                    report.push(entry(
                        &c.profile,
                        item,
                        "conflict",
                        Some(format!(
                            "changed in the profile and in ~/.claude: kept ~/.claude's; this profile's is in {}",
                            saved.display()
                        )),
                    ));
                    handled.push(c.profile);
                }
            }
        }
        // Every profile's copy as ~/.claude's file is now.
        for p in &profiles {
            let dst = profile_dir(paths, p).join(item);
            let Some(own) = object_at(&mut copied, p) else {
                continue;
            };
            if dst.exists() && !is_copy(&dst) {
                // A real link (made where file links are allowed): nothing to copy.
                own.shift_remove(item);
                continue;
            }
            let quiet = handled.contains(p);
            let buffer = read(&dst);
            if buffer.as_deref().is_some_and(|buffer| hash(buffer) == src_hash) {
                own.insert(item.to_owned(), json!(src_hash));
                if !quiet {
                    report.push(entry(p, item, "ok", Some("copy".to_owned())));
                }
                continue;
            }
            // A copy with no record that differs (made before Baton kept hashes): ~/.claude's wins, the
            // old copy is kept as a backup.
            if let Some(buffer) = buffer.as_deref()
                && !own.get(item).is_some_and(is_truthy)
                && !quiet
            {
                backup(format!("replaced-{p}-{item}"), buffer)?;
            }
            write_bytes_atomic(&dst, &src_buffer, Modes::default())?;
            own.insert(item.to_owned(), json!(src_hash));
            if !quiet {
                report.push(entry(
                    p,
                    item,
                    if buffer.is_some() { "refreshed" } else { "copied" },
                    None,
                ));
            }
        }
        source.insert(item.to_owned(), json!(src_hash));
    }
    record.insert("profiles".to_owned(), Value::Object(copied));
    record.insert("source".to_owned(), Value::Object(source));
    write_json_atomic(&copies_file(paths), &record)?;
    add_conflicts(paths, conflicts)?;
    Ok(report)
}

/// Whether a profile's copy was changed in the profile since Baton's last copy (it will be copied back
/// at the next launch): `no-source`, `missing`, `copy-fresh`, `copy-changed` or `copy-stale`.
pub fn copy_state(paths: &Paths, name: &str, item: &str) -> &'static str {
    let record = read_json(&copies_file(paths)).unwrap_or(Value::Null);
    let last = record
        .get("profiles")
        .and_then(|profiles| profiles.get(name))
        .and_then(|own| own.get(item));
    let Some(source) = read(&paths.claude_dir.join(item)) else {
        return "no-source";
    };
    let Some(copy) = read(&profile_dir(paths, name).join(item)) else {
        return "missing";
    };
    let copy_hash = hash(&copy);
    if copy_hash == hash(&source) {
        return "copy-fresh";
    }
    match last.filter(|last| is_truthy(last)) {
        Some(last) if last.as_str() != Some(copy_hash.as_str()) => "copy-changed",
        _ => "copy-stale",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_sha256_in_hex() {
        assert_eq!(
            hash(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn conflicts_are_read_leniently() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_core::paths::baton_paths(dir.path());
        assert!(read_conflicts(&paths).is_empty());
        write_json_atomic(
            &conflicts_file(&paths),
            &json!({ "conflicts": [{ "account": "a", "item": "settings.json", "kept": BASE }, 3, { "account": 4 }] }),
        )
        .unwrap();
        let list = read_conflicts(&paths);
        assert_eq!(list.len(), 1);
        assert_eq!(
            (
                list[0].account.as_str(),
                list[0].kept.as_str(),
                list[0].backup.clone()
            ),
            ("a", BASE, None)
        );
        clear_conflicts(&paths).unwrap();
        assert!(read_conflicts(&paths).is_empty());
        assert_eq!(
            fs::read_to_string(conflicts_file(&paths)).unwrap(),
            "{\n  \"conflicts\": []\n}\n"
        );
    }

    #[test]
    fn only_the_last_conflicts_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_core::paths::baton_paths(dir.path());
        let conflict = |n: usize| Conflict {
            account: format!("a{n}"),
            ..Conflict::default()
        };
        add_conflicts(&paths, Vec::new()).unwrap();
        assert!(!conflicts_file(&paths).exists());
        add_conflicts(&paths, (0..40).map(conflict).collect()).unwrap();
        add_conflicts(&paths, (40..60).map(conflict).collect()).unwrap();
        let list = read_conflicts(&paths);
        assert_eq!(list.len(), KEPT_CONFLICTS);
        assert_eq!(
            (list[0].account.as_str(), list[49].account.as_str()),
            ("a10", "a59")
        );
    }
}
