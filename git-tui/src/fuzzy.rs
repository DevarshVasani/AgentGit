//! Tiny subsequence fuzzy matcher for the file finder (`/`).
//!
//! No extra dependency: query chars must appear in order (case-insensitive)
//! in the candidate. Lower scores rank first; ties keep list order.

/// Score `query` against `candidate`. `None` when the query is not a
/// subsequence of the candidate. Empty query matches everything at 0.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<u32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let c: Vec<char> = candidate.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut positions = Vec::with_capacity(q.len());
    let mut from = 0;
    for qc in &q {
        let mut found = None;
        for (i, cc) in c.iter().enumerate().skip(from) {
            if cc == qc {
                found = Some(i);
                break;
            }
        }
        let pos = found?;
        positions.push(pos);
        from = pos + 1;
    }
    // Late starts and gappy runs cost; word-boundary hits earn back.
    let mut score = positions[0] as u32;
    for w in positions.windows(2) {
        let gap = (w[1] - w[0] - 1) as u32;
        score += gap * 3;
    }
    for &pos in &positions {
        let boundary = pos == 0 || matches!(c[pos - 1], '/' | '_' | '-' | '.' | ' ');
        if boundary {
            score = score.saturating_sub(2);
        }
    }
    Some(score)
}

/// Rank candidate indices by `(score, length, index)`; `None` scores drop.
pub fn rank(query: &str, candidates: &[&str]) -> Vec<(usize, u32)> {
    let mut out: Vec<(usize, u32)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, cand)| fuzzy_score(query, cand).map(|s| (i, s)))
        .collect();
    out.sort_by(|a, b| (a.1, candidates[a.0].len(), a.0).cmp(&(b.1, candidates[b.0].len(), b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_all_at_zero() {
        assert_eq!(fuzzy_score("", "src/main.rs"), Some(0));
    }

    #[test]
    fn non_subsequence_is_none() {
        assert_eq!(fuzzy_score("zzz", "src/main.rs"), None);
        assert_eq!(fuzzy_score("mnx", "main.rs"), None);
    }

    #[test]
    fn matching_is_case_insensitive_and_ordered() {
        assert!(fuzzy_score("mr", "main.rs").is_some());
        assert!(fuzzy_score("MR", "main.rs").is_some());
        // Wrong order fails either way.
        assert_eq!(fuzzy_score("rm", "main.rs"), None);
        assert_eq!(fuzzy_score("RM", "main.rs"), None);
    }

    #[test]
    fn consecutive_boundary_hits_rank_first() {
        let cands = ["src/app.rs", "docs/appendix.md"];
        let ranked = rank("ap", &cands);
        // Both match, but "src/app.rs" hits at an earlier '/' boundary.
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, 0);
    }

    #[test]
    fn exact_basename_beats_deep_path() {
        let cands = ["a/very/deep/dir/tree/main.rs", "main.rs"];
        let ranked = rank("main", &cands);
        assert_eq!(ranked[0].0, 1);
    }
}
