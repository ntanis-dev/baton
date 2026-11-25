//! Reading the history (`history/readings-2026-11.jsonl`, `events-...`) without reading all of it.
//!
//! A history file holds a month, one line per reading or event, appended as they happen: its lines
//! are in the order of their times, give or take the moment two writers append at once. So what is
//! wanted from a moment on starts near where a search of the file's bytes finds that moment, and the
//! lines before it are never read. And a file only ever grows (retention removes a month as a whole):
//! a [`History`] kept from one look to the next (the menu's) reads only what was added since.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

use crate::clock::{HOUR, Ms, ms_of, utc_months};
use crate::json::Get;
use crate::paths::Paths;
use crate::usage::history_file;

/// How much earlier than the moment asked for the reading of a file starts: lines written at once by
/// two processes can stand out of order by this much, and still none is missed.
const SLACK: Ms = HOUR;

/// Below this many bytes between two places in a file, the search stops and reads on from the first.
const CLOSE_ENOUGH: u64 = 64 * 1024;

/// How much of a file one look at a place in it reads: a line and more.
const PROBE: usize = 8 * 1024;

/// How many bytes before where a file was read up to are kept, to tell that it is still that file.
const MARK: u64 = 64;

/// What is held of one month's file.
#[derive(Debug, Default)]
struct Tail {
    /// Where it was read up to: just after a line's end.
    read: u64,
    /// The bytes just before `read`.
    mark: Vec<u8>,
    /// Every line of the file written at `from` or later, in the file's order, with its time.
    lines: Vec<(Ms, Value)>,
    /// The moment the lines held begin at.
    from: Ms,
}

/// What has been read of the history, by kind and month. One kept from one look to the next reads
/// only what was added since; a new one reads from where the moment asked for begins.
#[derive(Debug, Default)]
pub struct History {
    files: BTreeMap<(String, Ms), Tail>,
}

impl History {
    /// A history that has read nothing yet.
    pub fn new() -> History {
        History::default()
    }

    /// Brings what is held of `kind` from `from` up to `to` in step with the files: the lines added
    /// since the last look, or, for a month not held from that early, the file from where `from`
    /// begins. A month no longer asked for is let go.
    pub fn update(&mut self, paths: &Paths, kind: &str, from: Ms, to: Ms) {
        let months = utc_months(from, to);
        self.files
            .retain(|(own, month), _| own != kind || months.contains(month));
        for month in months {
            let file = history_file(paths, kind, month);
            let key = (kind.to_owned(), month);
            match self.files.remove(&key).and_then(|tail| grown(tail, &file, from)) {
                Some(tail) => {
                    self.files.insert(key, tail);
                }
                None => {
                    if let Some(tail) = read_from(&file, from) {
                        self.files.insert(key, tail);
                    }
                }
            }
        }
    }

    /// The lines of `kind` written from `from` to `to`, oldest first, as the last [`History::update`]
    /// found them: one that asked from `from` or earlier.
    pub fn lines(&self, kind: &str, from: Ms, to: Ms) -> Vec<&Value> {
        let mut out: Vec<(Ms, &Value)> = self
            .files
            .iter()
            .filter(|((own, _), _)| own == kind)
            .flat_map(|(_, tail)| tail.lines.iter())
            .filter(|(at, _)| *at >= from && *at <= to)
            .map(|(at, line)| (*at, line))
            .collect();
        out.sort_by_key(|(at, _)| *at);
        out.into_iter().map(|(_, line)| line).collect()
    }
}

/// The history of `kind` between two moments, oldest first, read afresh.
pub fn read_history(paths: &Paths, kind: &str, from: Ms, to: Ms) -> Vec<Value> {
    let mut history = History::new();
    history.update(paths, kind, from, to);
    history.lines(kind, from, to).into_iter().cloned().collect()
}

/// The lines of one text of a file, with their times, and how many bytes of it they took: up to the
/// end of the last whole line. A last line without its end is taken when it is whole JSON (a writer
/// that never ended it), and left for the next look when it is not (one being written).
fn parse(bytes: &[u8], from: Ms) -> (Vec<(Ms, Value)>, usize) {
    let mut out = Vec::new();
    let mut used = 0;
    for piece in bytes.split_inclusive(|byte| *byte == b'\n') {
        let ended = piece.last() == Some(&b'\n');
        let text = String::from_utf8_lossy(piece);
        let text = text.trim_start_matches('\u{feff}').trim();
        let line = (!text.is_empty()).then(|| crate::json::parse(text)).flatten();
        if !ended && line.is_none() {
            break;
        }
        used += piece.len();
        if let Some(line) = line
            && let Some(at) = ms_of(line.at("at"))
            && at >= from
        {
            out.push((at, line));
        }
    }
    (out, used)
}

/// The bytes of `file` from `at` to its end.
fn bytes_from(file: &mut File, at: u64) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut out = Vec::new();
    file.read_to_end(&mut out).ok()?;
    Some(out)
}

/// The first whole line that starts after `at`: where it starts, and its time. None when there is
/// none, or it says no time.
fn line_after(file: &mut File, at: u64) -> Option<(u64, Ms)> {
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut buffer = vec![0u8; PROBE];
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    let buffer = &buffer[..filled];
    let start = buffer.iter().position(|byte| *byte == b'\n')? + 1;
    let length = buffer[start..].iter().position(|byte| *byte == b'\n')?;
    let line = crate::json::parse(String::from_utf8_lossy(&buffer[start..start + length]).trim())?;
    Some((at + start as u64, ms_of(line.at("at"))?))
}

/// Where in a file of `len` bytes to start reading to have every line written at `from` or later:
/// the start of a line written before it, or the file's start.
fn start_of(file: &mut File, len: u64, from: Ms) -> u64 {
    let (mut low, mut high) = (0, len);
    while high - low > CLOSE_ENOUGH {
        let middle = low + (high - low) / 2;
        match line_after(file, middle) {
            Some((start, at)) if start < high && at < from => low = start,
            // Later, or nothing to go by there: start earlier.
            _ => high = middle,
        }
    }
    low
}

/// What is held of a file: its bytes before `read`, to tell it again.
fn mark_of(file: &mut File, read: u64) -> Option<Vec<u8>> {
    let start = read.saturating_sub(MARK);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut out = vec![0u8; (read - start) as usize];
    file.read_exact(&mut out).ok()?;
    Some(out)
}

/// A month's file read from where `from` begins; None when there is no such file.
fn read_from(path: &Path, from: Ms) -> Option<Tail> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = start_of(&mut file, len, from - SLACK);
    let bytes = bytes_from(&mut file, start)?;
    let (lines, used) = parse(&bytes, from);
    let read = start + used as u64;
    Some(Tail {
        read,
        mark: mark_of(&mut file, read)?,
        lines,
        from,
    })
}

/// What is held of a file brought in step with it: the lines added since, those before `from` let
/// go. None when it cannot be: the file is held from later than `from`, or it is not the file that was
/// read (it is gone, shorter, or another in its place).
fn grown(mut tail: Tail, path: &Path, from: Ms) -> Option<Tail> {
    if tail.from > from {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len < tail.read || mark_of(&mut file, tail.read)? != tail.mark {
        return None;
    }
    if len > tail.read {
        let bytes = bytes_from(&mut file, tail.read)?;
        let (lines, used) = parse(&bytes, tail.from);
        tail.lines.extend(lines);
        tail.read += used as u64;
        tail.mark = mark_of(&mut file, tail.read)?;
    }
    if from > tail.from {
        tail.lines.retain(|(at, _)| *at >= from);
        tail.from = from;
    }
    Some(tail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::{DAY, iso};
    use crate::fsutil::{append_json_line, append_text};
    use crate::paths::baton_paths;
    use serde_json::json;
    use std::fs;

    /// 2026-11-01T00:00:00Z.
    const NOV: Ms = 1_793_491_200_000;

    fn line(at: Ms, n: usize) -> Value {
        json!({ "at": iso(at), "n": n, "pad": "x".repeat(150) })
    }

    fn numbers(lines: &[&Value]) -> Vec<u64> {
        lines.iter().filter_map(|line| line["n"].as_u64()).collect()
    }

    #[test]
    fn a_month_is_read_from_where_the_moment_asked_for_begins() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let file = history_file(&paths, "readings", NOV);
        // A line a minute for twelve days: well over a megabyte.
        let count = 12 * 24 * 60;
        let mut text = String::new();
        for n in 0..count {
            text.push_str(&crate::json::compact(&line(NOV + n as Ms * 60_000, n)));
            text.push('\n');
        }
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, &text).unwrap();
        let from = NOV + 10 * DAY;
        let to = NOV + 11 * DAY;
        let mut history = History::new();
        history.update(&paths, "readings", from, to);
        let got = history.lines("readings", from, to);
        assert_eq!(numbers(&got), (10 * 1440..=11 * 1440).collect::<Vec<u64>>());
        // Reading starts near the moment (an hour before it), not at the top of the file.
        let first = text
            .lines()
            .take(10 * 1440 - 60)
            .map(|line| line.len() as u64 + 1)
            .sum::<u64>();
        let mut opened = File::open(&file).unwrap();
        let start = start_of(&mut opened, text.len() as u64, from - SLACK);
        assert!(
            start <= first && first - start <= CLOSE_ENOUGH + 1024,
            "{start} {first}"
        );
        assert_eq!(read_history(&paths, "readings", from, to).len(), got.len());
        // Two lines out of order by a few minutes, around the moment: neither is missed.
        let mut lines: Vec<&str> = text.lines().collect();
        lines.swap(10 * 1440 - 3, 10 * 1440 + 2);
        fs::write(&file, lines.join("\n") + "\n").unwrap();
        let again = read_history(&paths, "readings", from, to);
        assert_eq!(again.len(), 1441);
    }

    #[test]
    fn a_history_kept_reads_only_what_was_added_and_starts_over_for_another_file() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let file = history_file(&paths, "events", NOV);
        for n in 0..5 {
            append_json_line(&file, &line(NOV + n as Ms * HOUR, n)).unwrap();
        }
        let mut history = History::new();
        let to = NOV + 20 * DAY;
        history.update(&paths, "events", NOV, to);
        assert_eq!(numbers(&history.lines("events", NOV, to)), [0, 1, 2, 3, 4]);
        // Added since: only that is read.
        append_json_line(&file, &line(NOV + 5 * HOUR, 5)).unwrap();
        // One being written, not ended yet: left for the next look.
        append_text(&file, "{\"at\":\"2026-11-01T06").unwrap();
        let read = history.files.values().next().unwrap().read;
        history.update(&paths, "events", NOV, to);
        let held = history.files.values().next().unwrap();
        assert!(held.read > read);
        assert_eq!(numbers(&history.lines("events", NOV, to)), [0, 1, 2, 3, 4, 5]);
        append_text(&file, ":00:00.000Z\",\"n\":6}\n").unwrap();
        history.update(&paths, "events", NOV + 2 * HOUR, to);
        assert_eq!(numbers(&history.lines("events", NOV, to)), [2, 3, 4, 5, 6]);
        // Earlier than it holds: read again from there.
        history.update(&paths, "events", NOV, to);
        assert_eq!(numbers(&history.lines("events", NOV, to)), [0, 1, 2, 3, 4, 5, 6]);
        // Another file in its place (one removed and written anew), even a longer one: read anew.
        fs::remove_file(&file).unwrap();
        for n in 10..20 {
            append_json_line(&file, &line(NOV + n as Ms * HOUR, n)).unwrap();
        }
        history.update(&paths, "events", NOV, to);
        assert_eq!(
            numbers(&history.lines("events", NOV, to)),
            (10..20).collect::<Vec<u64>>()
        );
        // A month that is gone, and one no longer asked for, are let go.
        fs::remove_file(&file).unwrap();
        history.update(&paths, "events", NOV, to);
        assert!(history.lines("events", NOV, to).is_empty());
        assert!(history.files.is_empty());
    }

    #[test]
    fn months_are_read_together_and_lines_without_a_time_are_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let paths = baton_paths(dir.path());
        let oct = NOV - DAY;
        append_json_line(&history_file(&paths, "readings", oct), &line(oct, 1)).unwrap();
        let file = history_file(&paths, "readings", NOV);
        append_text(&file, "not json\n{\"n\":2}\n\n").unwrap();
        append_json_line(&file, &line(NOV, 3)).unwrap();
        // A last line nobody ended, whole: taken.
        append_text(&file, &crate::json::compact(&line(NOV + 1, 4))).unwrap();
        let got = read_history(&paths, "readings", oct, NOV + DAY);
        assert_eq!(
            got.iter()
                .filter_map(|line| line["n"].as_u64())
                .collect::<Vec<_>>(),
            [1, 3, 4]
        );
        assert!(read_history(&baton_paths(dir.path().join("none")), "readings", oct, NOV).is_empty());
    }
}
