//! One ASCII mark across the splash, workspace, size error and login.

use crate::lines::{Line, p, s};

/// The wordmark.
pub const LOGO: [&str; 5] = [
    "    ____  ___  __________  _   __",
    "   / __ )/   |/_  __/ __ \\/ | / /",
    "  / __  / /| | / / / / / /  |/ /",
    " / /_/ / ___ |/ / / /_/ / /|  /",
    "/_____/_/  |_/_/  \\____/_/ |_/",
];

/// What Baton is, in two lines.
pub const MOTTO: [&str; 2] = ["Your Claude accounts.", "One continuous conversation."];

/// What Baton is for, in one.
pub const SLOGAN: &str = "Keep the conversation moving.";

/// The wordmark as lines.
pub fn wordmark() -> Vec<Line> {
    LOGO.iter().map(|line| vec![s(line, "brand bold")]).collect()
}

/// The art of the splash screen.
pub fn splash_art() -> Vec<Line> {
    wordmark()
}

/// The slogan and the motto, each line after `indent`.
pub fn caption_lines(indent: &str) -> Vec<Line> {
    let mut out = vec![vec![s(format!("{indent}{SLOGAN}"), "accent bold")]];
    out.extend(
        MOTTO
            .iter()
            .map(|line| vec![s(format!("{indent}{line}"), "muted")]),
    );
    out
}

/// The wordmark, a blank line and the caption, each line after `indent`.
pub fn branding(indent: &str) -> Vec<Line> {
    let mut out: Vec<Line> = wordmark()
        .into_iter()
        .map(|line| {
            let mut indented = vec![p(indent)];
            indented.extend(line);
            indented
        })
        .collect();
    out.push(Vec::new());
    out.extend(caption_lines(indent));
    out
}

/// A baton, handed on.
pub fn relay_art() -> Vec<Line> {
    [
        "        .----------------------.",
        "   ----/  / / / / / / / / /   /----",
        "      '----------------------'",
    ]
    .iter()
    .map(|line| vec![s(line, "brand")])
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::plain;

    #[test]
    fn the_mark() {
        assert_eq!(wordmark().len(), 5);
        let lines = branding("  ");
        assert_eq!(lines.len(), 9);
        assert_eq!(plain(&lines[0]), format!("  {}", LOGO[0]));
        assert!(lines[5].is_empty());
        assert_eq!(plain(&lines[6]), "  Keep the conversation moving.");
        assert_eq!(plain(&lines[8]), "  One continuous conversation.");
        assert_eq!(relay_art().len(), 3);
        assert_eq!(splash_art(), wordmark());
    }
}
