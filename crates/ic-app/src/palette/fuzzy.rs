//! Fuzzy matching for the command palette: every character of the query
//! appears in the candidate in order (case-insensitive); contiguous runs,
//! word starts and an early start score higher.

/// What a match found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Match {
    /// Higher is better.
    pub(crate) score: i32,
    /// The matched characters' indices (in `char`s) in the candidate, in
    /// order, for highlighting.
    pub(crate) positions: Vec<usize>,
}

/// Points for a match at the candidate's start.
const START: i32 = 12;
/// Points for a match at a word's start (after a separator or a case
/// change).
const WORD_START: i32 = 9;
/// Points for a character right after the previous match.
const CONSECUTIVE: i32 = 6;
/// Points for every matched character.
const CHARACTER: i32 = 2;
/// Points lost per skipped character between matches (capped per gap).
const GAP: i32 = 1;
/// The most a single gap costs.
const MAX_GAP: i32 = 6;
/// Points for the query appearing whole, as a substring.
const SUBSTRING: i32 = 20;

/// Matches `query` against `candidate`, both already lowercased.
/// `None` if some query character is missing (or the query is empty).
#[cfg(test)]
pub(crate) fn fuzzy_match(query: &str, candidate: &str) -> Option<Match> {
    let haystack: Vec<char> = candidate.chars().collect();
    let needle: Vec<char> = query.chars().collect();
    match_chars(&needle, &haystack)
}

/// [`fuzzy_match`] on characters, so a candidate is split once, when the
/// palette indexes it, not on every keystroke.
fn match_chars(needle: &[char], haystack: &[char]) -> Option<Match> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    // A whole substring, preferably at a word start: the best case.
    if let Some(found) = substring_at_word(haystack, needle) {
        let positions: Vec<usize> = (found..found + needle.len()).collect();
        let score = score(haystack, &positions) + SUBSTRING;
        return Some(Match { score, positions });
    }
    // Otherwise each character at its first chance, word starts first.
    let mut positions = Vec::with_capacity(needle.len());
    let mut from = 0;
    for &wanted in needle {
        let next = next_position(haystack, from, wanted)?;
        positions.push(next);
        from = next + 1;
    }
    Some(Match {
        score: score(haystack, &positions),
        positions,
    })
}

/// The best start of `needle` as a substring of `haystack`: the first one
/// at a word start, else the first one.
fn substring_at_word(haystack: &[char], needle: &[char]) -> Option<usize> {
    let mut first = None;
    for start in 0..=haystack.len() - needle.len() {
        if haystack[start..start + needle.len()] == *needle {
            if is_word_start(haystack, start) {
                return Some(start);
            }
            first.get_or_insert(start);
        }
    }
    first
}

/// The next occurrence of `wanted` from `from`: one at a word start if
/// there is one before the next plain occurrence's word ends, else the
/// first.
fn next_position(haystack: &[char], from: usize, wanted: char) -> Option<usize> {
    let first = (from..haystack.len()).find(|&index| haystack[index] == wanted)?;
    if is_word_start(haystack, first) {
        return Some(first);
    }
    // A later word start with the character beats a mid-word match.
    let word_start = (first + 1..haystack.len())
        .find(|&index| haystack[index] == wanted && is_word_start(haystack, index));
    Some(word_start.unwrap_or(first))
}

fn is_word_start(haystack: &[char], index: usize) -> bool {
    index == 0
        || haystack
            .get(index - 1)
            .is_some_and(|before| !before.is_alphanumeric())
}

fn score(haystack: &[char], positions: &[usize]) -> i32 {
    let mut score = 0;
    let mut previous: Option<usize> = None;
    for &position in positions {
        score += CHARACTER;
        if position == 0 {
            score += START;
        } else if is_word_start(haystack, position) {
            score += WORD_START;
        }
        match previous {
            Some(previous) if position == previous + 1 => score += CONSECUTIVE,
            Some(previous) => {
                let gap = i32::try_from(position - previous - 1).unwrap_or(i32::MAX);
                score -= (gap * GAP).min(MAX_GAP);
            }
            None => {
                let lead = i32::try_from(position).unwrap_or(i32::MAX);
                score -= lead.min(MAX_GAP);
            }
        }
        previous = Some(position);
    }
    // Shorter candidates win ties: a closer match.
    score - i32::try_from(haystack.len() / 16).unwrap_or(i32::MAX)
}

/// A query split into its whitespace-separated terms, lowercased, ready
/// to match many candidates.
#[derive(Clone, Debug)]
pub(crate) struct Query {
    terms: Vec<Vec<char>>,
}

impl Query {
    /// Splits `query` (lowercased here).
    pub(crate) fn new(query: &str) -> Self {
        Self {
            terms: query
                .to_lowercase()
                .split_whitespace()
                .map(|term| term.chars().collect())
                .collect(),
        }
    }

    /// Matches every term against `candidate` (lowercased characters): all
    /// must match; the scores add up and the positions merge. `None` for
    /// an empty query.
    pub(crate) fn matches(&self, candidate: &[char]) -> Option<Match> {
        if self.terms.is_empty() {
            return None;
        }
        let mut total = Match {
            score: 0,
            positions: Vec::new(),
        };
        for term in &self.terms {
            let found = match_chars(term, candidate)?;
            total.score += found.score;
            total.positions.extend(found.positions);
        }
        total.positions.sort_unstable();
        total.positions.dedup();
        Some(total)
    }
}

/// Matches every whitespace-separated term of `query` against
/// `candidate`: all must match; the scores add up and the positions merge.
#[cfg(test)]
pub(crate) fn match_terms(query: &str, candidate: &str) -> Option<Match> {
    let candidate: Vec<char> = candidate.chars().collect();
    Query::new(query).matches(&candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(query: &str, candidate: &str) -> i32 {
        fuzzy_match(query, candidate).map_or(i32::MIN, |found| found.score)
    }

    #[test]
    fn every_character_must_appear_in_order() {
        assert!(fuzzy_match("pgr", "postgres-replication").is_some());
        assert!(fuzzy_match("rgp", "postgres-replication").is_none());
        assert!(fuzzy_match("", "anything").is_none());
        assert!(fuzzy_match("longer than", "short").is_none());
    }

    #[test]
    fn substrings_and_word_starts_win() {
        assert!(score("repl", "postgres-replication") > score("repl", "spare-plot-lane"));
        assert!(score("db", "db-prod-03") > score("db", "mongodb"));
        assert!(score("prod", "db-prod-03") > score("prod", "pr-o-d"));
        let found = fuzzy_match("rep", "postgres-replication").unwrap();
        assert_eq!(found.positions, [9, 10, 11], "the word start, not `res`");
    }

    #[test]
    fn spread_out_characters_prefer_word_starts() {
        let found = fuzzy_match("pr", "postgres-replication").unwrap();
        assert_eq!(found.positions[0], 0);
        let found = fuzzy_match("kn", "k8s-node-07").unwrap();
        assert_eq!(found.positions, [0, 4]);
    }

    #[test]
    fn all_terms_must_match() {
        let candidate = "postgres-replication db-prod-03";
        let found = match_terms("repl db-prod", candidate).unwrap();
        assert!(found.positions.contains(&9));
        assert!(found.positions.contains(&21));
        assert!(match_terms("repl web", candidate).is_none());
        assert!(match_terms("   ", candidate).is_none());
    }

    #[test]
    fn unicode_is_matched_by_character() {
        let found = fuzzy_match("über", "müller über-host").unwrap();
        assert_eq!(found.positions, [7, 8, 9, 10]);
    }
}
