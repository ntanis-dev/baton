//! The menu's keys, by the names it shows and tests them under: "up", "enter", "escape", "ctrl+c",
//! "shift+tab", or the character typed ("q", "/", "J").
//!
//! The runtime turns what the terminal sends into a [`Key`]; text that arrives at once (a paste, or
//! fast typing) goes to the reducer as text instead.

use std::fmt;

/// One key press, or a command a click stands for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// The Up arrow.
    Up,
    /// The Down arrow.
    Down,
    /// The Left arrow.
    Left,
    /// The Right arrow.
    Right,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
    /// Enter.
    Enter,
    /// Escape.
    Escape,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Delete.
    Delete,
    /// A character typed.
    Char(char),
    /// A character typed with Ctrl held, in lower case (Ctrl+Space is a space).
    Ctrl(char),
    /// Anything else the menu knows only by its name: "shift+tab", "ctrl+right", or a command a
    /// click stands for ("add-account").
    Named(String),
}

/// Whether a character is a control character.
fn is_control(c: char) -> bool {
    matches!(c, '\u{00}'..='\u{1F}' | '\u{7F}'..='\u{9F}')
}

impl Key {
    /// The key of a name: "up", "ctrl+u", "q".
    pub fn from_name(name: &str) -> Key {
        match name {
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "home" => Key::Home,
            "end" => Key::End,
            "enter" => Key::Enter,
            "escape" => Key::Escape,
            "tab" => Key::Tab,
            "backspace" => Key::Backspace,
            "delete" => Key::Delete,
            "ctrl+space" => Key::Ctrl(' '),
            _ => {
                let mut chars = name.chars();
                if let (Some(only), None) = (chars.next(), chars.next()) {
                    return Key::Char(only);
                }
                if let Some(rest) = name.strip_prefix("ctrl+") {
                    let mut chars = rest.chars();
                    if let (Some(only), None) = (chars.next(), chars.next()) {
                        return Key::Ctrl(only);
                    }
                }
                Key::Named(name.to_owned())
            }
        }
    }

    /// The key's name.
    pub fn name(&self) -> String {
        match self {
            Key::Up => "up".to_owned(),
            Key::Down => "down".to_owned(),
            Key::Left => "left".to_owned(),
            Key::Right => "right".to_owned(),
            Key::PageUp => "pageup".to_owned(),
            Key::PageDown => "pagedown".to_owned(),
            Key::Home => "home".to_owned(),
            Key::End => "end".to_owned(),
            Key::Enter => "enter".to_owned(),
            Key::Escape => "escape".to_owned(),
            Key::Tab => "tab".to_owned(),
            Key::Backspace => "backspace".to_owned(),
            Key::Delete => "delete".to_owned(),
            Key::Char(c) => c.to_string(),
            Key::Ctrl(' ') => "ctrl+space".to_owned(),
            Key::Ctrl(c) => format!("ctrl+{c}"),
            Key::Named(name) => name.clone(),
        }
    }

    /// The character typed, when the key is one that prints.
    pub fn printable(&self) -> Option<char> {
        match self {
            Key::Char(c) if !is_control(*c) => Some(*c),
            _ => None,
        }
    }

    /// Whether the key is this character.
    pub fn is(&self, c: char) -> bool {
        *self == Key::Char(c)
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// Whether a key is a character that prints.
pub fn printable(key: &Key) -> bool {
    key.printable().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for name in [
            "up",
            "down",
            "left",
            "right",
            "pageup",
            "pagedown",
            "home",
            "end",
            "enter",
            "escape",
            "tab",
            "backspace",
            "delete",
            "ctrl+c",
            "ctrl+space",
            "shift+tab",
            "ctrl+right",
            "add-account",
            "q",
            "/",
            "J",
            " ",
        ] {
            assert_eq!(Key::from_name(name).name(), name);
        }
        assert_eq!(Key::from_name("shift+up"), Key::Named("shift+up".to_owned()));
        assert_eq!(Key::from_name("ctrl+u"), Key::Ctrl('u'));
        assert_eq!(Key::from_name("J"), Key::Char('J'));
        assert_eq!(Key::from_name("é"), Key::Char('é'));
    }

    #[test]
    fn what_prints() {
        assert!(printable(&Key::Char('a')));
        assert!(printable(&Key::Char(' ')));
        assert!(!printable(&Key::Char('\u{7}')));
        assert!(!printable(&Key::Enter));
        assert!(!printable(&Key::Ctrl('u')));
        assert!(!printable(&Key::Named("add-account".to_owned())));
        assert!(Key::Char('q').is('q'));
    }
}
