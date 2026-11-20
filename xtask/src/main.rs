//! Repository automation for Baton, run as `cargo xtask <task>` (the alias lives in
//! `.cargo/config.toml`).
//!
//! It stands alone on purpose: std and `serde_json` only, and it shells out to `curl` for crates.io, so the
//! check that guards the dependency tree adds almost nothing to it.

mod date;
mod lock_age;

use std::process::ExitCode;

const USAGE: &str = "\
Usage: cargo xtask <task>

Tasks:
  lock-age [--days N]   check that every crates.io package in Cargo.lock is at least N days old (default 14)
  help                  show this help";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((task, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match task.as_str() {
        "lock-age" => match lock_age::Options::parse(rest) {
            Ok(options) => match lock_age::run(&options) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::FAILURE,
                Err(error) => {
                    eprintln!("xtask lock-age: {error}");
                    ExitCode::FAILURE
                }
            },
            Err(error) => {
                eprintln!("xtask lock-age: {error}\n\n{USAGE}");
                ExitCode::from(2)
            }
        },
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("xtask: unknown task `{other}`.\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
