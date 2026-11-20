//! The `baton` executable.

use std::process::ExitCode;

fn main() -> ExitCode {
    let code = baton::cli::main();
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
