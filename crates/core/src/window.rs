//! The two usage windows of a Claude plan, and the limit a refusal is about.

use std::fmt;
use std::ops::{Index, IndexMut};

use serde::{Deserialize, Serialize};

/// A usage window of a plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Window {
    /// The 5-hour window (Claude Code calls it the session limit).
    #[serde(rename = "five_hour")]
    FiveHour,
    /// The weekly window.
    #[serde(rename = "seven_day")]
    SevenDay,
}

/// Both windows, the short one first.
pub const WINDOWS: [Window; 2] = [Window::FiveHour, Window::SevenDay];

impl Window {
    /// The window's key in Baton's files and in Claude Code's status line: `five_hour`, `seven_day`.
    pub fn as_str(self) -> &'static str {
        match self {
            Window::FiveHour => "five_hour",
            Window::SevenDay => "seven_day",
        }
    }

    /// The window of a key.
    pub fn parse(key: &str) -> Option<Window> {
        WINDOWS.into_iter().find(|w| w.as_str() == key)
    }

    /// How long the window is, in milliseconds.
    pub fn length(self) -> i64 {
        match self {
            Window::FiveHour => 5 * crate::clock::HOUR,
            Window::SevenDay => 168 * crate::clock::HOUR,
        }
    }

    /// The window in words: `5-hour`, `weekly`.
    pub fn label(self) -> &'static str {
        Limit::from(self).label()
    }
}

impl fmt::Display for Window {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a refusal is about: one of the windows, or a limit Baton could not tie to a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Limit {
    /// The 5-hour window.
    #[serde(rename = "five_hour")]
    FiveHour,
    /// The weekly window.
    #[serde(rename = "seven_day")]
    SevenDay,
    /// A refusal with no window named.
    #[serde(rename = "unknown")]
    Unknown,
}

/// Every limit a refusal can name.
pub const LIMITS: [Limit; 3] = [Limit::FiveHour, Limit::SevenDay, Limit::Unknown];

impl Limit {
    /// The limit's key in Baton's files: `five_hour`, `seven_day`, `unknown`.
    pub fn as_str(self) -> &'static str {
        match self {
            Limit::FiveHour => "five_hour",
            Limit::SevenDay => "seven_day",
            Limit::Unknown => "unknown",
        }
    }

    /// The limit of a key; anything else is [`Limit::Unknown`].
    pub fn parse(key: &str) -> Limit {
        LIMITS
            .into_iter()
            .find(|l| l.as_str() == key)
            .unwrap_or(Limit::Unknown)
    }

    /// The window, when the limit is one.
    pub fn window(self) -> Option<Window> {
        match self {
            Limit::FiveHour => Some(Window::FiveHour),
            Limit::SevenDay => Some(Window::SevenDay),
            Limit::Unknown => None,
        }
    }

    /// The limit in words: `5-hour`, `weekly`, `usage`.
    pub fn label(self) -> &'static str {
        match self {
            Limit::FiveHour => "5-hour",
            Limit::SevenDay => "weekly",
            Limit::Unknown => "usage",
        }
    }
}

impl From<Window> for Limit {
    fn from(window: Window) -> Limit {
        match window {
            Window::FiveHour => Limit::FiveHour,
            Window::SevenDay => Limit::SevenDay,
        }
    }
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One value per window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerWindow<T> {
    /// The 5-hour window's.
    pub five_hour: T,
    /// The weekly window's.
    pub seven_day: T,
}

impl<T> PerWindow<T> {
    /// A value per window, made by `make`.
    pub fn from_fn(mut make: impl FnMut(Window) -> T) -> PerWindow<T> {
        PerWindow {
            five_hour: make(Window::FiveHour),
            seven_day: make(Window::SevenDay),
        }
    }

    /// The values with their windows, the 5-hour one first.
    pub fn iter(&self) -> impl Iterator<Item = (Window, &T)> {
        WINDOWS.into_iter().map(move |w| (w, &self[w]))
    }

    /// Each value changed by `change`.
    pub fn map<U>(&self, mut change: impl FnMut(Window, &T) -> U) -> PerWindow<U> {
        PerWindow::from_fn(|w| change(w, &self[w]))
    }
}

impl<T> Index<Window> for PerWindow<T> {
    type Output = T;

    fn index(&self, window: Window) -> &T {
        match window {
            Window::FiveHour => &self.five_hour,
            Window::SevenDay => &self.seven_day,
        }
    }
}

impl<T> IndexMut<Window> for PerWindow<T> {
    fn index_mut(&mut self, window: Window) -> &mut T {
        match window {
            Window::FiveHour => &mut self.five_hour,
            Window::SevenDay => &mut self.seven_day,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_words() {
        assert_eq!(Window::FiveHour.as_str(), "five_hour");
        assert_eq!(Window::parse("seven_day"), Some(Window::SevenDay));
        assert_eq!(Window::parse("unknown"), None);
        assert_eq!(Limit::parse("nonsense"), Limit::Unknown);
        assert_eq!(Limit::from(Window::SevenDay).label(), "weekly");
        assert_eq!(Limit::Unknown.label(), "usage");
        assert_eq!(Limit::FiveHour.window(), Some(Window::FiveHour));
        assert_eq!(serde_json::to_string(&Window::FiveHour).unwrap(), "\"five_hour\"");
        assert_eq!(
            serde_json::to_string(&vec![Limit::SevenDay, Limit::Unknown]).unwrap(),
            "[\"seven_day\",\"unknown\"]"
        );
    }

    #[test]
    fn one_value_per_window() {
        let mut each = PerWindow::from_fn(|w| w.length() / crate::clock::HOUR);
        assert_eq!((each.five_hour, each.seven_day), (5, 168));
        each[Window::FiveHour] = 6;
        assert_eq!(
            each.map(|_, hours| hours * 2),
            PerWindow {
                five_hour: 12,
                seven_day: 336
            }
        );
        assert_eq!(each.iter().map(|(w, _)| w).collect::<Vec<_>>(), WINDOWS);
    }
}
