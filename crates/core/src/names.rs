//! Account names are labels only: folders, files, hook arguments and environment variables use the
//! account's stable id, never its name. So a name may be any printable text the operator likes (an
//! email, words with spaces, any script): trimmed, not empty, at most [`NAME_MAX`] characters, no
//! control characters, line breaks or bidirectional overrides, unique ignoring case.

use unicode_normalization::UnicodeNormalization;

/// The longest a name may be, in characters.
pub const NAME_MAX: usize = 120;

/// The white space trimmed from the ends of a name.
fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{000B}' | '\u{000C}' | '\r' | ' ' | '\u{00A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// What a name may not hold: control characters, line and paragraph separators, bidirectional
/// overrides and isolates.
fn is_forbidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A name as it is kept: composed (NFC) and trimmed.
pub fn clean_name(value: &str) -> String {
    let composed: String = value.nfc().collect();
    composed.trim_matches(is_space).to_owned()
}

/// Why a name cannot be used, or None when it can.
pub fn name_problem(value: &str) -> Option<String> {
    let name = clean_name(value);
    if name.is_empty() {
        return Some("A name cannot be empty.".to_owned());
    }
    if name.chars().any(is_forbidden) {
        return Some("A name cannot hold control characters or line breaks.".to_owned());
    }
    if name.chars().count() > NAME_MAX {
        return Some(format!("A name is at most {NAME_MAX} characters."));
    }
    None
}

/// Whether a name can be used.
pub fn valid_name(value: &str) -> bool {
    name_problem(value).is_none()
}

/// A name for comparing: cleaned and in lower case.
fn fold(value: &str) -> String {
    clean_name(value).to_lowercase()
}

/// Whether two names are the same, ignoring case.
pub fn same_name(a: &str, b: &str) -> bool {
    fold(a) == fold(b)
}

/// The id a new account gets from its name: lower case letters, digits, `-` and `_` (anything else
/// becomes `-`), at most 40 characters, `account` when nothing is left; the config makes it unique.
pub fn id_from_name(name: &str) -> String {
    let mut base = String::new();
    let mut gap = false;
    for c in fold(name)
        .nfkd()
        .filter(|c| !('\u{0300}'..='\u{036F}').contains(c))
    {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
            if gap {
                base.push('-');
                gap = false;
            }
            base.push(c);
        } else {
            gap = true;
        }
    }
    if gap {
        base.push('-');
    }
    let edge = |c: char| c == '-' || c == '_';
    let trimmed = base.trim_matches(edge);
    let cut: String = trimmed.chars().take(40).collect();
    let id = cut.trim_end_matches(edge);
    if id.is_empty() {
        "account".to_owned()
    } else {
        id.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Work account \n"), "Work account");
        assert_eq!(clean_name("\u{FEFF}x\u{00A0}"), "x");
        // Composed: e + combining acute is one character.
        assert_eq!(clean_name("e\u{0301}"), "\u{00E9}");
    }

    #[test]
    fn what_a_name_may_be() {
        assert_eq!(name_problem("jane@example.com"), None);
        assert_eq!(name_problem("Δουλειά 2"), None);
        assert_eq!(name_problem("   ").as_deref(), Some("A name cannot be empty."));
        assert_eq!(
            name_problem("a\u{0007}b").as_deref(),
            Some("A name cannot hold control characters or line breaks.")
        );
        assert!(name_problem("a\nb").is_some());
        assert!(name_problem("a\u{202E}b").is_some());
        assert!(name_problem("a\u{2028}b").is_some());
        assert!(valid_name(&"x".repeat(120)));
        assert_eq!(
            name_problem(&"x".repeat(121)).as_deref(),
            Some("A name is at most 120 characters.")
        );
    }

    #[test]
    fn names_compare_ignoring_case() {
        assert!(same_name("Work", " work "));
        assert!(same_name("ΔΟΥΛΕΙΑ", "δουλεια"));
        assert!(!same_name("Work", "Work 2"));
    }

    #[test]
    fn ids_from_names() {
        assert_eq!(id_from_name("jane@example.com"), "jane-example-com");
        assert_eq!(id_from_name("Work account"), "work-account");
        assert_eq!(id_from_name("  --Spare__ "), "spare");
        assert_eq!(id_from_name("Café Émile"), "cafe-emile");
        assert_eq!(id_from_name("日本語"), "account");
        assert_eq!(id_from_name("a_b-c"), "a_b-c");
        assert_eq!(id_from_name(&"abc ".repeat(30)).len(), 39);
        assert_eq!(id_from_name(&"x".repeat(60)), "x".repeat(40));
        assert_eq!(id_from_name("!!!"), "account");
    }
}
