//! `lock-age`: every crates.io package in `Cargo.lock` is at least N days old (14 by default).
//!
//! A brand-new release is where a hijacked crate shows up first, and the ecosystem usually notices within
//! days, so the repository only takes releases that have been out for two weeks. This is the Rust analogue
//! of pnpm's `minimumReleaseAge` in the sibling repos, and like it there are no exemptions: a package that
//! is too young is pinned back with `cargo update -p NAME --precise VERSION`, or the change waits.
//!
//! Publish dates come from the crates.io API through `curl`, at most one request per 100 ms (crates.io's
//! crawler policy), and are cached in `target/xtask-lock-age.json`, since a published date never changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::date;

/// The registry source strings crates.io uses in `Cargo.lock`.
const CRATES_IO_SOURCES: [&str; 2] = [
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
];
/// Who is asking, as crates.io's crawler policy wants it said.
const USER_AGENT: &str = "baton-xtask (https://github.com/ntanis-dev/baton)";
/// The gap between two requests to crates.io.
const REQUEST_GAP: Duration = Duration::from_millis(100);
/// The cooldown when `--days` is not given.
const DEFAULT_DAYS: u32 = 14;
/// The cache file, under the target folder.
const CACHE_FILE: &str = "xtask-lock-age.json";

/// What the task was asked to do.
#[derive(Debug, PartialEq, Eq)]
pub struct Options {
    /// The minimum age in days.
    pub days: u32,
}

impl Options {
    /// Reads the task's arguments: `[--days N]`.
    pub fn parse(args: &[String]) -> Result<Options, String> {
        let mut days = DEFAULT_DAYS;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let value = if let Some(value) = arg.strip_prefix("--days=") {
                value.to_owned()
            } else if arg == "--days" {
                args.next().cloned().ok_or("`--days` needs a number of days.")?
            } else {
                return Err(format!("unknown argument `{arg}`."));
            };
            days = value
                .parse()
                .map_err(|_| format!("`--days` takes a whole number of days, not `{value}`."))?;
        }
        Ok(Options { days })
    }
}

/// One `[[package]]` entry of `Cargo.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Crate name.
    pub name: String,
    /// Locked version.
    pub version: String,
    /// Where it comes from; `None` for the workspace's own crates.
    pub source: Option<String>,
}

impl Package {
    fn is_crates_io(&self) -> bool {
        self.source
            .as_deref()
            .is_some_and(|source| CRATES_IO_SOURCES.contains(&source))
    }

    fn key(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// Reads the packages of `Cargo.lock` text. The format is simple and stable enough to read by hand:
/// `[[package]]` tables with `name`, `version` and `source` as quoted strings on their own lines.
pub fn parse_lock(text: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut current: Option<Package> = None;
    for line in text.lines() {
        let line = line.trim();
        // Only table headers start with `[`: array items are quoted strings and an array closes with `]`.
        // Any header ends the package before it; another table (`[metadata]`) starts nothing.
        if line.starts_with('[') {
            packages.extend(current.take());
            if line == "[[package]]" {
                current = Some(Package {
                    name: String::new(),
                    version: String::new(),
                    source: None,
                });
            }
            continue;
        }
        let Some(package) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(value) = unquote(value.trim()) else {
            continue;
        };
        match key.trim() {
            "name" => package.name = value,
            "version" => package.version = value,
            "source" => package.source = Some(value),
            _ => {}
        }
    }
    packages.extend(current.take());
    packages.retain(|package| !package.name.is_empty() && !package.version.is_empty());
    packages
}

fn unquote(value: &str) -> Option<String> {
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\"", "\"").replace("\\\\", "\\"))
}

/// A package that is too young, with the day it was published and the day it clears.
#[derive(Debug, PartialEq, Eq)]
pub struct Young {
    /// The package.
    pub package: Package,
    /// Its publish day.
    pub published: date::Date,
    /// The first day it passes.
    pub clears: date::Date,
}

/// The packages published fewer than `days` days before `today` (all as UTC day counts). A package
/// published on day D passes from day D + `days` on.
pub fn too_young(published: &[(Package, i64)], days: u32, today: i64) -> Vec<Young> {
    let mut young: Vec<Young> = published
        .iter()
        .filter(|(_, day)| day + i64::from(days) > today)
        .map(|(package, day)| Young {
            package: package.clone(),
            published: date::civil_from_days(*day),
            clears: date::civil_from_days(day + i64::from(days)),
        })
        .collect();
    young.sort_by(|a, b| {
        a.clears
            .cmp(&b.clears)
            .then_with(|| a.package.name.cmp(&b.package.name))
    });
    young
}

/// Runs the check. `Ok(true)` when every package is old enough, `Ok(false)` when some are not (they are
/// listed on stderr), `Err` when the check itself could not run.
pub fn run(options: &Options) -> Result<bool, String> {
    let root = workspace_root();
    let lock_path = root.join("Cargo.lock");
    let text = std::fs::read_to_string(&lock_path)
        .map_err(|error| format!("could not read {}: {error}", lock_path.display()))?;
    let packages = parse_lock(&text);
    let (crates_io, others): (Vec<Package>, Vec<Package>) =
        packages.into_iter().partition(Package::is_crates_io);
    let foreign: Vec<&Package> = others.iter().filter(|package| package.source.is_some()).collect();

    let cache_path = target_dir(&root).join(CACHE_FILE);
    let mut cache = read_cache(&cache_path);
    let mut fetcher = Fetcher::default();
    let mut published = Vec::with_capacity(crates_io.len());
    let mut failures = Vec::new();
    let mut fetched = 0usize;

    for package in &crates_io {
        let key = package.key();
        let created_at = match cache.get(&key) {
            Some(created_at) => created_at.clone(),
            None => match fetcher.created_at(package) {
                Ok(created_at) => {
                    fetched += 1;
                    cache.insert(key, created_at.clone());
                    created_at
                }
                Err(error) => {
                    failures.push(format!("{} {}: {error}", package.name, package.version));
                    continue;
                }
            },
        };
        match date::rfc3339_day(&created_at) {
            Some(day) => published.push((package.clone(), day)),
            None => failures.push(format!(
                "{} {}: crates.io gave an unreadable date `{created_at}`",
                package.name, package.version
            )),
        }
    }
    if fetched > 0 {
        write_cache(&cache_path, &cache);
    }

    if !failures.is_empty() {
        let mut message = String::from("could not check every package:");
        for failure in &failures {
            message.push_str("\n  ");
            message.push_str(failure);
        }
        return Err(message);
    }

    let today = date::today();
    let young = too_young(&published, options.days, today);
    let note = if foreign.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = foreign.iter().map(|package| package.key()).collect();
        format!(" Not from crates.io, so not checked here: {}.", names.join(", "))
    };
    if young.is_empty() {
        println!(
            "All {} crates.io packages in Cargo.lock are at least {} days old.{note}",
            crates_io.len(),
            options.days
        );
        return Ok(true);
    }

    let width = young
        .iter()
        .map(|entry| entry.package.name.len() + entry.package.version.len() + 1)
        .max()
        .unwrap_or(0);
    eprintln!(
        "These packages in Cargo.lock are younger than {} days (today is {}):",
        options.days,
        date::civil_from_days(today)
    );
    for entry in &young {
        let label = format!("{} {}", entry.package.name, entry.package.version);
        eprintln!(
            "  {label:<width$}  published {}, clears {}",
            entry.published, entry.clears
        );
    }
    eprintln!(
        "{} of {} packages are too young. Pin each back with `cargo update -p NAME --precise VERSION`, \
         or wait until it clears.{note}",
        young.len(),
        crates_io.len()
    );
    Ok(false)
}

/// The workspace root: the parent of this crate's folder.
fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap_or(manifest_dir).to_path_buf()
}

/// `CARGO_TARGET_DIR` when set (relative to the current folder, as cargo reads it), else `<root>/target`.
fn target_dir(root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) if !dir.is_empty() => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                std::env::current_dir().map(|cwd| cwd.join(&dir)).unwrap_or(dir)
            }
        }
        _ => root.join("target"),
    }
}

/// The cache maps `name@version` to crates.io's `created_at`. A missing or unreadable cache is empty.
fn read_cache(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) else {
        return BTreeMap::new();
    };
    map.into_iter()
        .filter_map(|(key, value)| value.as_str().map(|created_at| (key, created_at.to_owned())))
        .collect()
}

/// Writes the cache through a temporary file, so an interrupted run never leaves half a file. A cache
/// that cannot be written only costs the next run some requests, so failures are reported and ignored.
fn write_cache(path: &Path, cache: &BTreeMap<String, String>) {
    let map: Map<String, Value> = cache
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    let text = match serde_json::to_string_pretty(&Value::Object(map)) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("xtask lock-age: could not encode the cache: {error}");
            return;
        }
    };
    let temp = path.with_extension("json.tmp");
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&temp, text))
        .and_then(|()| std::fs::rename(&temp, path));
    if let Err(error) = result {
        eprintln!(
            "xtask lock-age: could not write the cache {}: {error}",
            path.display()
        );
    }
}

/// Asks crates.io, one request per [`REQUEST_GAP`] at most.
#[derive(Debug, Default)]
struct Fetcher {
    last: Option<Instant>,
}

impl Fetcher {
    fn created_at(&mut self, package: &Package) -> Result<String, String> {
        if let Some(last) = self.last {
            let elapsed = last.elapsed();
            if elapsed < REQUEST_GAP {
                thread::sleep(REQUEST_GAP - elapsed);
            }
        }
        self.last = Some(Instant::now());

        let url = format!(
            "https://crates.io/api/v1/crates/{}/{}",
            package.name, package.version
        );
        let output = Command::new("curl")
            .args(["-fsS", "--max-time", "30", "-A", USER_AGENT, &url])
            .output()
            .map_err(|error| format!("could not run curl: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("curl failed for {url}: {}", stderr.trim()));
        }
        let body: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("crates.io answered with something other than JSON: {error}"))?;
        body.pointer("/version/created_at")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "crates.io gave no `version.created_at`".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = "anyhow"
version = "1.0.104"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"

[[package]]
name = "baton-core"
version = "0.1.0"
dependencies = [
 "anyhow",
 "serde 1.0.229",
]

[[package]]
name = "serde"
version = "1.0.229"
source = "sparse+https://index.crates.io/"
dependencies = [
 "serde_derive",
]

[[package]]
name = "forked"
version = "0.1.0"
source = "git+https://example.com/forked#abc"

[metadata]
name = "not-a-package"
"#;

    fn package(name: &str, version: &str) -> Package {
        Package {
            name: name.to_owned(),
            version: version.to_owned(),
            source: Some(CRATES_IO_SOURCES[0].to_owned()),
        }
    }

    #[test]
    fn reads_every_package_and_its_source() {
        let packages = parse_lock(LOCK);
        let names: Vec<&str> = packages.iter().map(|package| package.name.as_str()).collect();
        assert_eq!(names, ["anyhow", "baton-core", "serde", "forked"]);
        assert!(packages[0].is_crates_io());
        assert!(!packages[1].is_crates_io());
        assert_eq!(packages[1].source, None);
        assert!(packages[2].is_crates_io());
        assert!(!packages[3].is_crates_io());
        assert_eq!(packages[2].key(), "serde@1.0.229");
    }

    #[test]
    fn a_package_clears_exactly_days_after_its_publish_day() {
        let today = date::days_from_civil(2026, 11, 2);
        let published = vec![
            (
                package("old-enough", "1.0.0"),
                date::days_from_civil(2026, 10, 19),
            ),
            (
                package("one-day-short", "1.0.0"),
                date::days_from_civil(2026, 10, 20),
            ),
            (package("brand-new", "2.0.0"), today),
        ];
        let young = too_young(&published, 14, today);
        let summary: Vec<String> = young
            .iter()
            .map(|entry| format!("{} {} {}", entry.package.name, entry.published, entry.clears))
            .collect();
        assert_eq!(
            summary,
            [
                "one-day-short 2026-10-20 2026-11-03",
                "brand-new 2026-11-02 2026-11-16"
            ]
        );
    }

    #[test]
    fn options_take_days_in_both_spellings() {
        let args = |list: &[&str]| list.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        assert_eq!(Options::parse(&[]), Ok(Options { days: 14 }));
        assert_eq!(Options::parse(&args(&["--days", "7"])), Ok(Options { days: 7 }));
        assert_eq!(Options::parse(&args(&["--days=30"])), Ok(Options { days: 30 }));
        assert!(Options::parse(&args(&["--days"])).is_err());
        assert!(Options::parse(&args(&["--days", "soon"])).is_err());
        assert!(Options::parse(&args(&["--fast"])).is_err());
    }

    #[test]
    fn the_cache_survives_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("baton-xtask-test-{}", std::process::id()));
        let path = dir.join(CACHE_FILE);
        let mut cache = BTreeMap::new();
        cache.insert("anyhow@1.0.104".to_owned(), "2026-09-01T10:00:00Z".to_owned());
        write_cache(&path, &cache);
        assert_eq!(read_cache(&path), cache);
        std::fs::remove_dir_all(&dir).ok();
        assert!(read_cache(&path).is_empty());
    }
}
