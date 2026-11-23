//! Reading a command's arguments: `--name value`, `--name=value`, `--flag`, `-h`, positionals and `--`.
//!
//! Strict, as Baton's command line has always been: an option the command does not have is an error,
//! and so is a value that looks like an option (write it `--name=-value`). An option that is on or off
//! never takes the argument after it for its value unless that is a word for on or off: `--last-resort
//! Spare` is the flag and a name.

use std::collections::BTreeMap;

use thiserror::Error;

/// What an option takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A value: `--name value` or `--name=value`.
    Text,
    /// Nothing: `--flag`.
    Flag,
    /// On or off: `--name` alone is on; `--name=off`, or `--name off` with one of the words for it
    /// ([`BOOL_WORDS`]). Read back as a text, `true` when it stood alone.
    Bool,
}

/// The words an on-or-off option takes after it: for on, for off, and for whatever the default is.
pub const BOOL_WORDS: [&str; 9] = ["true", "false", "on", "off", "yes", "no", "1", "0", "default"];

/// Whether an argument is a word for on or off.
fn bool_word(arg: &str) -> bool {
    BOOL_WORDS.iter().any(|word| arg.eq_ignore_ascii_case(word))
}

/// An option a command has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    /// Its name, without the dashes.
    pub name: &'static str,
    /// What it takes.
    pub kind: Kind,
    /// Its one-letter form.
    pub short: Option<char>,
}

impl Spec {
    /// An option that takes a value.
    pub const fn text(name: &'static str) -> Spec {
        Spec {
            name,
            kind: Kind::Text,
            short: None,
        }
    }

    /// An option that is on or off.
    pub const fn boolean(name: &'static str) -> Spec {
        Spec {
            name,
            kind: Kind::Bool,
            short: None,
        }
    }

    /// An option that takes nothing.
    pub const fn flag(name: &'static str) -> Spec {
        Spec {
            name,
            kind: Kind::Flag,
            short: None,
        }
    }

    /// The same option with a one-letter form.
    pub const fn short(mut self, letter: char) -> Spec {
        self.short = Some(letter);
        self
    }
}

/// Arguments that cannot be read.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("{0}")]
pub struct ArgError(pub String);

/// A command's arguments, read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// The values of the options that take one, by name; the last one given wins.
    texts: BTreeMap<String, String>,
    /// The flags that were given.
    flags: Vec<String>,
    /// Everything that is not an option, in order.
    pub positionals: Vec<String>,
}

impl Parsed {
    /// The value given for the option `name`.
    pub fn text(&self, name: &str) -> Option<&str> {
        self.texts.get(name).map(String::as_str)
    }

    /// Whether the flag `name` was given.
    pub fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|flag| flag == name)
    }

    /// The positional at `index`.
    pub fn positional(&self, index: usize) -> Option<&str> {
        self.positionals.get(index).map(String::as_str)
    }
}

/// An option as it was typed, on its way to being checked.
struct Token {
    /// The option's name.
    name: String,
    /// What was typed: `--name`, `-n`.
    raw: String,
    /// The value that came with it.
    value: Option<String>,
    /// Whether the value was written `--name=value`.
    inline: bool,
}

/// Whether a value looks like an option.
fn option_like(value: &str) -> bool {
    value.len() > 1 && value.starts_with('-')
}

/// Reads `args` against the options a command has.
pub fn parse_args(args: &[String], specs: &[Spec]) -> Result<Parsed, ArgError> {
    let by_name = |name: &str| specs.iter().find(|spec| spec.name == name);
    let by_short = |letter: char| specs.iter().find(|spec| spec.short == Some(letter));
    let mut parsed = Parsed::default();
    let mut tokens = Vec::new();
    let mut rest = args.iter().peekable();
    while let Some(arg) = rest.next() {
        if arg == "--" {
            parsed.positionals.extend(rest.cloned());
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (long, None),
            };
            let kind = by_name(name).map(|spec| spec.kind);
            let takes_value = kind == Some(Kind::Text)
                || (kind == Some(Kind::Bool) && rest.peek().is_some_and(|next| bool_word(next)));
            let (value, was_inline) = match inline {
                Some(value) => (Some(value), true),
                None if takes_value => (rest.next().cloned(), false),
                None => (None, false),
            };
            tokens.push(Token {
                name: name.to_owned(),
                raw: format!("--{name}"),
                value,
                inline: was_inline,
            });
            continue;
        }
        if arg.len() > 1 && arg.starts_with('-') {
            // One-letter options, alone or in a group; the last of a group may take the next argument.
            let letters: Vec<char> = arg.chars().skip(1).collect();
            for (at, letter) in letters.iter().enumerate() {
                let spec = by_short(*letter);
                let name = spec.map_or_else(|| letter.to_string(), |spec| spec.name.to_owned());
                let last = at + 1 == letters.len();
                let value = match spec {
                    Some(spec) if spec.kind == Kind::Text && last => rest.next().cloned(),
                    _ => None,
                };
                tokens.push(Token {
                    name,
                    raw: format!("-{letter}"),
                    value,
                    inline: false,
                });
            }
            continue;
        }
        parsed.positionals.push(arg.clone());
    }
    for token in tokens {
        if let Some(value) = token
            .value
            .as_deref()
            .filter(|value| !token.inline && option_like(value))
        {
            let _ = value;
            let example = if token.raw.starts_with("--") {
                format!("'{}=-XYZ'", token.raw)
            } else {
                format!("'--{}=-XYZ' or '{}-XYZ'", token.name, token.raw)
            };
            return Err(ArgError(format!(
                "Option '{raw}' argument is ambiguous.\nDid you forget to specify the option argument for '{raw}'?\nTo specify an option argument starting with a dash use {example}.",
                raw = token.raw
            )));
        }
        let Some(spec) = by_name(&token.name) else {
            return Err(ArgError(format!(
                "Unknown option '{raw}'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"{raw}\"",
                raw = token.raw
            )));
        };
        let both = match spec.short {
            Some(letter) => format!("-{letter}, --{}", spec.name),
            None => format!("--{}", spec.name),
        };
        match (spec.kind, token.value) {
            (Kind::Text, Some(value)) => {
                parsed.texts.insert(token.name, value);
            }
            (Kind::Text, None) => {
                return Err(ArgError(format!("Option '{both} <value>' argument missing")));
            }
            (Kind::Bool, value) => {
                parsed
                    .texts
                    .insert(token.name, value.unwrap_or_else(|| "true".to_owned()));
            }
            (Kind::Flag, Some(_)) => {
                return Err(ArgError(format!("Option '{both}' does not take an argument")));
            }
            (Kind::Flag, None) => {
                if !parsed.flags.contains(&token.name) {
                    parsed.flags.push(token.name);
                }
            }
        }
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPECS: [Spec; 4] = [
        Spec::text("to"),
        Spec::text("session"),
        Spec::flag("json"),
        Spec::flag("help").short('h'),
    ];

    fn parse(args: &[&str]) -> Result<Parsed, ArgError> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        parse_args(&args, &SPECS)
    }

    #[test]
    fn options_and_positionals() {
        let parsed = parse(&["one", "--to", "Spare", "--json", "two", "--session=abc"]).unwrap();
        assert_eq!(parsed.positionals, ["one", "two"]);
        assert_eq!(parsed.text("to"), Some("Spare"));
        assert_eq!(parsed.text("session"), Some("abc"));
        assert!(parsed.flag("json"));
        assert!(!parsed.flag("help"));
        assert_eq!(parsed.positional(1), Some("two"));
        assert_eq!(parsed.positional(2), None);
        assert_eq!(parse(&[]).unwrap(), Parsed::default());
    }

    #[test]
    fn the_last_value_wins_and_empty_values_are_values() {
        let parsed = parse(&["--to", "a", "--to=b", "--session="]).unwrap();
        assert_eq!(parsed.text("to"), Some("b"));
        assert_eq!(parsed.text("session"), Some(""));
    }

    #[test]
    fn after_two_dashes_everything_is_positional() {
        let parsed = parse(&["--json", "--", "--to", "-x", "name"]).unwrap();
        assert!(parsed.flag("json"));
        assert_eq!(parsed.positionals, ["--to", "-x", "name"]);
        assert_eq!(parse(&["-"]).unwrap().positionals, ["-"]);
    }

    #[test]
    fn one_letter_options() {
        assert!(parse(&["-h"]).unwrap().flag("help"));
        assert_eq!(
            parse(&["-x"]).unwrap_err().0,
            "Unknown option '-x'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"-x\""
        );
        assert!(parse(&["-hx"]).unwrap_err().0.starts_with("Unknown option '-x'"));
        assert!(parse(&["-5"]).unwrap_err().0.starts_with("Unknown option '-5'"));
    }

    #[test]
    fn what_cannot_be_read() {
        assert_eq!(
            parse(&["--nope"]).unwrap_err().0,
            "Unknown option '--nope'. To specify a positional argument starting with a '-', place it at the end of the command after '--', as in '-- \"--nope\""
        );
        assert_eq!(
            parse(&["--to"]).unwrap_err().0,
            "Option '--to <value>' argument missing"
        );
        assert_eq!(
            parse(&["--json=yes"]).unwrap_err().0,
            "Option '--json' does not take an argument"
        );
        assert_eq!(
            parse(&["--help=1"]).unwrap_err().0,
            "Option '-h, --help' does not take an argument"
        );
        assert_eq!(
            parse(&["--to", "--json"]).unwrap_err().0,
            "Option '--to' argument is ambiguous.\nDid you forget to specify the option argument for '--to'?\nTo specify an option argument starting with a dash use '--to=-XYZ'."
        );
        assert_eq!(parse(&["--to=--json"]).unwrap().text("to"), Some("--json"));
        assert_eq!(parse(&["--to", "-"]).unwrap().text("to"), Some("-"));
    }
}
