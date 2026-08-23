//! The subsequence matcher behind every "type a few letters" list in the app —
//! the command palette and the vault browser. Kept here rather than beside
//! either one so both rank by identical rules.

/// Case-insensitive subsequence match with a score (higher = better): contiguous
/// runs and word-start hits are rewarded. `None` if not every query char appears
/// in order. An empty query matches everything with score 0.
pub fn score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut qi = 0;
    let mut score = 0;
    let mut prev: Option<usize> = None;
    for (i, &tc) in t.iter().enumerate() {
        if qi < q.len() && tc == q[qi] {
            score += 1;
            if prev == Some(i.wrapping_sub(1)) {
                score += 5; // contiguous run
            }
            if i == 0 || !t[i - 1].is_alphanumeric() {
                score += 8; // start of a word
            }
            prev = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score)
}

/// How far a hidden search term ranks below a visible one.
///
/// Small on purpose: a keyword match is a real match, just a weaker signal than
/// the name you can see. Large enough that a title match always wins, small
/// enough that a keyword match still beats a poor title match.
pub const ALIAS_PENALTY: i32 = 4;

/// Scores `query` against what a row *says*, falling back to terms it carries
/// but doesn't show, at [`ALIAS_PENALTY`].
///
/// The same shape in four places — the palette's keywords, a theme's author, a
/// model's display name, a board's vault-relative path — and it wants to stay
/// the same in all of them: a row found by something invisible should never
/// outrank one found by what's printed on it.
pub fn score_alias(query: &str, title: &str, alias: Option<&str>) -> Option<i32> {
    score(query, title)
        .or_else(|| score(query, alias?).map(|score| score - ALIAS_PENALTY))
}

/// The indices of `items` that match, best first.
///
/// A **stable** sort, which is what lets each caller's own order stand in for a
/// tiebreak — registry order in the palette, modified time in the vault.
pub fn rank<T>(items: &[T], mut score: impl FnMut(&T) -> Option<i32>) -> Vec<usize> {
    let mut scored: Vec<(usize, i32)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| Some((i, score(item)?)))
        .collect();
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_word_starts_and_contiguity() {
        // Subsequence found → Some; missing char → None.
        assert!(score("nb", "New board").is_some());
        assert!(score("xyz", "New board").is_none());
        // A contiguous, word-start prefix beats a scattered match.
        let prefix = score("new", "New board").unwrap();
        let scattered = score("new", "Rename widget").unwrap_or(i32::MIN);
        assert!(prefix > scattered);
    }

    #[test]
    fn an_empty_query_matches_everything() {
        assert_eq!(score("", "anything at all"), Some(0));
    }

    #[test]
    fn a_hidden_term_never_outranks_a_visible_one() {
        // "schoonover" is Solarized's author and appears nowhere on the row, so
        // it must find the theme — but a theme actually *called* something must
        // win over one that merely mentions it.
        let by_alias = score_alias("solar", "Nord", Some("solarized")).expect("matches");
        let by_title = score_alias("solar", "Solarized Dark", None).expect("matches");
        assert!(by_title > by_alias, "{by_title} should beat {by_alias}");
    }

    #[test]
    fn no_alias_means_only_the_title_matches() {
        assert!(score_alias("gruv", "Gruvbox Dark", None).is_some());
        assert!(score_alias("schoonover", "Gruvbox Dark", None).is_none());
    }

    #[test]
    fn ranking_is_stable_so_the_callers_own_order_breaks_ties() {
        // Equal scores keep registry order in the palette and modified order in
        // the vault; a sort that reordered them would shuffle the list under a
        // query that says nothing about which comes first.
        let items = ["copy", "copy", "copy"];
        assert_eq!(rank(&items, |t| score("copy", t)), [0, 1, 2]);
    }

    #[test]
    fn ranking_drops_what_does_not_match() {
        let items = ["alpha", "beta", "gamma"];
        // Only the rows that match, and the best first: "gam" is a word-start
        // run in "gamma" and absent from the other two.
        assert_eq!(rank(&items, |t| score("gam", t)), [2]);
        assert!(rank(&items, |t| score("zzz", t)).is_empty());
        assert_eq!(rank(&items, |t| score("", t)).len(), 3, "an empty query keeps everything");
    }

}
