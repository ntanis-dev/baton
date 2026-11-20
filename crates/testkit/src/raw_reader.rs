//! Reads the console in raw mode (as Claude Code does) and logs every key it receives as JSON lines to
//! the file named by its first argument; ends after two Ctrl+C (code 0) or after the milliseconds of
//! its second argument (code 2, 10 seconds when not given). A console that cannot be read is logged
//! as `{ "error": ... }` (code 3).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use baton_core::fsutil::append_json_line;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde_json::{Value, json};

/// A key press as the text a raw-mode reader of the console would get for it.
pub fn key_text(key: &KeyEvent) -> Option<String> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    Some(match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii_alphabetic() => {
            char::from((c.to_ascii_lowercase() as u8) & 0x1f).to_string()
        }
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "\r".to_owned(),
        KeyCode::Esc => "\x1b".to_owned(),
        KeyCode::Tab => "\t".to_owned(),
        KeyCode::Backspace => "\x7f".to_owned(),
        _ => return None,
    })
}

/// Appends one entry to the log.
fn write(log: &Path, entry: &Value) {
    append_json_line(log, entry).expect("the reader's log");
}

/// Runs the reader with these arguments; returns its exit code.
pub fn run(argv: &[String]) -> i32 {
    let log = PathBuf::from(argv.first().expect("a log file"));
    fs::write(&log, "").expect("the reader's log");
    let limit = argv
        .get(1)
        .and_then(|ms| ms.parse::<u64>().ok())
        .unwrap_or(10_000);
    if let Err(error) = crossterm::terminal::enable_raw_mode() {
        write(&log, &json!({ "error": error.to_string() }));
        return 3;
    }
    write(&log, &json!({ "ready": true }));
    let deadline = Instant::now() + Duration::from_millis(limit);
    let mut ctrl_c = 0;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !event::poll(left).unwrap_or(false) {
            let _ = crossterm::terminal::disable_raw_mode();
            return 2;
        }
        let Ok(Event::Key(key)) = event::read() else {
            continue;
        };
        let Some(text) = key_text(&key) else {
            continue;
        };
        write(&log, &json!({ "data": text }));
        ctrl_c += text.chars().filter(|c| *c == '\x03').count();
        if ctrl_c >= 2 {
            let _ = crossterm::terminal::disable_raw_mode();
            return 0;
        }
    }
}
