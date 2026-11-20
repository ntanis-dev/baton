//! The `fake-login` program: a stand-in for `claude auth status` and `claude auth login`
//! (`baton_testkit::fake_login`).

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(baton_testkit::fake_login::run(&argv));
}
