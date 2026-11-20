//! The `fake-claude-both` program: the fake claude for a whole baton instance. It is the session
//! wrapper's claude (`baton_testkit::fake_claude`, its scenario) and the usage poller's reads (`auth
//! status` and the stream-json answer to "/usage": `baton_testkit::fake_usage`).

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let auth =
        argv.first().map(String::as_str) == Some("auth") && argv.get(1).map(String::as_str) == Some("status");
    let code = if auth || argv.iter().any(|arg| arg == "--input-format") {
        baton_testkit::fake_usage::run(&argv)
    } else {
        baton_testkit::fake_claude::run(&argv)
    };
    std::process::exit(code);
}
