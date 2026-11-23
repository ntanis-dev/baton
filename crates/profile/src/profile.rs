//! Every profile shares everything but the sign-in with the shared base (~/.claude): directories by
//! junction (Windows) or symbolic link, files by symbolic link when the OS allows it (on Windows:
//! Developer Mode), else by a copy that Baton keeps in step with ~/.claude's and the other profiles'
//! ([`crate::shared`]). A copy becomes a link once links are allowed: it is brought in step first.
//!
//! The per-profile global config (.claude.json) is never linked: it holds the signed-in account's
//! identity, so Baton seeds only the keys that make a profile start like plain claude (onboarding done,
//! the same trusted folders and MCP servers). Credentials are never touched: they are neither in the
//! shared list nor read, and a link to one is removed.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use baton_core::clock::now_ms;
use baton_core::config::{Account, Config, Mark, load_config};
use baton_core::fsutil::random_hex;
use baton_core::json::{Get, Object, compact, is_truthy};
use baton_core::paths::{Paths, global_config_file, profile_dir, resolve, same_path};
use serde::Serialize;
use serde_json::Value;

use crate::claude_json::{Loaded, load, rewrite};
use crate::shared::{COPIED, copy_state, sync_shared};
use crate::sync::{Apply, full_sync, sync_settings};

/// Something of ~/.claude that every profile shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shared {
    /// Its name in ~/.claude and in the profile.
    pub name: &'static str,
    /// Whether it is a folder.
    pub dir: bool,
}

/// A shared folder.
const fn dir(name: &'static str) -> Shared {
    Shared { name, dir: true }
}

/// A shared file.
const fn file(name: &'static str) -> Shared {
    Shared { name, dir: false }
}

/// What every profile shares: what every conversation needs, then what Claude Code 2.1.302 keeps per
/// conversation or per user next to it (file-history: /rewind checkpoints per session; plans: plan-mode
/// files the transcript points at; tasks, todos; rules and output-styles: user memory and styles).
pub const SHARED: [Shared; 15] = [
    dir("projects"),
    file("settings.json"),
    file("CLAUDE.md"),
    dir("skills"),
    dir("agents"),
    dir("commands"),
    dir("plugins"),
    file("keybindings.json"),
    dir("file-history"),
    dir("plans"),
    dir("tasks"),
    dir("todos"),
    dir("rules"),
    dir("output-styles"),
    // The prompt history: one file where links are allowed, else every profile's lines together.
    file("history.jsonl"),
];

/// Never shared, never linked: credentials, per-process and per-install state and secrets (sessions/
/// and daemon/ hold keys, ssh/ holds SSH keys).
pub const NEVER: [&str; 20] = [
    ".credentials.json",
    ".claude.json",
    "backups",
    "cache",
    "daemon",
    "debug",
    "feedback",
    "ide",
    "jobs",
    "mcp-needs-auth-cache.json",
    "paste-cache",
    "image-cache",
    "scheduled-tasks",
    "session-env",
    "sessions",
    "shell-snapshots",
    "ssh",
    "stats-cache.json",
    "statsig",
    "telemetry",
];

/// .claude.json keys copied from ~/.claude.json (allowlist; everything else, the account identity
/// first, stays per profile).
pub const SEED_KEYS: [&str; 23] = [
    "hasCompletedOnboarding",
    "lastOnboardingVersion",
    "lastReleaseNotesSeen",
    "theme",
    "mcpServers",
    "hasSeenTasksHint",
    "hasSeenAutoDefaultNotice",
    "hasSeenAutoModeOutsideReadPrompt",
    "hasOpenedAgentsView",
    "bypassPermissionsModeAccepted",
    "hasAcknowledgedCostThreshold",
    "editorMode",
    "autoCompactEnabled",
    "verbose",
    "preferredNotifChannel",
    "shiftEnterKeyBindingInstalled",
    "optionAsMetaKeyInstalled",
    "autoUpdates",
    "installMethod",
    "diffTool",
    "githubRepoPaths",
    "officialMarketplaceAutoInstallAttempted",
    "officialMarketplaceAutoInstalled",
];

/// The keys of a folder's entry in .claude.json that are seeded and carried.
pub const SEED_PROJECT_KEYS: [&str; 10] = [
    "hasTrustDialogAccepted",
    "hasCompletedProjectOnboarding",
    "projectOnboardingSeenCount",
    "allowedTools",
    "mcpServers",
    "enabledMcpjsonServers",
    "disabledMcpjsonServers",
    "mcpContextUris",
    "hasClaudeMdExternalIncludesApproved",
    "hasClaudeMdExternalIncludesWarningShown",
];

/// What is at `path`, a link itself and not what it points to; None when nothing is.
fn lstat(path: &Path) -> Option<fs::Metadata> {
    fs::symlink_metadata(path).ok()
}

/// Whether `path` is a link (a symbolic link or, on Windows, a junction).
fn is_link(path: &Path) -> bool {
    lstat(path).is_some_and(|meta| meta.file_type().is_symlink())
}

/// Removes a link: a file link, or a link to a folder (a junction on Windows), never what it points to.
pub fn remove_link(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::IsADirectory
            ) =>
        {
            fs::remove_dir(path) // a junction
        }
        Err(error) => Err(error),
    }
}

/// A path without the `\\?\` prefix Windows may put on what a link points to.
fn plain(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path,
    }
}

/// Whether a link's target is the shared item `src`, however either path is written.
fn points_to(target: &Path, src: &Path) -> bool {
    same_path(plain(target.to_path_buf()), plain(src.to_path_buf()))
}

/// Where a link points, made absolute; None when `path` is not a link.
fn link_target(path: &Path) -> Option<PathBuf> {
    fs::read_link(path).ok().map(|target| resolve(plain(target)))
}

/// Links the folder `src` at `dst`: a junction on Windows (which needs no privilege), a symbolic link
/// elsewhere.
pub fn link_dir(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        junction(src, dst)
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(src, dst)
    }
}

/// Makes the junction `dst` to the folder `src`.
///
/// std has no stable way to make a junction and this workspace allows no unsafe code, so the system's
/// own `mklink /J` makes it. The two paths reach it through the environment of that one command, never
/// through its command line, so no character in them means anything to the shell.
#[cfg(windows)]
fn junction(src: &Path, dst: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    /// CREATE_NO_WINDOW: the helper never shows a console of its own.
    const NO_WINDOW: u32 = 0x0800_0000;
    let output = Command::new("cmd")
        .raw_arg(r#"/D /V:OFF /C mklink /J "%BATON_LINK%" "%BATON_TARGET%""#)
        .env("BATON_LINK", dst)
        .env("BATON_TARGET", resolve(src))
        .stdin(Stdio::null())
        .creation_flags(NO_WINDOW)
        .output()?;
    if output.status.success() && is_link(dst) {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.trim();
    Err(io::Error::other(if said.is_empty() {
        format!("could not make the junction {}", dst.display())
    } else {
        format!("could not make the junction {}: {said}", dst.display())
    }))
}

/// Links the file `src` at `dst` with a symbolic link.
pub fn link_file(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(src, dst)
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(src, dst)
    }
}

/// What the symbolic link probe found, once it ran.
static SYMLINK_CACHE: Mutex<Option<bool>> = Mutex::new(None);

/// Whether this process may create file symbolic links (Windows: Developer Mode or the privilege), by
/// trying in a temporary folder. Always true elsewhere. Asked once per process.
pub fn can_symlink_files(tmp_dir: &Path) -> bool {
    if !cfg!(windows) {
        return true;
    }
    let mut cache = SYMLINK_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(known) = *cache {
        return known;
    }
    let id = random_hex(4);
    let target = tmp_dir.join(format!("symlink-test-{id}.txt"));
    let link = tmp_dir.join(format!("symlink-test-{id}.lnk"));
    let allowed = fs::create_dir_all(tmp_dir)
        .and_then(|()| fs::write(&target, ""))
        .and_then(|()| link_file(&target, &link))
        .is_ok();
    let _ = fs::remove_file(&link);
    let _ = fs::remove_file(&target);
    *cache = Some(allowed);
    allowed
}

/// Forgets what the symbolic link probe found, so that the next question tries again.
pub fn reset_symlink_cache() {
    *SYMLINK_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

/// Whether two files hold the same bytes.
fn same_bytes(a: &Path, b: &Path) -> bool {
    match (fs::read(a), fs::read(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// What linking did with one item of a profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LinkEntry {
    /// The item: a shared folder or file, or `.claude.json`.
    pub name: String,
    /// `linked`, `ok`, `copied`, `refreshed`, `relinked`, `seeded`, `synced`, `adopted`, `no-source`,
    /// `conflict`, `skipped` or `removed-unsafe-link`.
    pub action: &'static str,
    /// More about it, when there is more to say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One entry of a link report.
fn done(name: &str, action: &'static str) -> LinkEntry {
    LinkEntry {
        name: name.to_owned(),
        action,
        detail: None,
    }
}

/// One entry of a link report, with more to say.
fn done_with(name: &str, action: &'static str, detail: impl Into<String>) -> LinkEntry {
    LinkEntry {
        name: name.to_owned(),
        action,
        detail: Some(detail.into()),
    }
}

/// How a profile is linked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkOptions {
    /// Whether files are shared by symbolic links; None asks the system ([`can_symlink_files`]).
    pub file_links: Option<bool>,
    /// Whether the accounts' settings are kept in step ([`crate::sync`]); None asks the policy
    /// (`fullSync`, on unless the operator turned it off).
    pub full_sync: Option<bool>,
}

impl LinkOptions {
    /// Linking with file links allowed or not, whatever the system says.
    pub fn file_links(allowed: bool) -> LinkOptions {
        LinkOptions {
            file_links: Some(allowed),
            ..LinkOptions::default()
        }
    }
}

/// Links (or refreshes) one profile. Returns one entry per item.
pub fn link_profile(paths: &Paths, name: &str, options: &LinkOptions) -> io::Result<Vec<LinkEntry>> {
    let profile = profile_dir(paths, name);
    fs::create_dir_all(&profile)?;
    let files = options
        .file_links
        .unwrap_or_else(|| can_symlink_files(&paths.tmp));
    let sync = options
        .full_sync
        .unwrap_or_else(|| full_sync(&load_config(paths)));
    let mut report = Vec::new();
    let mut copies: Vec<(&'static str, bool)> = Vec::new();
    let config = config_with(paths, name);
    // A copy from when links were not allowed becomes a link: what was changed in it goes to
    // ~/.claude's first, so that nothing of it is lost when the copy goes.
    let own_copy =
        |item: &Shared| !item.dir && lstat(&profile.join(item.name)).is_some_and(|meta| meta.is_file());
    if files && SHARED.iter().any(own_copy) {
        sync_shared(paths, &config, Apply::One(name), now_ms(), Some(true))?;
    }
    for item in SHARED {
        let src = paths.claude_dir.join(item.name);
        let dst = profile.join(item.name);
        if sync {
            share_first(&item, &src, &dst)?;
        }
        let src_stat = fs::metadata(&src).ok();
        let dst_stat = lstat(&dst);
        let is_link = dst_stat
            .as_ref()
            .is_some_and(|meta| meta.file_type().is_symlink());
        let target = if is_link { link_target(&dst) } else { None };
        let points_home = target.as_ref().is_some_and(|target| points_to(target, &src));
        let Some(src_stat) = src_stat else {
            if is_link && points_home {
                remove_link(&dst)?;
            }
            report.push(done(item.name, "no-source"));
            continue;
        };
        if item.dir != src_stat.is_dir() {
            report.push(done_with(item.name, "skipped", "type differs in ~/.claude"));
            continue;
        }
        if is_link && points_home {
            report.push(done(item.name, "ok"));
            continue;
        }
        if is_link {
            // A link elsewhere: re-point it only if it is a dangling or old Baton link.
            if let Some(target) = target.as_ref().filter(|target| lstat(target).is_some()) {
                report.push(done_with(
                    item.name,
                    "conflict",
                    format!("links to {}", target.display()),
                ));
                continue;
            }
            remove_link(&dst)?;
        }
        let action = if is_link { "relinked" } else { "linked" };
        if item.dir {
            if dst_stat.is_some() && !is_link {
                // What an account made in a folder of its own becomes everyone's.
                if sync && adopt_folder(&src, &dst)? {
                    link_dir(&src, &dst)?;
                    report.push(done(item.name, "adopted"));
                    continue;
                }
                report.push(done_with(
                    item.name,
                    "conflict",
                    "a folder of its own exists in the profile",
                ));
                continue;
            }
            link_dir(&src, &dst)?;
            report.push(done(item.name, action));
            continue;
        }
        if files {
            if dst_stat.is_some() && !is_link {
                if !same_bytes(&src, &dst) && copy_state(paths, name, item.name) != "copy-fresh" {
                    report.push(done_with(
                        item.name,
                        "conflict",
                        "the profile's own differs from ~/.claude's and waits for an answer: baton sync",
                    ));
                    continue;
                }
                fs::remove_file(&dst)?;
            }
            // A system that says it allows file links and then refuses this one: a copy instead.
            if link_file(&src, &dst).is_ok() {
                report.push(done(item.name, action));
                continue;
            }
        }
        // A copy: kept in step below, with ~/.claude's and the other profiles'.
        copies.push((item.name, dst_stat.is_some()));
    }
    // What ~/.claude does not have but a profile holds a copy of is shared too.
    for item in SHARED.iter().filter(|item| !item.dir && !files) {
        if !copies.iter().any(|(name, _)| *name == item.name) && COPIED.contains(&item.name) {
            copies.push((item.name, lstat(&profile.join(item.name)).is_some()));
            report.retain(|entry| !(entry.name == item.name && entry.action == "no-source"));
        }
    }
    if !copies.is_empty() {
        let shared = sync_shared(paths, &config, Apply::One(name), now_ms(), Some(false))?;
        for (item, existed) in copies {
            report.push(copy_entry(
                paths,
                name,
                item,
                existed,
                shared.busy,
                shared.of(item),
            ));
        }
    }
    // Defensive: nothing of the NEVER list may be a link into ~/.claude.
    for item in NEVER {
        let dst = profile.join(item);
        if is_link(&dst) {
            remove_link(&dst)?;
            report.push(done(item, "removed-unsafe-link"));
        }
    }
    if sync {
        report.push(sync_entry(paths, name)?);
    } else {
        report.extend(seed_global_config_file(paths, name)?);
    }
    Ok(report)
}

/// With full sync on, before an item is linked: a shared folder that ~/.claude does not have yet is
/// made there, so that it is shared from the start and not born inside one profile; and a shared file
/// that only this profile has becomes ~/.claude's.
fn share_first(item: &Shared, src: &Path, dst: &Path) -> io::Result<()> {
    if lstat(src).is_some() {
        return Ok(());
    }
    if item.dir {
        return fs::create_dir_all(src);
    }
    let own = lstat(dst).is_some_and(|meta| meta.is_file());
    if own {
        if let Some(parent) = src.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(dst, src)?;
    }
    Ok(())
}

/// Moves what a profile keeps in a folder of its own into the shared one, as far as the names are
/// free there. Returns whether the profile's folder is empty now and gone, ready to be a link; what
/// both have under one name stays where it is, for the operator to look at.
fn adopt_folder(src: &Path, dst: &Path) -> io::Result<bool> {
    let names: Vec<_> = fs::read_dir(dst)?
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    for name in names {
        let shared = src.join(&name);
        if lstat(&shared).is_some() || fs::rename(dst.join(&name), &shared).is_err() {
            return Ok(false);
        }
    }
    fs::remove_dir(dst)?;
    Ok(true)
}

/// The config with the account `name` in it: an account that is being added is linked before it is
/// saved.
fn config_with(paths: &Paths, name: &str) -> Config {
    let mut config = load_config(paths);
    if !config.accounts.iter().any(|account| account.id == name) {
        config.accounts.push(Account {
            id: name.to_owned(),
            name: name.to_owned(),
            mark: Mark::Normal,
            five_hour: None,
            weekly: None,
        });
    }
    config
}

/// What keeping a profile's copy of a shared file in step came to. `existed` says whether the profile
/// had the file before; `synced` is the file's part of the sync (None: nobody holds a copy of it).
fn copy_entry(
    paths: &Paths,
    name: &str,
    item: &str,
    existed: bool,
    busy: bool,
    synced: Option<&crate::sync::SyncReport>,
) -> LinkEntry {
    if busy {
        return done_with(item, "skipped", "another baton is syncing the shared files");
    }
    let there = lstat(&profile_dir(paths, name).join(item)).is_some();
    let Some(synced) = synced else {
        return done(item, if there { "ok" } else { "no-source" });
    };
    if synced.unreadable.iter().any(|id| id == name) {
        return done_with(item, "skipped", "the profile's own cannot be read; left as it is");
    }
    if !synced.questions.is_empty() {
        return done_with(
            item,
            "conflict",
            "the accounts changed it in different ways: baton sync asks which stays",
        );
    }
    match synced.written.iter().find(|written| written.account == name) {
        Some(_) if !existed => done(item, "copied"),
        Some(written) if item.ends_with(".json") => {
            done_with(item, "synced", format!("{} settings", written.settings))
        }
        Some(_) => done(item, "synced"),
        None if there => done_with(item, "ok", "copy"),
        None => done(item, "no-source"),
    }
}

/// Brings the profile's `.claude.json` in step with the other accounts' ([`crate::sync`]) and
/// says what came of it.
fn sync_entry(paths: &Paths, name: &str) -> io::Result<LinkEntry> {
    const NAME: &str = ".claude.json";
    let config = config_with(paths, name);
    let synced = sync_settings(paths, &config, Apply::One(name), now_ms())?;
    if synced.busy {
        return Ok(done_with(
            NAME,
            "skipped",
            "another baton is syncing the settings",
        ));
    }
    if synced.unreadable.iter().any(|id| id == name) {
        return Ok(done_with(
            NAME,
            "skipped",
            "the profile's own is not valid JSON; left as it is",
        ));
    }
    Ok(
        match synced.written.iter().find(|written| written.account == name) {
            Some(written) => done_with(NAME, "synced", format!("{} settings", written.settings)),
            None => done_with(NAME, "ok", "in step"),
        },
    )
}

/// The status of one item of a profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Inspected {
    /// The item.
    pub name: String,
    /// `linked`, `wrong-link`, `missing`, `no-source`, `conflict`, `unsafe-link`, or a copy's state:
    /// `copy-fresh`, `copy-changed`.
    pub status: &'static str,
    /// Where a link points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The status of one profile's links, without changing anything. `file_links` says whether file links
/// are allowed (nothing here depends on it any more: a file is shared by a link or by a copy).
pub fn inspect_profile(paths: &Paths, name: &str, file_links: Option<bool>) -> Vec<Inspected> {
    let profile = profile_dir(paths, name);
    let mut out = Vec::new();
    let _ = file_links;
    let mut status = |name: &str, status: &'static str, detail: Option<String>| {
        out.push(Inspected {
            name: name.to_owned(),
            status,
            detail,
        });
    };
    for item in SHARED {
        let src = paths.claude_dir.join(item.name);
        let dst = profile.join(item.name);
        if fs::metadata(&src).is_err() {
            status(item.name, "no-source", None);
            continue;
        }
        let Some(dst_stat) = lstat(&dst) else {
            status(item.name, "missing", None);
            continue;
        };
        if dst_stat.file_type().is_symlink() {
            let target = link_target(&dst);
            let home = target.as_ref().is_some_and(|target| points_to(target, &src));
            status(
                item.name,
                if home { "linked" } else { "wrong-link" },
                target.map(|target| target.to_string_lossy().into_owned()),
            );
            continue;
        }
        if item.dir {
            status(item.name, "conflict", None);
        } else {
            status(item.name, copy_state(paths, name, item.name), None);
        }
    }
    for item in NEVER {
        if is_link(&profile.join(item)) {
            status(item, "unsafe-link", None);
        }
    }
    out
}

/// Whether two folder names that differ only in case are one folder here: so it is on the file
/// systems Windows and macOS come with, and not on Linux's.
const CASE_FOLDS: bool = cfg!(any(windows, target_os = "macos"));

/// A folder as .claude.json keys it, for comparing: forward slashes, no trailing one, and in lower
/// case where case does not tell two folders apart.
pub(crate) fn normalize_project_key(key: &str) -> String {
    let plain = key.replace('\\', "/");
    let plain = plain.trim_end_matches('/');
    if CASE_FOLDS {
        plain.to_lowercase()
    } else {
        plain.to_owned()
    }
}

/// One seeded project key, the profile's value and the incoming one merged: what either side granted
/// stays (true over false, the higher count, both lists, both server maps with the incoming entry for a
/// name both have).
fn merge_project_value(own: Option<&Value>, value: &Value) -> Value {
    match (own, value) {
        (None, _) => value.clone(),
        (Some(Value::Bool(own)), Value::Bool(value)) => Value::Bool(*own || *value),
        (Some(own @ Value::Number(a)), Value::Number(b)) => {
            if a.as_f64() >= b.as_f64() {
                own.clone()
            } else {
                value.clone()
            }
        }
        (Some(Value::Array(own)), Value::Array(value)) => {
            let mut seen: Vec<String> = Vec::new();
            let mut merged = Vec::new();
            for entry in value.iter().chain(own) {
                let key = compact(entry);
                if !seen.contains(&key) {
                    seen.push(key);
                    merged.push(entry.clone());
                }
            }
            Value::Array(merged)
        }
        (Some(Value::Object(own)), Value::Object(value)) => {
            let mut merged = own.clone();
            for (key, entry) in value {
                merged.insert(key.clone(), entry.clone());
            }
            Value::Object(merged)
        }
        _ => value.clone(),
    }
}

/// A folder's seeded keys merged into the profile's entry for it, never put in place of it: Claude
/// Code saves every folder with its defaults (trust false, empty lists), so a plain copy would take
/// back the trust the profile has (its own, or the one a hop just carried to it) at every launch.
fn with_project_keys(own: Option<&Value>, picked: &Object) -> Value {
    let mut out = own.and_then(Value::as_object).cloned().unwrap_or_default();
    for (key, value) in picked {
        let merged = merge_project_value(out.get(key), value);
        out.insert(key.clone(), merged);
    }
    Value::Object(out)
}

/// The seeded keys a folder's entry holds.
fn picked_project_keys(entry: &Object) -> Object {
    SEED_PROJECT_KEYS
        .iter()
        .filter_map(|key| Some(((*key).to_owned(), entry.get(*key)?.clone())))
        .collect()
}

/// The seed: allowlisted keys from ~/.claude.json merged into the profile's own (pure). Account-identity
/// keys are never taken from the source; the profile's own values for them are kept.
pub fn seed_global_config(source: Option<&Value>, target: Option<&Value>) -> Value {
    let mut out = target.and_then(Value::as_object).cloned().unwrap_or_default();
    let Some(source) = source.and_then(Value::as_object) else {
        return Value::Object(out);
    };
    for key in SEED_KEYS {
        if let Some(value) = source.get(key) {
            out.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(Value::Object(folders)) = source.get("projects") {
        let mut projects = out
            .get("projects")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (path, entry) in folders {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let picked = picked_project_keys(entry);
            if picked.is_empty() {
                continue;
            }
            let merged = with_project_keys(projects.get(path), &picked);
            projects.insert(path.clone(), merged);
        }
        out.insert("projects".to_owned(), Value::Object(projects));
    }
    Value::Object(out)
}

/// Seeds a profile's .claude.json from ~/.claude.json. A profile's file that is there but cannot be
/// read is left as it is: it holds the account's identity.
pub fn seed_global_config_file(paths: &Paths, name: &str) -> io::Result<Vec<LinkEntry>> {
    const NAME: &str = ".claude.json";
    let target_file = global_config_file(paths, name);
    let Some(source) = load(&paths.claude_json).json().filter(is_truthy) else {
        return Ok(vec![done(NAME, "no-source")]);
    };
    let (mut unreadable, mut existed) = (false, false);
    let wrote = rewrite(&target_file, |loaded| {
        unreadable = *loaded == Loaded::Unreadable;
        if unreadable {
            return None;
        }
        let before = loaded.clone().json().filter(is_truthy);
        existed = before.is_some();
        let after = seed_global_config(Some(&source), before.as_ref());
        let same = before
            .as_ref()
            .is_some_and(|before| compact(before) == compact(&after));
        (!same).then_some(after)
    })?;
    Ok(vec![if unreadable {
        done_with(
            NAME,
            "skipped",
            "the profile's own is not valid JSON; left as it is",
        )
    } else if !wrote {
        done_with(NAME, "ok", "seeded")
    } else {
        done(NAME, if existed { "refreshed" } else { "seeded" })
    }])
}

/// Carries a folder's trust (and its other seeded project keys) from the profile a session ran on to
/// the one it moves to, so the resumed session starts without the trust dialog (which would also keep
/// the hooks off). ~/.claude.json is never written. Returns whether anything was carried.
pub fn carry_trust(paths: &Paths, from: &str, to: &str, cwd: &str) -> io::Result<bool> {
    if cwd.is_empty() || from == to {
        return Ok(false);
    }
    let source = load(&global_config_file(paths, from))
        .json()
        .unwrap_or(Value::Null);
    let want = normalize_project_key(cwd);
    rewrite(&global_config_file(paths, to), |loaded| {
        let target = match loaded {
            // The account's identity is in there: never written over.
            Loaded::Unreadable => return None,
            loaded => loaded
                .clone()
                .json()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default(),
        };
        let mut changed = false;
        let mut projects = target
            .get("projects")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for (key, entry) in source.at("projects").as_object().into_iter().flatten() {
            if normalize_project_key(key) != want || !entry.truthy("hasTrustDialogAccepted") {
                continue;
            }
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let merged = with_project_keys(projects.get(key), &picked_project_keys(entry));
            if projects
                .get(key)
                .is_none_or(|own| compact(own) != compact(&merged))
            {
                projects.insert(key.clone(), merged);
                changed = true;
            }
        }
        changed.then(|| {
            let mut out = target;
            out.insert("projects".to_owned(), Value::Object(projects));
            Value::Object(out)
        })
    })
}

/// Removes Baton's links from a profile (the folder itself, with its own sign-in, stays). Returns the
/// names of what was unlinked.
pub fn unlink_profile(paths: &Paths, name: &str) -> io::Result<Vec<String>> {
    let profile = profile_dir(paths, name);
    let mut removed = Vec::new();
    for item in SHARED {
        let dst = profile.join(item.name);
        if !is_link(&dst) {
            continue;
        }
        if link_target(&dst).is_some_and(|target| points_to(&target, &paths.claude_dir.join(item.name))) {
            remove_link(&dst)?;
            removed.push(item.name.to_owned());
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_keys_compare_whatever_the_slashes_and_the_case_where_case_is_one() {
        assert_eq!(normalize_project_key("e:/work///"), "e:/work");
        assert_eq!(normalize_project_key("e:\\work\\"), "e:/work");
        // Where `Work` and `work` are one folder they are one key; on Linux they are two folders.
        let same = normalize_project_key("/home/Op/x") == normalize_project_key("/home/op/x");
        assert_eq!(same, CASE_FOLDS);
        assert_eq!(
            normalize_project_key("E:\\Work\\"),
            if CASE_FOLDS { "e:/work" } else { "E:/Work" }
        );
    }

    #[test]
    fn what_either_side_granted_stays() {
        let merge = |own: Value, value: Value| merge_project_value(Some(&own), &value);
        assert_eq!(merge_project_value(None, &json!(false)), json!(false));
        assert_eq!(merge(json!(true), json!(false)), json!(true));
        assert_eq!(merge(json!(false), json!(true)), json!(true));
        assert_eq!(merge(json!(3), json!(0)), json!(3));
        assert_eq!(merge(json!(1), json!(2.5)), json!(2.5));
        assert_eq!(
            merge(json!(["Read", "Bash"]), json!(["Bash"])),
            json!(["Bash", "Read"])
        );
        assert_eq!(
            merge(json!([{ "a": 1 }]), json!([{ "a": 1 }, { "a": 2 }])),
            json!([{ "a": 1 }, { "a": 2 }])
        );
        assert_eq!(
            merge(json!({ "own": 1, "both": 1 }), json!({ "both": 2, "new": 3 })),
            json!({ "own": 1, "both": 2, "new": 3 })
        );
        // Different kinds: the incoming value.
        assert_eq!(merge(json!("text"), json!(true)), json!(true));
        assert_eq!(merge(json!(null), json!([1])), json!([1]));
    }

    #[test]
    fn links_point_where_they_were_made_to() {
        let dir = tempfile::tempdir().unwrap();
        let base = resolve(fs::canonicalize(dir.path()).map(plain).unwrap());
        let (src, dst) = (base.join("source folder"), base.join("link & 100% (x)"));
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("inside.txt"), "x").unwrap();
        assert_eq!(link_target(&dst), None);
        link_dir(&src, &dst).unwrap();
        assert!(is_link(&dst));
        assert!(same_path(link_target(&dst).unwrap(), &src));
        assert_eq!(fs::read_to_string(dst.join("inside.txt")).unwrap(), "x");
        remove_link(&dst).unwrap();
        assert!(lstat(&dst).is_none());
        assert!(src.join("inside.txt").exists(), "what it pointed to stays");
        assert!(remove_link(&dst).is_err());
    }
}
