//! Fuzzy matching for the list filter (/): the characters of a term appear in order, in any case. A
//! whole substring beats scattered letters; matches at word starts and in runs score higher. Each
//! space-separated term must match one of the fields.

/// A match of a query in a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// How good the match is; higher is better.
    pub score: i64,
    /// Where each character of the query was found, by character.
    pub positions: Vec<usize>,
}

/// Whether a character separates words.
fn is_separator(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '-' | '_' | '.' | '/' | '\\' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '@' | '#' | ','
        )
}

/// Whether a word starts at character `i` of `text`.
fn boundary(text: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let Some(before) = text.get(i - 1) else {
        return false;
    };
    is_separator(*before)
        || (before.is_ascii_lowercase() && text.get(i).is_some_and(char::is_ascii_uppercase))
}

/// A text's characters in lower case, one for one.
fn lower(text: &str) -> Vec<char> {
    text.chars()
        .map(|c| c.to_lowercase().next().unwrap_or(c))
        .collect()
}

/// Where `needle` first occurs in `hay` at or after `from`.
fn find(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|at| hay[*at..*at + needle.len()] == *needle)
}

/// The match of `query` in `text`, or None.
pub fn fuzzy(query: &str, text: &str) -> Option<Match> {
    let q = lower(query);
    let raw: Vec<char> = text.chars().collect();
    let t = lower(text);
    if q.is_empty() {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    }
    if let Some(at) = find(&t, &q, 0) {
        // The earliest word-start occurrence, if there is one.
        let mut best = at;
        let mut next = Some(at);
        while let Some(i) = next {
            if boundary(&raw, i) {
                best = i;
                break;
            }
            next = find(&t, &q, i + 1);
        }
        let score =
            200 + q.len() as i64 * 10 - best.min(50) as i64 + if boundary(&raw, best) { 60 } else { 0 };
        return Some(Match {
            score,
            positions: (best..best + q.len()).collect(),
        });
    }
    let mut positions = Vec::new();
    let mut score = 0i64;
    let mut from = 0usize;
    let mut last: Option<usize> = None;
    for ch in q {
        // Prefer the next word start that has this letter, close by.
        let near = (from..t.len().min(from + 24)).find(|i| t[*i] == ch && boundary(&raw, *i));
        let found = near.or_else(|| (from..t.len()).find(|i| t[*i] == ch))?;
        score += 10
            + if boundary(&raw, found) { 15 } else { 0 }
            + if last.is_some_and(|last| found == last + 1) {
                12
            } else {
                0
            }
            - (found - from).min(10) as i64;
        positions.push(found);
        last = Some(found);
        from = found + 1;
    }
    Some(Match { score, positions })
}

/// The best total score of every term over the fields, or None when a term matches none of them.
pub fn match_fields(query: &str, fields: &[&str]) -> Option<i64> {
    let mut total = 0;
    for term in query.split_whitespace() {
        let best = fields
            .iter()
            .filter(|field| !field.is_empty())
            .filter_map(|field| fuzzy(term, field))
            .map(|found| found.score)
            .max()?;
        total += best;
    }
    Some(total)
}

/// Whether every term of the query matches one of the fields.
pub fn fuzzy_matches(query: &str, fields: &[&str]) -> bool {
    match_fields(query, fields).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substrings_and_scattered_letters() {
        assert_eq!(
            fuzzy("", "anything"),
            Some(Match {
                score: 0,
                positions: vec![]
            })
        );
        assert_eq!(
            fuzzy("acct3", "acct3"),
            Some(Match {
                score: 310,
                positions: vec![0, 1, 2, 3, 4]
            })
        );
        // A word start further on beats an earlier match inside a word.
        assert_eq!(fuzzy("ab", "xab ab").map(|m| m.positions), Some(vec![4, 5]));
        assert_eq!(fuzzy("ab", "xab").map(|m| m.score), Some(219));
        assert_eq!(fuzzy("so", "sports-odds").map(|m| m.positions), Some(vec![0, 7]));
        assert_eq!(fuzzy("wa", "Work Account").map(|m| m.positions), Some(vec![0, 5]));
        assert_eq!(fuzzy("zzz", "acct3"), None);
        assert_eq!(fuzzy("ACC", "my acct").map(|m| m.positions), Some(vec![3, 4, 5]));
    }

    #[test]
    fn every_term_must_match_a_field() {
        assert_eq!(match_fields("", &["x"]), Some(0));
        assert_eq!(match_fields("   ", &["x"]), Some(0));
        assert!(fuzzy_matches("acct pref", &["acct2", "preferred"]));
        assert!(!fuzzy_matches("acct zzz", &["acct2", "preferred"]));
        assert!(!fuzzy_matches("a", &["", ""]));
        assert_eq!(match_fields("acct3", &["acct3", "normal"]), Some(310));
    }
}
