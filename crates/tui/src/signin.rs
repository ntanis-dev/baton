//! Claude Code's own sign-in, with Baton's screen around it.
//!
//! Claude Code owns the browser sign-in and reads what is typed (a verification code) straight from
//! the terminal. Of what it prints, only its public sign-in link and whether it asks for a code reach
//! the screen: no raw output, and nothing that was typed, is shown or kept.

use std::io::{self, Read};
use std::process::{Child, Stdio};
use std::sync::LazyLock;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use baton_claude::lead::{ProcessSignals, Signals};
use baton_core::claude_path::Launcher;
use baton_core::env::Env;
use baton_core::text::clean;
use baton_core::theme::{GlyphSet, pick_glyphs};
use regex::Regex;

use crate::console::{Console, StdConsole};
use crate::layout::{SIDEBAR_COLS, Size, too_small};
use crate::paint::{Painter, WRAP_OFF, WRAP_ON, row_text};
use crate::state::{Init, initial_state};
use crate::ui::kit::theme_of;
use crate::ui::login::login_frame;

/// Something that looks like a link.
static LINK: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r#"https://[^\s<>"']+"#).ok());
/// A link's place in a line that is not whole yet.
static LINK_START: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"https://\S+").ok());
/// Words that ask for a code.
static PROMPT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:paste|enter)[^\n]{0,80}(?:authorization|verification|code)|(?:authorization|verification) code",
    )
    .ok()
});
/// The same, in a line that is not whole yet.
static PROMPT_START: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)(?:paste|enter)[^\n]{0,80}code").ok());
/// The hosts a sign-in link may be on.
static HOSTS: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(^|\.)(claude\.ai|claude\.com|anthropic\.com)$").ok());
/// Names of what a public link never carries.
static SECRETS: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"(?i)token|secret|password").ok());

/// A percent-encoded part of a link, decoded; `+` is a space.
fn decoded(part: &str) -> String {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |at: usize| bytes.get(at).and_then(|b| char::from(*b).to_digit(16));
        match (bytes[i], hex(i + 1), hex(i + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                i += 3;
            }
            (b'+', _, _) => {
                out.push(b' ');
                i += 1;
            }
            (byte, _, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first public authorization link in a text, or None. Only public authorization links belong on
/// the screen: never arbitrary output, credentials, callbacks or links that carry a token.
pub fn authorization_url(text: &str) -> Option<String> {
    let links = LINK.as_ref()?;
    for candidate in links.find_iter(text) {
        let link = candidate.as_str();
        let rest = &link["https://".len()..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        // Someone's name or password in front of the host.
        if authority.contains('@') || authority.is_empty() {
            continue;
        }
        let host = authority.split(':').next().unwrap_or(authority).to_lowercase();
        if !HOSTS.as_ref().is_some_and(|hosts| hosts.is_match(&host)) {
            continue;
        }
        let (before_hash, hash) = tail.split_once('#').map_or((tail, ""), |(a, b)| (a, b));
        if !hash.is_empty() {
            continue;
        }
        let (path, query) = before_hash
            .split_once('?')
            .map_or((before_hash, ""), |(a, b)| (a, b));
        if !matches!(
            path,
            "/authorize" | "/authorize/" | "/oauth/authorize" | "/oauth/authorize/"
        ) {
            continue;
        }
        let secret = query.split('&').filter(|pair| !pair.is_empty()).any(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let (key, value) = (decoded(key), decoded(value));
            SECRETS.as_ref().is_some_and(|secrets| secrets.is_match(&key))
                || (key == "code" && value != "true" && value != "false")
        });
        if secret {
            continue;
        }
        return Some(format!("https://{}{tail}", authority.to_lowercase()));
    }
    None
}

/// What the sign-in screen knows of Claude Code's sign-in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginUpdate {
    /// Its public sign-in link, once it printed one.
    pub url: Option<String>,
    /// Whether it asks for a verification code.
    pub code_prompt: bool,
}

/// Reads what `claude auth login` prints for its sign-in link and its code prompt.
#[derive(Clone, Debug, Default)]
pub struct LoginOutput {
    /// The bytes of a character that has not arrived whole.
    partial: Vec<u8>,
    /// The line that has not ended yet.
    pending: String,
    /// What is known so far.
    known: LoginUpdate,
}

impl LoginOutput {
    /// A reader that knows nothing yet.
    pub fn new() -> LoginOutput {
        LoginOutput::default()
    }

    /// What is known so far.
    pub fn known(&self) -> &LoginUpdate {
        &self.known
    }

    /// Looks at a piece of text; what is known now, when it is more than before.
    fn inspect(&mut self, text: &str) -> Option<LoginUpdate> {
        let visible = clean(text);
        let found = authorization_url(&visible);
        let prompt = PROMPT.as_ref().is_some_and(|prompt| prompt.is_match(&visible));
        let new_link = found.is_some() && found != self.known.url;
        if !new_link && (!prompt || self.known.code_prompt) {
            return None;
        }
        if found.is_some() {
            self.known.url = found;
        }
        self.known.code_prompt |= prompt;
        Some(self.known.clone())
    }

    /// Takes the next bytes of output; each time more is known, what is known then.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<LoginUpdate> {
        self.partial.extend_from_slice(chunk);
        let whole = match std::str::from_utf8(&self.partial) {
            Ok(text) => text.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.partial.len(),
        };
        self.pending
            .push_str(&String::from_utf8_lossy(&self.partial[..whole]));
        self.partial.drain(..whole);
        let mut updates = Vec::new();
        let text = std::mem::take(&mut self.pending);
        let mut lines: Vec<&str> = text.split('\n').collect();
        let last = lines.pop().unwrap_or("");
        for line in lines {
            updates.extend(self.inspect(line.strip_suffix('\r').unwrap_or(line)));
        }
        // An unfinished line is kept, its end only when it runs very long.
        let keep = last.chars().count().saturating_sub(32_768);
        self.pending = last.chars().skip(keep).collect();
        // A prompt need not end with a newline. Links wait for a whole line.
        let asks = PROMPT_START
            .as_ref()
            .is_some_and(|prompt| prompt.is_match(&clean(&self.pending)));
        if asks {
            let unlinked = match LINK_START.as_ref() {
                Some(link) => link.replace_all(&self.pending, "").into_owned(),
                None => self.pending.clone(),
            };
            updates.extend(self.inspect(&unlinked));
        }
        updates
    }

    /// The output ended: the last line counts as whole.
    pub fn end(&mut self) -> Vec<LoginUpdate> {
        let mut rest = std::mem::take(&mut self.pending);
        rest.push_str(&String::from_utf8_lossy(&std::mem::take(&mut self.partial)));
        self.inspect(&rest).into_iter().collect()
    }
}

/// What `claude auth login` gets for its input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoginStdio {
    /// The terminal: Claude Code reads the verification code itself.
    #[default]
    Inherit,
    /// Nothing (tests, a preview).
    Ignore,
}

/// What a sign-in is run with.
pub struct SignInOptions {
    /// How Claude Code is started.
    pub launcher: Launcher,
    /// The environment it runs with: the account's profile is its `CLAUDE_CONFIG_DIR`
    /// (`baton_core::paths::profile_env`).
    pub env: Env,
    /// The account's name, for the screen.
    pub name: String,
    /// The terminal to draw on; None for the process's own.
    pub console: Option<Box<dyn Console>>,
    /// What Claude Code gets for its input.
    pub stdio: LoginStdio,
    /// Whether colours are drawn.
    pub color: bool,
    /// Which glyph set is drawn; None picks by the environment.
    pub glyphs: Option<GlyphSet>,
    /// The interrupts ignored while Claude Code has the terminal (Ctrl+C is Claude Code's to take);
    /// None for the process's own on its own terminal, and for nothing on a `console` that was handed
    /// in.
    pub signals: Option<Box<dyn Signals>>,
}

impl SignInOptions {
    /// A sign-in on the process's terminal, in colour unless `NO_COLOR` is set.
    pub fn new(launcher: Launcher, env: Env, name: &str) -> SignInOptions {
        SignInOptions {
            color: !env.has("NO_COLOR"),
            launcher,
            env,
            name: name.to_owned(),
            console: None,
            stdio: LoginStdio::Inherit,
            glyphs: None,
            signals: None,
        }
    }
}

impl std::fmt::Debug for SignInOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInOptions")
            .field("launcher", &self.launcher)
            .field("name", &self.name)
            .field("stdio", &self.stdio)
            .field("color", &self.color)
            .field("glyphs", &self.glyphs)
            .finish_non_exhaustive()
    }
}

/// What a sign-in needs besides a terminal.
#[derive(Clone, Debug)]
pub struct SignIn<'a> {
    /// How Claude Code is started.
    pub launcher: &'a Launcher,
    /// The environment it runs with (the account's profile).
    pub env: &'a Env,
    /// The account's name.
    pub name: &'a str,
    /// What Claude Code gets for its input.
    pub stdio: LoginStdio,
    /// Whether colours are drawn.
    pub color: bool,
    /// Which glyph set is drawn.
    pub glyphs: GlyphSet,
    /// The colour depth of the terminal, in bits.
    pub depth: u8,
}

/// Runs `claude auth login` under an account's profile, with Baton's screen around it, and returns
/// its exit code (1 when it could not be started).
pub fn sign_in(mut options: SignInOptions) -> i32 {
    let own_terminal = options.console.is_none();
    let mut signals = options
        .signals
        .take()
        .or_else(|| own_terminal.then(|| Box::new(ProcessSignals) as Box<dyn Signals>));
    let mut own;
    let console: &mut dyn Console = match options.console.as_mut() {
        Some(console) => console.as_mut(),
        None => {
            own = StdConsole::new();
            &mut own
        }
    };
    let spec = SignIn {
        launcher: &options.launcher,
        env: &options.env,
        name: &options.name,
        stdio: options.stdio,
        color: options.color,
        glyphs: options.glyphs.unwrap_or_else(|| pick_glyphs(&options.env)),
        depth: baton_core::term::color_depth(&options.env),
    };
    let names = interrupts();
    if let Some(signals) = signals.as_mut() {
        signals.ignore(names);
    }
    let code = sign_in_on(console, &spec);
    if let Some(signals) = signals.as_mut() {
        signals.restore(names);
    }
    code
}

/// The interrupts that are Claude Code's while it has the terminal.
pub fn interrupts() -> &'static [&'static str] {
    if cfg!(windows) {
        &["SIGINT", "SIGBREAK"]
    } else {
        &["SIGINT", "SIGQUIT"]
    }
}

/// What a reader of Claude Code's output sends on.
enum Piece {
    /// Some of what it printed.
    Bytes(Vec<u8>),
    /// That output ended.
    End,
}

/// Sends what a pipe gives on, piece by piece, with which output it is (0: standard output, 1:
/// standard error).
fn forward(mut pipe: impl Read + Send + 'static, which: usize, to: mpsc::Sender<(usize, Piece)>) {
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if to.send((which, Piece::Bytes(buffer[..n].to_vec()))).is_err() {
                        return;
                    }
                }
            }
        }
        let _ = to.send((which, Piece::End));
    });
}

/// Draws the sign-in screen.
struct Screen<'a> {
    /// What the sign-in is run with.
    spec: &'a SignIn<'a>,
    /// What was last drawn.
    painter: Painter,
    /// The size it was last drawn at.
    size: Size,
}

impl Screen<'_> {
    /// Draws the screen for what is known of the sign-in.
    fn draw(&mut self, console: &mut dyn Console, login: &LoginUpdate) -> io::Result<()> {
        let (cols, rows) = console.size().unwrap_or((100, 28));
        let size = Size { cols, rows };
        if size != self.size {
            self.size = size;
            self.painter.reset();
        }
        let state = initial_state(Init {
            size,
            color: self.spec.color,
            glyphs: self.spec.glyphs,
            ..Init::default()
        });
        let theme = theme_of(&state);
        let frame = login_frame(&state, self.spec.name, login.url.as_deref(), login.code_prompt);
        let rows: Vec<String> = frame
            .rows
            .iter()
            .enumerate()
            .map(|(y, row)| {
                row_text(
                    row,
                    &theme,
                    self.spec.depth,
                    frame.link.as_ref().filter(|link| link.row == y),
                )
            })
            .collect();
        let mut out = format!("\x1b[?25l{}", self.painter.send(rows));
        if login.code_prompt && !too_small(&state) {
            // The cursor after the prompt, where Claude Code echoes what is typed.
            out.push_str(&format!(
                "\x1b[{};{}H\x1b[?25h",
                size.rows.saturating_sub(3),
                SIDEBAR_COLS + 6
            ));
        }
        console.write(&out)
    }
}

/// Runs the sign-in on a terminal that is already the caller's (the menu steps aside for it). On a
/// terminal Claude Code's output is read for its link and its prompt; elsewhere it goes where the
/// process's own output goes.
pub fn sign_in_on(console: &mut dyn Console, spec: &SignIn<'_>) -> i32 {
    let tty = console.is_tty();
    let args = ["auth".to_owned(), "login".to_owned()];
    let mut command = spec.launcher.command(&args);
    spec.env.apply(&mut command);
    let input = || {
        if spec.stdio == LoginStdio::Ignore {
            Stdio::null()
        } else {
            Stdio::inherit()
        }
    };
    if !tty {
        command.stdin(input()).stdout(input()).stderr(input());
        return match command.status() {
            Ok(status) => status.code().unwrap_or(1),
            Err(_) => 1,
        };
    }
    let mut screen = Screen {
        spec,
        painter: Painter::new(),
        size: Size { cols: 0, rows: 0 },
    };
    let mut login = LoginUpdate::default();
    let _ = console.write(&format!("\x1b[?1049h{WRAP_OFF}\x1b[2J\x1b[H"));
    let _ = screen.draw(console, &login);
    command
        .stdin(input())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let code = match command.spawn() {
        Ok(child) => follow(child, console, &mut screen, &mut login),
        Err(_) => 1,
    };
    let _ = console.write(&format!("\x1b[0m\x1b[?25h\x1b[?1049l{WRAP_ON}"));
    code
}

/// Follows a running sign-in: its output for the link and the prompt, the terminal for a new size,
/// until it ends. Its exit code.
fn follow(
    mut child: Child,
    console: &mut dyn Console,
    screen: &mut Screen<'_>,
    login: &mut LoginUpdate,
) -> i32 {
    let (tx, rx) = mpsc::channel();
    let mut readers = [LoginOutput::new(), LoginOutput::new()];
    let mut open = 0;
    if let Some(out) = child.stdout.take() {
        forward(out, 0, tx.clone());
        open += 1;
    }
    if let Some(err) = child.stderr.take() {
        forward(err, 1, tx.clone());
        open += 1;
    }
    drop(tx);
    let mut observe = |updates: Vec<LoginUpdate>, console: &mut dyn Console, screen: &mut Screen<'_>| {
        for update in updates {
            if update.url.is_some() {
                login.url = update.url;
            }
            login.code_prompt |= update.code_prompt;
            let _ = screen.draw(console, login);
        }
    };
    while open > 0 {
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok((which, Piece::Bytes(bytes))) => {
                let updates = readers[which].push(&bytes);
                observe(updates, console, screen);
            }
            Ok((which, Piece::End)) => {
                let updates = readers[which].end();
                observe(updates, console, screen);
                open -= 1;
            }
            Err(RecvTimeoutError::Timeout) => {
                // A new size draws the screen again.
                let (cols, rows) = console.size().unwrap_or((100, 28));
                if (Size { cols, rows }) != screen.size {
                    observe(vec![LoginUpdate::default()], console, screen);
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    match child.wait() {
        Ok(status) => status.code().unwrap_or(1),
        Err(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://claude.ai/oauth/authorize?client_id=fixture&state=public&code=true";

    #[test]
    fn sign_in_extracts_a_public_link_across_chunks_and_a_prompt_without_exposing_raw_output() {
        let mut updates = Vec::new();
        let mut parser = LoginOutput::new();
        updates.extend(parser.push(b"unrelated output\nhttps://claude.ai/oauth/auth"));
        assert_eq!(updates.len(), 0);
        updates.extend(parser.push(b"orize?client_id=fixture&state=public&code=true\n"));
        assert_eq!(
            updates.last(),
            Some(&LoginUpdate {
                url: Some(URL.to_owned()),
                code_prompt: false
            })
        );
        updates.extend(parser.push(b"Paste the verification code here: "));
        assert_eq!(
            updates.last(),
            Some(&LoginUpdate {
                url: Some(URL.to_owned()),
                code_prompt: true
            })
        );
        updates.extend(parser.end());
        assert_eq!(updates.len(), 2);
        assert_eq!(
            parser.known(),
            &LoginUpdate {
                url: Some(URL.to_owned()),
                code_prompt: true
            }
        );
    }

    #[test]
    fn sign_in_ignores_tokens_callbacks_untrusted_domains_and_terminal_escapes() {
        let mut updates = Vec::new();
        let mut parser = LoginOutput::new();
        for invalid in [
            "https://claude.ai/oauth/authorize?access_token=private",
            "https://claude.ai/oauth/authorize?code=private",
            "https://claude.ai/oauth/callback?code=private",
            "https://claude.ai.example.com/oauth/authorize?client_id=fixture",
            "https://example.com/oauth/authorize?client_id=fixture",
            "https://someone:private@claude.ai/oauth/authorize",
            "https://claude.ai/oauth/authorize#private",
        ] {
            updates.extend(parser.push(format!("{invalid}\n").as_bytes()));
        }
        updates.extend(parser.push(b"\x1b[2Jthis output must not be displayed\n"));
        updates.extend(parser.end());
        assert_eq!(updates, vec![]);
    }

    #[test]
    fn which_links_are_public_sign_in_links() {
        let ok = |link: &str| authorization_url(&format!("open {link} now")).as_deref() == Some(link);
        assert!(ok(URL));
        assert!(ok("https://claude.com/authorize?client_id=x"));
        assert!(ok(
            "https://console.anthropic.com/oauth/authorize/?client_id=x&code=false"
        ));
        assert_eq!(
            authorization_url("https://CLAUDE.AI/oauth/authorize?state=x").as_deref(),
            Some("https://claude.ai/oauth/authorize?state=x")
        );
        for bad in [
            "http://claude.ai/oauth/authorize",
            "https://claude.ai/",
            "https://claude.ai/oauth/authorize/more",
            "https://notclaude.ai/oauth/authorize",
            "https://claude.ai/oauth/authorize?client%5Fsecret=1",
            "https://claude.ai/oauth/authorize?Password=1",
            "https://claude.ai/oauth/authorize?%63ode=abc",
        ] {
            assert_eq!(authorization_url(bad), None, "{bad}");
        }
        // The first public one wins; a quote ends a link.
        assert_eq!(
            authorization_url("\"https://example.com/x\" then 'https://claude.ai/authorize?x=1'").as_deref(),
            Some("https://claude.ai/authorize?x=1")
        );
    }

    #[test]
    fn a_prompt_without_a_line_end_shows_and_a_link_waits_for_its_line() {
        let mut parser = LoginOutput::new();
        // A link in an unfinished line is not taken, even beside a prompt.
        let updates = parser.push(b"Enter the code from https://claude.ai/oauth/authorize?client_id=x");
        assert_eq!(
            updates,
            vec![LoginUpdate {
                url: None,
                code_prompt: true
            }]
        );
        let updates = parser.push(b"&state=y\r\n");
        assert_eq!(
            updates,
            vec![LoginUpdate {
                url: Some("https://claude.ai/oauth/authorize?client_id=x&state=y".to_owned()),
                code_prompt: true
            }]
        );
        // Characters cut in two are put together again.
        let mut parser = LoginOutput::new();
        let text = "caf\u{e9}: authorization code\n".as_bytes();
        assert_eq!(parser.push(&text[..4]), vec![]);
        assert_eq!(
            parser.push(&text[4..]),
            vec![LoginUpdate {
                url: None,
                code_prompt: true
            }]
        );
    }
}
