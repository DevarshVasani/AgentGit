//! Word-level highlighting for the diff panel (presentation-only).
//!
//! Pure functions over line text; the engine stays line-oriented.

/// A run of text within one diff line; `changed` runs get emphasized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordSeg {
    pub text: String,
    pub changed: bool,
}

use similar::{ChangeTag, TextDiff};

/// Char-level diff of one old line against one new line.
/// Returns segments for the old line and the new line.
///
/// The char diff is O(ND), so two guards keep frames fast:
/// - an empty side needs no diffing: the other side is wholly changed;
/// - lines past `CHAR_DIFF_CAP_BYTES` combined skip the diff and render as
///   wholly changed (line-level red/green wash still applies).
const CHAR_DIFF_CAP_BYTES: usize = 400;

fn changed_side(text: &str) -> Vec<WordSeg> {
    if text.is_empty() {
        Vec::new()
    } else {
        vec![WordSeg {
            text: text.to_string(),
            changed: true,
        }]
    }
}

/// Char-level diff of one old line against one new line.
/// Returns segments for the old line and the new line.
pub fn word_diff(old: &str, new: &str) -> (Vec<WordSeg>, Vec<WordSeg>) {
    if old.is_empty() || new.is_empty() {
        return (changed_side(old), changed_side(new));
    }
    if old.len() + new.len() > CHAR_DIFF_CAP_BYTES {
        return (changed_side(old), changed_side(new));
    }
    let diff = TextDiff::from_chars(old, new);
    let mut old_segs: Vec<WordSeg> = Vec::new();
    let mut new_segs: Vec<WordSeg> = Vec::new();
    for op in diff.ops() {
        for change in diff.iter_changes(op) {
            let text: String = change.value().chars().collect();
            if text.is_empty() {
                continue;
            }
            match change.tag() {
                ChangeTag::Equal => {
                    push_seg(&mut old_segs, text.clone(), false);
                    push_seg(&mut new_segs, text, false);
                }
                ChangeTag::Delete => push_seg(&mut old_segs, text, true),
                ChangeTag::Insert => push_seg(&mut new_segs, text, true),
            }
        }
    }
    (old_segs, new_segs)
}

fn push_seg(segs: &mut Vec<WordSeg>, text: String, changed: bool) {
    if let Some(last) = segs.last_mut() {
        if last.changed == changed {
            last.text.push_str(&text);
            return;
        }
    }
    segs.push(WordSeg { text, changed });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(segs: &[WordSeg]) -> String {
        segs.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn identical_lines_have_no_changed_runs() {
        let (old, new) = word_diff("hello world", "hello world");
        assert_eq!(texts(&old), "hello world");
        assert_eq!(texts(&new), "hello world");
        assert!(old.iter().all(|s| !s.changed));
        assert!(new.iter().all(|s| !s.changed));
    }

    #[test]
    fn single_changed_word_is_isolated() {
        let (old, new) = word_diff("hello world", "hello WORLD");
        assert_eq!(texts(&old), "hello world");
        assert_eq!(texts(&new), "hello WORLD");
        assert!(old.iter().any(|s| s.changed && s.text == "world"));
        assert!(new.iter().any(|s| s.changed && s.text == "WORLD"));
        assert!(old.iter().any(|s| !s.changed && s.text.contains("hello")));
    }

    #[test]
    fn fully_replaced_line_is_all_changed() {
        let (old, new) = word_diff("aaa", "bbb");
        assert!(old.iter().all(|s| s.changed));
        assert!(new.iter().all(|s| s.changed));
    }

    #[test]
    fn empty_sides_are_all_changed() {
        let (old, new) = word_diff("", "new");
        assert!(old.is_empty());
        assert_eq!(texts(&new), "new");
        assert!(new.iter().all(|s| s.changed));
        let (old, new) = word_diff("gone", "");
        assert!(new.is_empty());
        assert_eq!(texts(&old), "gone");
        assert!(old.iter().all(|s| s.changed));
    }

    #[test]
    fn overlong_lines_skip_char_diff_and_stay_whole() {
        // The char diff is O(ND): long lines render as wholly changed so a
        // single minified line can't freeze a frame. No text may be lost.
        let old = "a".repeat(1000);
        let new = "b".repeat(1000);
        let (old_segs, new_segs) = word_diff(&old, &new);
        assert_eq!(texts(&old_segs), old);
        assert_eq!(texts(&new_segs), new);
        assert!(old_segs.iter().all(|s| s.changed));
        assert!(new_segs.iter().all(|s| s.changed));
    }

    #[test]
    fn short_lines_still_get_char_diff() {
        let (old, new) = word_diff("hello world", "hello WORLD");
        assert!(old.iter().any(|s| s.changed && s.text == "world"));
        assert!(new.iter().any(|s| s.changed && s.text == "WORLD"));
    }
}
