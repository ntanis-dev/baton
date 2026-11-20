//! The `claude-stub` program: a stand-in claude for `baton doctor`. It says its version, and for
//! `auth status --json` whether it runs under a profile, with an email that Baton must never repeat.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let profile = std::env::var_os("CLAUDE_CONFIG_DIR").is_some_and(|dir| !dir.is_empty());
    match args.join(" ").as_str() {
        "--version" => println!("2.1.302 (Claude Code)"),
        "auth status --json" => {
            let method = if profile { "claude.ai" } else { "none" };
            println!(
                "{{\"loggedIn\":{profile},\"authMethod\":\"{method}\",\"email\":\"someone@example.invalid\"}}"
            );
            std::process::exit(if profile { 0 } else { 1 });
        }
        _ => {}
    }
}
