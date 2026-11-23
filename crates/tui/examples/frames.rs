//! The menu's frames on the fixtures of `tests/common`, as plain text, for review:
//!
//! ```text
//! cargo run -p baton-tui --example frames -- [dir]
//! ```
//!
//! Every frame of `tests/common/frames.rs`, one file each (default dir: `target/frames`). The times in
//! them are the clock's of where this runs; the ones on record under `tests/golden` are UTC's.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::fs;
use std::path::PathBuf;

use common::frames::{as_text, every_frame};

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/frames".to_owned()),
    );
    fs::create_dir_all(&dir).unwrap();
    let frames = every_frame();
    for (name, rows) in &frames {
        fs::write(dir.join(format!("{name}.txt")), as_text(rows)).unwrap();
    }
    println!("Wrote {} frames to {}.", frames.len(), dir.display());
}
