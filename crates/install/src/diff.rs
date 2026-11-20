//! A small line diff (longest common subsequence) for showing what `baton install` changes. Files here
//! are small (settings.json, one command file).

/// The lines of a text, without the empty one a final line break leaves; a text that is not there
/// has none.
fn lines(text: Option<&str>) -> Vec<String> {
    let Some(text) = text else {
        return Vec::new();
    };
    let mut lines: Vec<String> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(str::to_owned)
        .collect();
    if lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// The changed lines between two texts (None: the file is not there), each with `context` unchanged
/// lines around it: `+ added`, `- removed`, `  unchanged`, and `  ...` where lines were left out.
/// Nothing when the texts hold the same lines.
pub fn diff_lines(before: Option<&str>, after: Option<&str>, context: usize) -> Vec<String> {
    let a = lines(before);
    let b = lines(after);
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', &a[i]));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            ops.push(('+', &b[j]));
            j += 1;
        } else {
            ops.push(('-', &a[i]));
            i += 1;
        }
    }
    let mut keep = vec![false; ops.len()];
    let mut any = false;
    for k in 0..ops.len() {
        if ops[k].0 == ' ' {
            continue;
        }
        any = true;
        let from = k.saturating_sub(context);
        let to = (k + context).min(ops.len() - 1);
        for kept in &mut keep[from..=to] {
            *kept = true;
        }
    }
    if !any {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut last: Option<usize> = None;
    for (k, (kind, text)) in ops.iter().enumerate() {
        if !keep[k] {
            continue;
        }
        if last.is_some_and(|last| k > last + 1) {
            out.push("  ...".to_owned());
        }
        out.push(format!("{kind} {text}"));
        last = Some(k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_lines_are_no_diff() {
        assert!(diff_lines(Some("a\nb\n"), Some("a\nb\n"), 2).is_empty());
        assert!(
            diff_lines(Some("a\r\nb"), Some("a\nb\n"), 2).is_empty(),
            "line ends are not a change"
        );
        assert!(diff_lines(None, None, 2).is_empty());
        assert!(diff_lines(None, Some(""), 2).is_empty());
    }

    #[test]
    fn a_new_file_is_all_added_and_a_removed_one_all_removed() {
        assert_eq!(diff_lines(None, Some("one\ntwo\n"), 2), ["+ one", "+ two"]);
        assert_eq!(diff_lines(Some("one\ntwo\n"), None, 2), ["- one", "- two"]);
        assert_eq!(diff_lines(Some("\n"), Some("x\n"), 2), ["+ x", "- "]);
    }

    #[test]
    fn changes_keep_their_context_and_gaps_are_marked() {
        let before = "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n";
        let after = "1\n2\nthree\n4\n5\n6\n7\n8\n9\nten\n";
        assert_eq!(
            diff_lines(Some(before), Some(after), 2),
            [
                "  1", "  2", "+ three", "- 3", "  4", "  5", "  ...", "  8", "  9", "+ ten", "- 10"
            ]
        );
        assert_eq!(
            diff_lines(Some(before), Some(after), 0),
            ["+ three", "- 3", "  ...", "+ ten", "- 10"]
        );
        assert_eq!(
            diff_lines(Some("a\nb\nc\n"), Some("a\nc\n"), 1),
            ["  a", "- b", "  c"],
            "a removed line between two that stay"
        );
    }
}
