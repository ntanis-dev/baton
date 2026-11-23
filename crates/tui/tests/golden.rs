//! The menu's frames against the ones on record under `tests/golden`: a change to how anything is
//! drawn shows as a difference to look at, where the other tests only say what a few lines hold.
//!
//! The frames show times, so they are drawn with the clock on UTC: the test runs itself again as a
//! child whose `TZ` says so, whatever the zone of where it runs.
//!
//! When a frame changed on purpose, write the records anew and read what changed with `git diff`:
//!
//! ```text
//! BATON_GOLDEN=write cargo test -p baton-tui --test golden
//! ```
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::frames::{as_text, every_frame};

/// Set in the child that draws the frames.
const CHILD: &str = "BATON_GOLDEN_CHILD";
/// `write`: the frames as they are drawn now go on record.
const WRITE: &str = "BATON_GOLDEN";

/// Where the frames on record are.
fn records() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

/// Where a frame's rows first differ from its record: the row, as it is on record and as it is drawn.
fn difference(name: &str, kept: &str, drawn: &str) -> String {
    let (kept, drawn): (Vec<&str>, Vec<&str>) = (kept.lines().collect(), drawn.lines().collect());
    let at = (0..kept.len().max(drawn.len()))
        .find(|row| kept.get(*row) != drawn.get(*row))
        .unwrap_or(0);
    let row = |rows: &[&str]| {
        rows.get(at)
            .map_or("(no such row)".to_owned(), |row| format!("{row:?}"))
    };
    format!(
        "{name}, row {}:\n  on record: {}\n  drawn:     {}",
        at + 1,
        row(&kept),
        row(&drawn)
    )
}

#[test]
fn every_frame_is_the_one_on_record() {
    if std::env::var_os(CHILD).is_none() {
        let again = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "every_frame_is_the_one_on_record", "--nocapture"])
            .env("TZ", "UTC")
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            again.status.success(),
            "{}{}",
            String::from_utf8_lossy(&again.stdout),
            String::from_utf8_lossy(&again.stderr)
        );
        return;
    }
    let dir = records();
    let frames = every_frame();
    let names: BTreeSet<String> = frames.iter().map(|(name, _)| format!("{name}.txt")).collect();
    assert_eq!(names.len(), frames.len(), "every frame has a name of its own");
    let on_record: BTreeSet<String> = fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    if std::env::var_os(WRITE).is_some_and(|value| value == "write") {
        fs::create_dir_all(&dir).unwrap();
        for gone in on_record.difference(&names) {
            fs::remove_file(dir.join(gone)).unwrap();
        }
        for (name, rows) in &frames {
            fs::write(dir.join(format!("{name}.txt")), as_text(rows)).unwrap();
        }
        println!("Wrote {} frames to {}.", frames.len(), dir.display());
        return;
    }
    let mut wrong: Vec<String> = Vec::new();
    for (name, rows) in &frames {
        let drawn = as_text(rows);
        match fs::read_to_string(dir.join(format!("{name}.txt"))) {
            // A checkout may have given the records the line ends of its system.
            Ok(kept) if kept.replace("\r\n", "\n") == drawn => {}
            Ok(kept) => wrong.push(difference(name, &kept.replace("\r\n", "\n"), &drawn)),
            Err(_) => wrong.push(format!("{name}: drawn, and not on record")),
        }
    }
    for gone in on_record.difference(&names) {
        wrong.push(format!("{gone}: on record, and drawn no more"));
    }
    assert!(
        wrong.is_empty(),
        "{} of {} frames are not the ones on record. If that is meant, write them anew with \
         BATON_GOLDEN=write cargo test -p baton-tui --test golden and review git diff.\n\n{}",
        wrong.len(),
        frames.len(),
        wrong.join("\n\n")
    );
}
