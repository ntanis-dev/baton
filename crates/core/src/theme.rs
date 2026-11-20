//! The palette, glyph sets and style words of the menu: the plain commands and the status line draw
//! with them too.
//!
//! One palette of roles (it keeps bold and inverse and drops every colour under `NO_COLOR`), and two
//! glyph sets: "rich" for terminals with modern fonts (Windows Terminal, VS Code, mintty, macOS and
//! Linux terminals), "plain" for the classic Windows console host. `BATON_GLYPHS=rich|plain` overrides
//! the guess.

use crate::env::Env;

/// The colour of a role, as `#rrggbb`.
pub fn role(name: &str) -> Option<&'static str> {
    Some(match name {
        "accent" | "baton" | "warn" => "#d7ac78",
        "brand" => "#e4bc89",
        "ok" => "#92ae99",
        "err" => "#cf8b82",
        "info" | "hot" => "#b2b7bf",
        "muted" => "#7e858e",
        "divider" => "#343a43",
        "surface" => "#181b20",
        "background" => "#121519",
        "text" => "#d6d8dc",
        _ => return None,
    })
}

/// The colours a style may name directly.
const COLOURS: [&str; 17] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "gray",
    "grey",
    "redBright",
    "greenBright",
    "yellowBright",
    "blueBright",
    "magentaBright",
    "cyanBright",
    "whiteBright",
];

/// The corners and edges of a box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Corners {
    /// Top left.
    pub tl: &'static str,
    /// Top right.
    pub tr: &'static str,
    /// Bottom left.
    pub bl: &'static str,
    /// Bottom right.
    pub br: &'static str,
    /// A horizontal edge.
    pub h: &'static str,
    /// A vertical edge.
    pub v: &'static str,
}

/// The characters a terminal draws with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyphs {
    /// The frames of a spinner.
    pub spinner: &'static [&'static str],
    /// Sparkline levels, low to high.
    pub ticks: &'static [&'static str],
    /// Bar chart levels, low to high.
    pub bars: &'static [&'static str],
    /// The filled part of a usage bar.
    pub full: &'static str,
    /// The empty part of a usage bar.
    pub empty: &'static str,
    /// The threshold on a usage bar.
    pub limit: &'static str,
    /// A filled dot.
    pub dot: &'static str,
    /// An empty dot.
    pub ring: &'static str,
    /// A diamond.
    pub diamond: &'static str,
    /// Done.
    pub check: &'static str,
    /// Failed.
    pub cross: &'static str,
    /// Blocked.
    pub blocked: &'static str,
    /// The preferred mark.
    pub preferred: &'static str,
    /// The normal mark.
    pub normal: &'static str,
    /// The last-resort mark.
    pub last_resort: &'static str,
    /// The Baton mark.
    pub baton: &'static str,
    /// A link between two things.
    pub link: &'static str,
    /// A bullet.
    pub bullet: &'static str,
    /// An arrow.
    pub arrow: &'static str,
    /// The Up key.
    pub up: &'static str,
    /// The Down key.
    pub down: &'static str,
    /// The Left key.
    pub left: &'static str,
    /// The Right key.
    pub right: &'static str,
    /// The Enter key.
    pub enter: &'static str,
    /// A separator.
    pub sep: &'static str,
    /// An open group.
    pub open: &'static str,
    /// A closed group.
    pub closed: &'static str,
    /// More.
    pub more: &'static str,
    /// Less.
    pub less: &'static str,
    /// The border's style: `round` or `single`.
    pub border: &'static str,
    /// The border's characters.
    pub corners: Corners,
}

/// The glyphs of terminals with modern fonts.
pub const RICH: Glyphs = Glyphs {
    spinner: &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"],
    ticks: &["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"],
    bars: &["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"],
    full: "━",
    empty: "─",
    limit: "┃",
    dot: "●",
    ring: "○",
    diamond: "◆",
    check: "✓",
    cross: "✗",
    blocked: "⊘",
    preferred: "★",
    normal: "·",
    last_resort: "▿",
    baton: "╺━╸",
    link: "━",
    bullet: "•",
    arrow: "›",
    up: "↑",
    down: "↓",
    left: "←",
    right: "→",
    enter: "⏎",
    sep: "·",
    open: "▾",
    closed: "▸",
    more: "+",
    less: "-",
    border: "round",
    corners: Corners {
        tl: "╭",
        tr: "╮",
        bl: "╰",
        br: "╯",
        h: "─",
        v: "│",
    },
};

/// The glyphs of the classic Windows console host.
pub const PLAIN: Glyphs = Glyphs {
    spinner: &["|", "/", "-", "\\"],
    ticks: &["_", ".", ":", "-", "=", "+", "*", "#"],
    bars: &["▄", "█"],
    full: "█",
    empty: "░",
    limit: "|",
    dot: "*",
    ring: "o",
    diamond: "?",
    check: "+",
    cross: "x",
    blocked: "x",
    preferred: "*",
    normal: ".",
    last_resort: "v",
    baton: "=#=",
    link: "-",
    bullet: "*",
    arrow: ">",
    up: "↑",
    down: "↓",
    left: "←",
    right: "→",
    enter: "Enter",
    sep: "·",
    open: "v",
    closed: ">",
    more: "+",
    less: "-",
    border: "single",
    corners: Corners {
        tl: "┌",
        tr: "┐",
        bl: "└",
        br: "┘",
        h: "─",
        v: "│",
    },
};

/// Which glyph set a terminal gets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GlyphSet {
    /// Modern fonts.
    #[default]
    Rich,
    /// The classic Windows console host.
    Plain,
}

impl GlyphSet {
    /// The set's name: `rich`, `plain`.
    pub fn as_str(self) -> &'static str {
        match self {
            GlyphSet::Rich => "rich",
            GlyphSet::Plain => "plain",
        }
    }

    /// The set of a name; anything else is the rich one.
    pub fn parse(name: &str) -> GlyphSet {
        if name == "plain" {
            GlyphSet::Plain
        } else {
            GlyphSet::Rich
        }
    }

    /// The set's glyphs.
    pub fn glyphs(self) -> &'static Glyphs {
        match self {
            GlyphSet::Rich => &RICH,
            GlyphSet::Plain => &PLAIN,
        }
    }
}

/// The glyph set for this environment on this system.
pub fn pick_glyphs(env: &Env) -> GlyphSet {
    pick_glyphs_on(env, cfg!(windows))
}

/// The glyph set for an environment, on Windows or not.
pub fn pick_glyphs_on(env: &Env, windows: bool) -> GlyphSet {
    match env
        .get("BATON_GLYPHS")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "rich" => return GlyphSet::Rich,
        "plain" => return GlyphSet::Plain,
        _ => {}
    }
    if !windows {
        return GlyphSet::Rich;
    }
    let term = env.get("TERM").unwrap_or_default().to_lowercase();
    let modern = ["xterm", "mintty", "alacritty", "wezterm", "kitty"]
        .iter()
        .any(|name| term.contains(name));
    if env.has("WT_SESSION") || env.has("TERM_PROGRAM") || env.get("ConEmuANSI") == Some("ON") || modern {
        return GlyphSet::Rich;
    }
    GlyphSet::Plain
}

/// How a span is drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextProps {
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Underlined.
    pub underline: bool,
    /// Foreground and background swapped.
    pub inverse: bool,
    /// Dimmed.
    pub dim: bool,
    /// The foreground: a colour's name or `#rrggbb`.
    pub color: Option<String>,
    /// The background: a colour's name or `#rrggbb`.
    pub background: Option<String>,
}

/// Whether a word is `#rrggbb`.
fn is_hex(word: &str) -> bool {
    word.len() == 7 && word.starts_with('#') && word[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// What the tone of a style is so far.
#[derive(PartialEq)]
enum Tone {
    /// None named.
    None,
    /// A colour.
    Colour(String),
    /// A selected part: it keeps its own colours whatever else is named.
    Selected,
}

/// The drawing of a style's words, with or without colour.
pub fn text_props(style: &str, color: bool) -> TextProps {
    let mut props = TextProps::default();
    let mut tone = Tone::None;
    let mut dim = false;
    for word in style.split(' ').filter(|word| !word.is_empty()) {
        match word {
            "bold" => props.bold = true,
            "italic" => props.italic = true,
            "underline" => props.underline = true,
            "inverse" => props.inverse = true,
            "label" => {
                if color {
                    props.background = Some("#20252c".to_owned());
                } else {
                    props.italic = true;
                }
                tone = Tone::Colour(role("muted").unwrap_or_default().to_owned());
            }
            "dim" => dim = true,
            "selected" => {
                // A solid accent bar, bright bold text; NO_COLOR: inverse and bold.
                if color {
                    props.color = Some("#f0d5b3".to_owned());
                    props.background = Some("#35302a".to_owned());
                } else {
                    props.inverse = true;
                }
                props.bold = true;
                tone = Tone::Selected;
            }
            _ => {
                if let Some(on) = word.strip_prefix("on-").and_then(role) {
                    if color {
                        props.background = Some(on.to_owned());
                        props.color = Some(if word == "on-err" { "whiteBright" } else { "black" }.to_owned());
                    } else {
                        props.inverse = true;
                    }
                    tone = Tone::Selected;
                } else if tone != Tone::Selected {
                    if let Some(colour) = role(word) {
                        tone = Tone::Colour(colour.to_owned());
                    } else if COLOURS.contains(&word) || is_hex(word) {
                        tone = Tone::Colour(word.to_owned());
                    }
                }
            }
        }
    }
    if color && let Tone::Colour(colour) = &tone {
        props.color = Some(colour.clone());
    }
    // A selected part is never dim (NO_COLOR: inverse and bold only).
    if dim && tone != Tone::Selected {
        if color && props.color.is_none() {
            props.color = role("muted").map(str::to_owned);
        } else if props.background.is_none() {
            props.dim = true;
        }
    }
    props
}

/// A look: colour or not, and a glyph set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    /// Whether colours are drawn.
    pub color: bool,
    /// Which glyph set.
    pub glyphs: GlyphSet,
    /// The glyphs themselves.
    pub g: &'static Glyphs,
}

impl Default for Theme {
    fn default() -> Theme {
        make_theme(true, GlyphSet::Rich)
    }
}

impl Theme {
    /// How a style is drawn in this look.
    pub fn props(&self, style: &str) -> TextProps {
        text_props(style, self.color)
    }

    /// The colour of a role in this look; None without colour.
    pub fn tone(&self, name: &str) -> Option<String> {
        self.color.then(|| role(name).unwrap_or(name).to_owned())
    }
}

/// A look.
pub fn make_theme(color: bool, glyphs: GlyphSet) -> Theme {
    Theme {
        color,
        glyphs,
        g: glyphs.glyphs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_glyphs() {
        let env = |pairs: &[(&str, &str)]| Env::from_pairs(pairs.iter().map(|(k, v)| (*k, *v)));
        assert_eq!(pick_glyphs_on(&env(&[]), false), GlyphSet::Rich);
        assert_eq!(pick_glyphs_on(&env(&[]), true), GlyphSet::Plain);
        assert_eq!(pick_glyphs_on(&env(&[("WT_SESSION", "x")]), true), GlyphSet::Rich);
        assert_eq!(
            pick_glyphs_on(&env(&[("TERM", "xterm-256color")]), true),
            GlyphSet::Rich
        );
        assert_eq!(
            pick_glyphs_on(&env(&[("ConEmuANSI", "ON")]), true),
            GlyphSet::Rich
        );
        assert_eq!(
            pick_glyphs_on(&env(&[("BATON_GLYPHS", "PLAIN")]), false),
            GlyphSet::Plain
        );
        assert_eq!(
            pick_glyphs_on(&env(&[("BATON_GLYPHS", "rich")]), true),
            GlyphSet::Rich
        );
        assert_eq!(GlyphSet::parse("plain").glyphs().baton, "=#=");
        assert_eq!(GlyphSet::parse("anything").glyphs().baton, "╺━╸");
    }

    #[test]
    fn style_words() {
        let bold = text_props("ok bold", true);
        assert!(bold.bold);
        assert_eq!(bold.color.as_deref(), Some("#92ae99"));
        assert_eq!(
            text_props("ok bold", false),
            TextProps {
                bold: true,
                ..TextProps::default()
            }
        );
        assert_eq!(text_props("red", true).color.as_deref(), Some("red"));
        assert_eq!(
            text_props("#abcdef underline", true).color.as_deref(),
            Some("#abcdef")
        );
        assert_eq!(text_props("nonsense", true), TextProps::default());
        // Dim: the muted colour, or the terminal's own dim when a colour is set or there is none.
        assert_eq!(text_props("dim", true).color.as_deref(), Some("#7e858e"));
        let dim_ok = text_props("ok dim", true);
        assert!(dim_ok.dim && dim_ok.color.as_deref() == Some("#92ae99"));
        assert!(text_props("dim", false).dim);
        // Selected keeps its own colours and is never dim.
        let selected = text_props("selected err dim", true);
        assert_eq!(selected.color.as_deref(), Some("#f0d5b3"));
        assert_eq!(selected.background.as_deref(), Some("#35302a"));
        assert!(selected.bold && !selected.dim);
        let plain = text_props("selected", false);
        assert!(plain.inverse && plain.bold && plain.color.is_none());
        let on = text_props("on-err", true);
        assert_eq!(
            (on.background.as_deref(), on.color.as_deref()),
            (Some("#cf8b82"), Some("whiteBright"))
        );
        assert_eq!(text_props("on-ok", true).color.as_deref(), Some("black"));
        assert!(text_props("on-ok", false).inverse);
        let label = text_props("label", true);
        assert_eq!(
            (label.background.as_deref(), label.color.as_deref()),
            (Some("#20252c"), Some("#7e858e"))
        );
        assert!(text_props("label", false).italic);
    }

    #[test]
    fn a_theme() {
        let theme = make_theme(false, GlyphSet::Plain);
        assert_eq!(theme.g.check, "+");
        assert_eq!(theme.tone("ok"), None);
        assert_eq!(Theme::default().tone("ok").as_deref(), Some("#92ae99"));
        assert_eq!(Theme::default().tone("pink").as_deref(), Some("pink"));
    }
}
