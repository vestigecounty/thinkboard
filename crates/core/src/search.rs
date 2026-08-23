//! Case-insensitive text search across a canvas's notes.
//!
//! Runs on every keystroke in the find bar, over every node, on the main
//! thread — so it is written to avoid allocating per node. Pure ASCII text and
//! query (the overwhelmingly common case) are compared as bytes with no
//! intermediate at all; anything else falls back to a folded `char` buffer that
//! is reused across nodes rather than rebuilt for each.

use crate::model::CanvasNode;

/// One search hit: a character range within a node's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Index into the node list.
    pub node: usize,
    /// Character offset of the hit within that node's text.
    pub start: usize,
    /// Length of the hit, in characters.
    pub len: usize,
}

/// Folds one char for case-insensitive comparison. Single-char folding keeps a
/// 1:1 mapping with the source, so offsets stay valid indices into the original
/// text — which is what the highlight needs.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Every occurrence of `query` across the text nodes, in board order. Ranges are
/// character offsets/lengths. An empty query matches nothing.
///
/// Matches do not overlap: after a hit the scan resumes past it, so `"aa"` finds
/// two matches in `"aaaa"`, not three.
pub fn find_matches(nodes: &[CanvasNode], query: &str) -> Vec<Match> {
    let needle: Vec<char> = query.chars().map(fold).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    // The all-ASCII fast path: byte offsets equal char offsets, and folding is
    // a byte operation, so the whole scan runs over the original string.
    let ascii_needle: Option<Vec<u8>> = query
        .is_ascii()
        .then(|| query.as_bytes().to_ascii_lowercase());

    let mut matches = Vec::new();
    // Reused across nodes; only the non-ASCII path touches it.
    let mut hay: Vec<char> = Vec::new();

    for (node, canvas_node) in nodes.iter().enumerate() {
        if !canvas_node.is_text() {
            continue;
        }
        let Some(text) = canvas_node.text.as_deref() else {
            continue;
        };
        // A node whose text is shorter than the query in *bytes* is shorter in
        // chars too, so it cannot match.
        if text.len() < needle.len() {
            continue;
        }

        match &ascii_needle {
            Some(needle_bytes) if text.is_ascii() => {
                scan(text.as_bytes(), needle_bytes, node, &mut matches, |hay, at, n| {
                    hay[at..at + n].eq_ignore_ascii_case(needle_bytes)
                });
            }
            _ => {
                hay.clear();
                hay.extend(text.chars().map(fold));
                scan(&hay, &needle, node, &mut matches, |hay, at, n| {
                    hay[at..at + n] == needle[..]
                });
            }
        }
    }
    matches
}

/// Walks `hay` for non-overlapping runs of `needle.len()` items that `eq`
/// accepts, recording each as a [`Match`] on `node`.
fn scan<T>(
    hay: &[T],
    needle: &[T],
    node: usize,
    matches: &mut Vec<Match>,
    eq: impl Fn(&[T], usize, usize) -> bool,
) {
    let n = needle.len();
    let mut start = 0;
    while start + n <= hay.len() {
        if eq(hay, start, n) {
            matches.push(Match { node, start, len: n });
            start += n; // non-overlapping
        } else {
            start += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn node(text: &str) -> CanvasNode {
        CanvasNode { text: Some(text.to_owned()), height: 100.0, ..fixtures::node("n") }
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert!(find_matches(&[node("anything")], "").is_empty());
    }

    #[test]
    fn matching_ignores_case() {
        let matches = find_matches(&[node("Hello World")], "hello");
        assert_eq!(matches.len(), 1);
        assert_eq!((matches[0].start, matches[0].len), (0, 5));
    }

    #[test]
    fn repeats_are_matched_without_overlapping() {
        let matches = find_matches(&[node("aaaa")], "aa");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].start, 0);
        assert_eq!(matches[1].start, 2);
    }

    #[test]
    fn offsets_are_char_indices_not_bytes() {
        // The highlight indexes chars, so a multi-byte prefix must not shift it.
        let matches = find_matches(&[node("héllo needle")], "needle");
        assert_eq!(matches[0].start, 6);
    }

    #[test]
    fn image_nodes_are_skipped() {
        let mut image = node("needle");
        image.kind = "file".to_owned();
        image.file = Some("pic.png".to_owned());
        assert!(find_matches(&[image], "needle").is_empty());
    }

    #[test]
    fn the_ascii_and_unicode_paths_agree() {
        // The fast path only runs when both query and text are ASCII, so every
        // case has to give the same answer either way. Forcing the slow path by
        // appending a non-ASCII char (past the matches) must not change them.
        let cases = [
            ("The Quick brown fox", "quick"),
            ("quick, quick!", "quick"),
            ("aaaa", "aa"),
            ("nothing here", "zzz"),
            ("CASE case CaSe", "case"),
        ];
        for (text, query) in cases {
            let ascii = find_matches(&[node(text)], query);
            let forced = find_matches(&[node(&format!("{text} ünicode"))], query);
            assert_eq!(
                ascii, forced,
                "paths disagree on {text:?} / {query:?}: {ascii:?} vs {forced:?}"
            );
        }
    }

    #[test]
    fn a_node_shorter_than_the_query_is_skipped() {
        assert!(find_matches(&[node("ab")], "abcdef").is_empty());
        // …but the length check is in bytes, so a short multi-byte string that
        // *could* match still gets scanned.
        assert_eq!(find_matches(&[node("ééé")], "ééé").len(), 1);
    }

    #[test]
    fn matches_are_reported_in_board_order() {
        let nodes = vec![node("first hit"), node("no"), node("second hit")];
        let matches = find_matches(&nodes, "hit");
        assert_eq!(matches.iter().map(|m| m.node).collect::<Vec<_>>(), [0, 2]);
    }
}
