//! The `fake-usage-claude` program: the usage poller's stand-in for `claude`
//! (`baton_testkit::fake_usage`).

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(baton_testkit::fake_usage::run(&argv));
}
