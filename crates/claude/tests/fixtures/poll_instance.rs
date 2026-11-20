//! A stand-in for a baton instance (the menu or a baton session wrapper) as far as the usage poll
//! goes: a `PollHost` in a process of its own, with the fake usage claude. It logs every account it
//! polls to `$INSTANCE_LOG` and stops (releasing the lease) when its stdin closes.
//!
//! It is a program of this crate, not of `baton-testkit`, because it needs the `PollHost`: the test
//! support crate cannot depend on the crate whose tests depend on it. The tests find it through
//! `CARGO_BIN_EXE_poll-instance`.
//!
//! ```text
//! BATON_HOME         the home the instance runs under
//! INSTANCE_LOG       the file its events go to, as JSON lines: ready, polled (with the line), stopped
//! INSTANCE_KIND      menu or start (the default)
//! FAKE_USAGE_CLAUDE  the path of the fake usage claude it polls with
//! HEARTBEAT_MS       the time between two heartbeats (200)
//! STALE_MS           a lease not renewed for this long is free (1500)
//! ```

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use baton_claude::poll_host::{PollHost, PollHostOptions};
use baton_core::claude_path::Launcher;
use baton_core::clock::now_ms;
use baton_core::env::Env;
use baton_core::fsutil::append_json_line;
use baton_core::paths::baton_paths;
use serde_json::json;

fn main() -> ExitCode {
    let env = Env::from_process();
    let (Some(home), Some(log), Some(fake)) = (
        env.some("BATON_HOME"),
        env.some("INSTANCE_LOG"),
        env.some("FAKE_USAGE_CLAUDE"),
    ) else {
        eprintln!("poll-instance: BATON_HOME, INSTANCE_LOG and FAKE_USAGE_CLAUDE are needed");
        return ExitCode::from(2);
    };
    let log = PathBuf::from(log);
    let pid = std::process::id();
    let event = move |event: &str, line: Option<&str>| {
        let mut entry = json!({ "pid": pid, "at": now_ms(), "event": event });
        if let Some(line) = line {
            entry["line"] = json!(line);
        }
        // A log that cannot be written shows as a test that times out.
        let _ = append_json_line(&log, &entry);
    };
    let number = |name: &str| {
        env.get(name)
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|n| *n > 0)
    };
    let mut options = PollHostOptions::new(
        baton_paths(home),
        env.some("INSTANCE_KIND").unwrap_or("start"),
        env.clone(),
    );
    options.launcher = Some(Launcher::program(fake));
    options.heartbeat_ms = number("HEARTBEAT_MS").unwrap_or(200);
    options.stale_ms = number("STALE_MS").map_or(1500, |ms| ms as i64);
    let polled = event.clone();
    options.log = Some(Arc::new(move |line: &str| polled("polled", Some(line))));
    let host = PollHost::new(options);
    host.start();
    event("ready", None);
    // Until stdin closes.
    let mut rest = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut rest);
    host.stop();
    event("stopped", None);
    ExitCode::SUCCESS
}
