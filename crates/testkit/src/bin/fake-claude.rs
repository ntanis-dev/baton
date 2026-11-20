//! The `fake-claude` program: the session wrapper's stand-in for `claude` (`baton_testkit::fake_claude`).

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(baton_testkit::fake_claude::run(&argv));
}
