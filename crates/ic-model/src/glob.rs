//! Icinga's `match()` wildcard, the one implementation every crate uses.
//!
//! Follows `Utility::Match` (`third-party/mmatch`), checked against Icinga
//! 2.15 (contract probes, `contract/` Docker instance) and, earlier, against
//! the C code on 800 000 random cases:
//!
//! - `*` matches any run of bytes (also `/`, also none), `?` exactly one
//!   *byte* (so `??` matches `ä`);
//! - `\*` and `\?` match the character itself; any other backslash (`\\`,
//!   `\x`, a trailing `\`) is an ordinary character, so the pattern `a\\b`
//!   matches the text `a\\b` (two backslashes), not `a\b`;
//! - ASCII letters match case-insensitively (`ICINGA*` matches `icinga-1`),
//!   other characters compare byte for byte (`Ä` does not match `ä`);
//! - the whole text must match;
//! - like the C original, pattern and text end at the first NUL byte.
//!
//! # Cost
//!
//! Filters are shared as YAML and patterns can come from object data, so the
//! matcher must not be slow on any input. It never backtracks: the stars
//! split the pattern into segments; the first segment must match at the
//! start of the text, the last one at its end, and each segment in between
//! is placed at its leftmost match after the previous one (for `*` and `?`
//! that is always a match if any placement is). Each segment is found in a
//! single pass over the text: a segment without `?` by a substring search
//! (`memchr::memmem`, linear), one with `?` by a bit-parallel scan (one
//! machine word for up to 64 bytes). Matching is therefore linear in the
//! text and the pattern, except for a `?` segment longer than 64 bytes,
//! whose remainder is compared at each place its first 64 bytes match:
//! [`Glob::extra_steps`] bounds that work so a caller with a budget (the
//! filter engine) can refuse it.

use std::borrow::Cow;

use memchr::memmem;

/// One element of a compiled pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item {
    /// `*`
    Star,
    /// `?`
    One,
    /// A byte, already lower-cased when it is an ASCII letter.
    Byte(u8),
}

/// Bytes a word-sized bit-parallel scan covers.
const WORD: usize = 64;

/// A compiled `match()` pattern, for matching many texts against one pattern.
#[derive(Clone, Debug)]
pub struct Glob {
    /// The pattern, for equality and debugging.
    items: Vec<Item>,
    shape: Shape,
}

impl PartialEq for Glob {
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
    }
}

impl Eq for Glob {}

/// The pattern split at its stars.
#[derive(Clone, Debug)]
enum Shape {
    /// No star: the text matches this segment exactly.
    Exact(Vec<Item>),
    /// `first*middle…*last`: `first` and `last` may be empty.
    Stars {
        first: Vec<Item>,
        middle: Vec<Searcher>,
        last: Vec<Item>,
    },
}

/// A segment between two stars, ready to be searched for.
#[derive(Clone, Debug)]
enum Searcher {
    /// Only bytes: a substring search (lower-cased text if it has letters).
    Literal {
        finder: Box<memmem::Finder<'static>>,
        letters: bool,
    },
    /// With `?`: a bit-parallel scan for the first [`WORD`] items, then the
    /// rest compared in place.
    Wild {
        items: Vec<Item>,
        /// Per byte: bit `i` set when item `i` (of the first [`WORD`])
        /// accepts that byte.
        masks: Box<[u64; 256]>,
    },
}

impl Searcher {
    fn new(items: Vec<Item>) -> Self {
        if items.iter().all(|item| matches!(item, Item::Byte(_))) {
            let needle: Vec<u8> = items
                .iter()
                .filter_map(|item| match item {
                    Item::Byte(byte) => Some(*byte),
                    _ => None,
                })
                .collect();
            let letters = needle.iter().any(u8::is_ascii_lowercase);
            return Self::Literal {
                finder: Box::new(memmem::Finder::new(&needle).into_owned()),
                letters,
            };
        }
        let mut masks = Box::new([0_u64; 256]);
        for (index, item) in items.iter().take(WORD).enumerate() {
            let bit = 1_u64 << index;
            match *item {
                Item::One => masks.iter_mut().for_each(|mask| *mask |= bit),
                Item::Byte(byte) => {
                    masks[usize::from(byte)] |= bit;
                    masks[usize::from(byte.to_ascii_uppercase())] |= bit;
                }
                Item::Star => unreachable!("segments have no stars"),
            }
        }
        Self::Wild { items, masks }
    }

    /// The segment's length in bytes.
    fn len(&self) -> usize {
        match self {
            Self::Literal { finder, .. } => finder.needle().len(),
            Self::Wild { items, .. } => items.len(),
        }
    }

    /// Where the segment first lies wholly inside `text[from..to]`.
    fn find(&self, text: &[u8], lower: &[u8], from: usize, to: usize) -> Option<usize> {
        match self {
            Self::Literal { finder, letters } => {
                let haystack = if *letters { lower } else { text };
                finder.find(&haystack[from..to]).map(|at| from + at)
            }
            Self::Wild { items, masks } => {
                let head = items.len().min(WORD);
                let done = 1_u64 << (head - 1);
                let mut state = 0_u64;
                for (offset, byte) in text[from..to].iter().enumerate() {
                    state = ((state << 1) | 1) & masks[usize::from(*byte)];
                    if state & done == 0 {
                        continue;
                    }
                    let start = from + offset + 1 - head;
                    if start + items.len() <= to && matches_at(&items[head..], text, start + head) {
                        return Some(start);
                    }
                }
                None
            }
        }
    }
}

/// Whether `items` (no stars) match `text` at `at` (the caller checks that
/// they fit).
fn matches_at(items: &[Item], text: &[u8], at: usize) -> bool {
    items
        .iter()
        .zip(&text[at..])
        .all(|(item, byte)| match item {
            Item::Byte(expected) => *expected == byte.to_ascii_lowercase(),
            Item::One | Item::Star => true,
        })
}

impl Glob {
    /// Compiles `pattern` (never fails: every text is a valid pattern).
    #[must_use]
    pub fn new(pattern: &str) -> Self {
        let bytes = until_nul(pattern);
        let mut items = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while let Some(&byte) = bytes.get(index) {
            index += 1;
            items.push(match byte {
                b'*' => {
                    // A run of stars is one star.
                    if items.last() == Some(&Item::Star) {
                        continue;
                    }
                    Item::Star
                }
                b'?' => Item::One,
                b'\\' if matches!(bytes.get(index), Some(b'*' | b'?')) => {
                    index += 1;
                    Item::Byte(bytes[index - 1])
                }
                other => Item::Byte(other.to_ascii_lowercase()),
            });
        }
        let mut segments: Vec<Vec<Item>> = items
            .split(|item| *item == Item::Star)
            .map(<[Item]>::to_vec)
            .collect();
        let shape = if segments.len() == 1 {
            Shape::Exact(segments.remove(0))
        } else {
            let last = segments.pop().unwrap_or_default();
            let first = segments.remove(0);
            Shape::Stars {
                first,
                middle: segments.into_iter().map(Searcher::new).collect(),
                last,
            }
        };
        Self { items, shape }
    }

    /// Whether the whole `text` matches.
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        let text = until_nul(text);
        match &self.shape {
            Shape::Exact(items) => text.len() == items.len() && matches_at(items, text, 0),
            Shape::Stars {
                first,
                middle,
                last,
            } => {
                let Some(end) = text.len().checked_sub(last.len()) else {
                    return false;
                };
                if end < first.len() || !matches_at(first, text, 0) || !matches_at(last, text, end)
                {
                    return false;
                }
                let lower: Cow<'_, [u8]> = if middle
                    .iter()
                    .any(|searcher| matches!(searcher, Searcher::Literal { letters: true, .. }))
                {
                    Cow::Owned(text.to_ascii_lowercase())
                } else {
                    Cow::Borrowed(text)
                };
                let mut at = first.len();
                for searcher in middle {
                    match searcher.find(text, &lower, at, end) {
                        Some(start) => at = start + searcher.len(),
                        None => return false,
                    }
                }
                true
            }
        }
    }

    /// At most how many byte comparisons matching a text of `text_len`
    /// bytes takes beyond the linear passes: only `?` segments longer than
    /// 64 bytes cost any (their remainder at each place their start
    /// matches). Zero for every pattern people write.
    #[must_use]
    pub fn extra_steps(&self, text_len: usize) -> usize {
        let Shape::Stars { middle, .. } = &self.shape else {
            return 0;
        };
        middle
            .iter()
            .filter_map(|searcher| match searcher {
                Searcher::Wild { items, .. } if items.len() > WORD => Some(items.len() - WORD),
                _ => None,
            })
            .fold(0_usize, |sum, rest| {
                sum.saturating_add(rest.saturating_mul(text_len))
            })
    }
}

/// Whether `text` matches the wildcard `pattern`, as Icinga's `match()`
/// does. Compile a [`Glob`] instead to test many texts.
#[must_use]
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    Glob::new(pattern).is_match(text)
}

/// C string semantics: the text ends at the first NUL byte.
fn until_nul(text: &str) -> &[u8] {
    let bytes = text.as_bytes();
    bytes
        .iter()
        .position(|byte| *byte == 0)
        .map_or(bytes, |end| &bytes[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A direct implementation of the semantics, as an oracle.
    fn oracle(pattern: &[u8], text: &[u8]) -> bool {
        let mut items = Vec::new();
        let mut index = 0;
        while index < pattern.len() {
            let byte = pattern[index];
            index += 1;
            items.push(match byte {
                0 => break,
                b'*' => Item::Star,
                b'?' => Item::One,
                b'\\' if matches!(pattern.get(index), Some(b'*' | b'?')) => {
                    index += 1;
                    Item::Byte(pattern[index - 1])
                }
                other => Item::Byte(other.to_ascii_lowercase()),
            });
        }
        let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
        // matches[i][j]: items[i..] matches text[j..].
        let mut matches = vec![vec![false; text.len() + 1]; items.len() + 1];
        matches[items.len()][text.len()] = true;
        for i in (0..items.len()).rev() {
            for j in (0..=text.len()).rev() {
                matches[i][j] = match items[i] {
                    Item::Star => matches[i + 1][j] || (j < text.len() && matches[i][j + 1]),
                    Item::One => j < text.len() && matches[i + 1][j + 1],
                    Item::Byte(byte) => {
                        j < text.len()
                            && text[j].to_ascii_lowercase() == byte
                            && matches[i + 1][j + 1]
                    }
                };
            }
        }
        matches[0][0]
    }

    #[test]
    fn icinga_match_test_suite() {
        // Icinga's own test cases (test/base-match.cpp).
        assert!(glob_matches("*", "hello"));
        assert!(!glob_matches("\\**", "hello"));
        assert!(glob_matches("\\**", "*ello"));
        assert!(glob_matches("?e*l?", "hello"));
        assert!(glob_matches("?e*l?", "helo"));
        assert!(!glob_matches("world", "hello"));
        assert!(!glob_matches("hee*", "hello"));
        assert!(glob_matches("he??o", "hello"));
        assert!(glob_matches("he?", "hel"));
        assert!(glob_matches("he*", "hello"));
        assert!(glob_matches("he*o", "heo"));
        assert!(glob_matches("he**o", "heo"));
        assert!(glob_matches("he**o", "hello"));
    }

    /// Every row was run through `match()` on the Docker Icinga 2.15.6
    /// (`filter_vars`, `host.name=="icinga-master" && match(p, t)`).
    #[test]
    fn matches_icinga_on_case_and_escapes() {
        let cases = [
            // Case: ASCII letters fold, everything else is bytes.
            ("ICINGA*", "icinga-master", true),
            ("icinga*", "ICINGA-MASTER", true),
            ("Icinga-Master", "icinga-master", true),
            ("Ä", "ä", false),
            ("ä*", "ä-x", true),
            ("a*c", "aäc", true),
            // `?` is one byte.
            ("a?c", "abc", true),
            ("a?c", "ac", false),
            ("a?c", "aäc", false),
            ("a??c", "aäc", true),
            ("?", "ä", false),
            ("??", "ä", true),
            // Escapes: only `\*` and `\?`.
            ("a\\*b", "a*b", true),
            ("a\\*b", "axb", false),
            ("a\\?b", "a?b", true),
            ("a\\?b", "axb", false),
            ("\\*", "*", true),
            ("\\*", "x", false),
            ("a\\\\b", "a\\b", false),
            ("a\\\\b", "a\\\\b", true),
            ("a\\xb", "a\\xb", true),
            ("a\\xb", "axb", false),
            ("a\\", "a\\", true),
            ("a\\", "a", false),
            ("a\\\\*", "a\\zz", false),
            ("a\\\\*", "a\\*", true),
            ("a\\*", "a\\x", false),
            ("a\\*", "a*", true),
            // Stars.
            ("*", "", true),
            ("", "", true),
            ("", "x", false),
            ("a**b", "ab", true),
        ];
        for (pattern, text, expected) in cases {
            assert_eq!(
                glob_matches(pattern, text),
                expected,
                "match({pattern:?}, {text:?})"
            );
            assert_eq!(
                oracle(pattern.as_bytes(), text.as_bytes()),
                expected,
                "oracle({pattern:?}, {text:?})"
            );
        }
    }

    #[test]
    fn permission_patterns() {
        assert!(glob_matches("actions/*", "actions/"));
        assert!(glob_matches("*/query/*", "objects/query/host"));
        assert!(glob_matches("events/??", "events/ab"));
        assert!(!glob_matches("events/??", "events/abc"));
        assert!(glob_matches("a*b*c", "axxbyyc"));
        assert!(!glob_matches("a*b*c", "axxbyy"));
        assert!(glob_matches("**", ""));
        assert!(!glob_matches("a*b*c", "axxcyyb"));
    }

    #[test]
    fn text_ends_at_the_first_nul() {
        assert!(glob_matches("a*", "a\0b"));
        assert!(glob_matches("a\0b", "a"));
        assert!(glob_matches("*\n*", "a\nb"));
    }

    #[test]
    fn special_characters_are_literal() {
        assert!(glob_matches("[ab]", "[ab]"));
        assert!(!glob_matches("[ab]", "a"));
        assert!(glob_matches("^$.", "^$."));
        assert!(glob_matches("*.example.com", "web.example.com"));
        assert!(!glob_matches("*.example.com", "webXexampleYcom"));
    }

    #[test]
    fn matches_oracle_on_generated_cases() {
        // Deterministic pseudo-random patterns over a small alphabet that
        // includes every special character.
        const ALPHABET: &[u8] = b"aAb*?\\.";
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..50_000 {
            let pattern_len = usize::try_from(next() % 10).unwrap();
            let text_len = usize::try_from(next() % 10).unwrap();
            let pick = |value: u64| ALPHABET[usize::try_from(value % 7).unwrap()];
            let pattern: Vec<u8> = (0..pattern_len).map(|_| pick(next())).collect();
            let text: Vec<u8> = (0..text_len).map(|_| pick(next())).collect();
            let pattern = String::from_utf8(pattern).unwrap();
            let text = String::from_utf8(text).unwrap();
            assert_eq!(
                glob_matches(&pattern, &text),
                oracle(pattern.as_bytes(), text.as_bytes()),
                "match({pattern:?}, {text:?})"
            );
        }
    }

    #[test]
    fn long_wild_segments_match_like_the_oracle() {
        // `?` segments longer than one machine word: the bit-parallel scan
        // finds where the first 64 bytes fit, the rest is compared there.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..300 {
            let segment: String = (0..60 + next() % 20)
                .map(|_| if next() % 5 == 0 { '?' } else { 'a' })
                .collect();
            let pattern = format!("*{segment}b*");
            let text: String = (0..next() % 200)
                .map(|_| if next() % 30 == 0 { 'B' } else { 'A' })
                .collect();
            assert_eq!(
                glob_matches(&pattern, &text),
                oracle(pattern.as_bytes(), text.as_bytes()),
                "match({pattern:?}, {text:?})"
            );
        }
        let long = Glob::new(&format!("*{}*", "?".repeat(100)));
        assert_eq!(long.extra_steps(10), 360);
        assert_eq!(Glob::new("*a?b*c*").extra_steps(1_000_000), 0);
        assert_eq!(Glob::new(&"a".repeat(500)).extra_steps(1_000_000), 0);
    }

    #[test]
    fn many_stars_do_not_blow_up() {
        let pattern = "*a".repeat(5_000);
        assert!(!glob_matches(&pattern, "a"));
        assert!(glob_matches(&pattern, &"a".repeat(5_000)));
        assert!(!glob_matches(&format!("{pattern}b"), &"a".repeat(100_000)));
    }

    /// One star before a long tail used to backtrack at every position
    /// (pattern length times text length: seconds for 100 000 bytes).
    #[test]
    fn long_segments_take_linear_time() {
        let text = "a".repeat(100_000);
        let tail = format!("{}b", "a".repeat(10_000));
        let wild = format!("{}?b", "a".repeat(40));
        let start = std::time::Instant::now();
        for pattern in [
            format!("*{tail}"),
            format!("*{tail}*"),
            format!("*{tail}*x"),
            format!("x*{tail}*"),
            format!("*{wild}*"),
            format!("*{}*", tail.to_uppercase()),
            format!("*{}?b*", "?a".repeat(29)),
        ] {
            assert!(!glob_matches(&pattern, &text), "{}", &pattern[..20]);
        }
        assert!(glob_matches(&format!("*{}*", "a".repeat(10_000)), &text));
        assert!(glob_matches(&format!("*{}", "A?".repeat(5_000)), &text));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }
}
