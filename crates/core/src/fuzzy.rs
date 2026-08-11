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
}
