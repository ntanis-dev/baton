//! A three-way merge of text by lines, as a version control system does it: two texts that were both
//! made from one base become one text that has what each of them changed. Where both changed the same
//! lines in different ways there is no such text, and only a person can say what stays.

/// The largest table (lines of the base that changed, times lines of the other text that changed) the
/// longest common run of lines is worked out over; two texts that differ by more are not merged.
const MAX_CELLS: usize = 4_000_000;

/// The lines of a text, each with its own ending.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// For each line of `base`, the line of `other` it is in the longest common run of lines of the two;
/// None when the texts differ by too much to work that out.
fn matches(base: &[&str], other: &[&str]) -> Option<Vec<Option<usize>>> {
    let mut out = vec![None; base.len()];
    // What the two start and end with alike needs no table.
    let head = base.iter().zip(other).take_while(|(a, b)| a == b).count();
    let tail = base[head..]
        .iter()
        .rev()
        .zip(other[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    for (at, slot) in out.iter_mut().enumerate().take(head) {
        *slot = Some(at);
    }
    for back in 1..=tail {
        out[base.len() - back] = Some(other.len() - back);
    }
    let (a, b) = (&base[head..base.len() - tail], &other[head..other.len() - tail]);
    if a.is_empty() || b.is_empty() {
        return Some(out);
    }
    if a.len().saturating_mul(b.len()) > MAX_CELLS {
        return None;
    }
    // lengths[i][j]: the longest common run of a[i..] and b[j..].
    let width = b.len() + 1;
    let mut lengths = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lengths[i * width + j] = if a[i] == b[j] {
                lengths[(i + 1) * width + j + 1] + 1
            } else {
                lengths[(i + 1) * width + j].max(lengths[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out[head + i] = Some(head + j);
            i += 1;
            j += 1;
        } else if lengths[(i + 1) * width + j] >= lengths[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    Some(out)
}

/// The text that has what `ours` and `theirs` each changed of `base`. None when both changed the same
/// lines in different ways, or the texts differ by too much to tell.
pub fn merge3(base: &str, ours: &str, theirs: &str) -> Option<String> {
    if ours == theirs || theirs == base {
        return Some(ours.to_owned());
    }
    if ours == base {
        return Some(theirs.to_owned());
    }
    let (base, a, b) = (lines(base), lines(ours), lines(theirs));
    let (in_a, in_b) = (matches(&base, &a)?, matches(&base, &b)?);
    let mut out = String::new();
    // What of each text is taken so far.
    let (mut at, mut at_a, mut at_b) = (0, 0, 0);
    // One stretch between two lines all three have: what one side left as it was gives way to what
    // the other made of it.
    let stretch = |out: &mut String, base: &[&str], a: &[&str], b: &[&str]| -> bool {
        let chosen = if a == base || a == b {
            b
        } else if b == base {
            a
        } else {
            return false;
        };
        out.extend(chosen.iter().copied());
        true
    };
    for line in 0..base.len() {
        let (Some(to_a), Some(to_b)) = (in_a[line], in_b[line]) else {
            continue;
        };
        if !stretch(&mut out, &base[at..line], &a[at_a..to_a], &b[at_b..to_b]) {
            return None;
        }
        out.push_str(base[line]);
        (at, at_a, at_b) = (line + 1, to_a + 1, to_b + 1);
    }
    if !stretch(&mut out, &base[at..], &a[at_a..], &b[at_b..]) {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "# Notes\n\none\ntwo\nthree\n\nfour\n";

    #[test]
    fn what_each_changed_is_in_the_merged_text() {
        let ours = "# Notes\n\none\nTWO\nthree\n\nfour\n";
        let theirs = "# Notes\n\none\ntwo\nthree\n\nfour\nfive\n";
        assert_eq!(
            merge3(BASE, ours, theirs).unwrap(),
            "# Notes\n\none\nTWO\nthree\n\nfour\nfive\n"
        );
        // The same from the other side.
        assert_eq!(merge3(BASE, theirs, ours), merge3(BASE, ours, theirs));
        // Lines added at the start, lines taken out in the middle.
        let first = "first\n# Notes\n\none\ntwo\nthree\n\nfour\n";
        let fewer = "# Notes\n\none\nthree\n\nfour\n";
        assert_eq!(
            merge3(BASE, first, fewer).unwrap(),
            "first\n# Notes\n\none\nthree\n\nfour\n"
        );
    }

    #[test]
    fn one_side_that_did_not_change_gives_way_and_the_same_change_twice_is_one() {
        let changed = "# Notes\n\nONE\ntwo\nthree\n\nfour\n";
        assert_eq!(merge3(BASE, BASE, changed).unwrap(), changed);
        assert_eq!(merge3(BASE, changed, BASE).unwrap(), changed);
        assert_eq!(merge3(BASE, changed, changed).unwrap(), changed);
        assert_eq!(merge3("", "", "new\n").unwrap(), "new\n");
        assert_eq!(merge3("", "a\n", "a\n").unwrap(), "a\n");
        // A last line with no ending is a line like any other.
        assert_eq!(merge3("a\nb\n", "A\nb\n", "a\nb\nc").unwrap(), "A\nb\nc");
    }

    #[test]
    fn the_same_lines_changed_in_two_ways_are_for_a_person() {
        let ours = "# Notes\n\none\nTWO\nthree\n\nfour\n";
        let theirs = "# Notes\n\none\ntwo!\nthree\n\nfour\n";
        assert_eq!(merge3(BASE, ours, theirs), None);
        // Two different texts with nothing to start from.
        assert_eq!(merge3("", "a\n", "b\n"), None);
        // Changes that touch: a line one side changed is next to a line the other added.
        let next_to = "# Notes\n\none\ntwo\nmore\nthree\n\nfour\n";
        assert_eq!(merge3(BASE, ours, next_to), None);
    }

    #[test]
    fn texts_that_differ_by_too_much_are_not_merged() {
        let many = |word: &str| (0..3000).map(|n| format!("{word} {n}\n")).collect::<String>();
        assert_eq!(merge3(&many("base"), &many("ours"), &many("theirs")), None);
        // A long text with one change on each side needs no table at all.
        let long = many("line");
        let ours = long.replacen("line 10\n", "ours\n", 1);
        let theirs = long.replacen("line 2990\n", "theirs\n", 1);
        let merged = merge3(&long, &ours, &theirs).unwrap();
        assert!(merged.contains("ours\n") && merged.contains("theirs\n"));
        assert_eq!(merged.lines().count(), 3000);
    }
}
