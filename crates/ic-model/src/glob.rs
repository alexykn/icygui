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
//! Matching never backtracks beyond the last `*`, so it cannot blow up: the
//! worst case is the pattern's length times the text's.

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

/// A compiled `match()` pattern, for matching many texts against one pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glob {
    items: Vec<Item>,
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
        Self { items }
    }

    /// Whether the whole `text` matches.
    #[must_use]
    pub fn is_match(&self, text: &str) -> bool {
        let text = until_nul(text);
        let (mut p, mut t) = (0, 0);
        // Where the last `*` stands and how much text it has taken so far.
        let mut star: Option<(usize, usize)> = None;
        while t < text.len() {
            match self.items.get(p) {
                Some(Item::Star) => {
                    star = Some((p, t));
                    p += 1;
                }
                Some(Item::One) => {
                    p += 1;
                    t += 1;
                }
                Some(Item::Byte(byte)) if *byte == text[t].to_ascii_lowercase() => {
                    p += 1;
                    t += 1;
                }
                _ => match star {
                    Some((star_p, star_t)) => {
                        p = star_p + 1;
                        t = star_t + 1;
                        star = Some((star_p, star_t + 1));
                    }
                    None => return false,
                },
            }
        }
        self.items[p..].iter().all(|item| *item == Item::Star)
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
        for _ in 0..20_000 {
            let pattern_len = usize::try_from(next() % 7).unwrap();
            let text_len = usize::try_from(next() % 7).unwrap();
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
    fn many_stars_do_not_blow_up() {
        let pattern = "*a".repeat(5_000);
        assert!(!glob_matches(&pattern, "a"));
        assert!(glob_matches(&pattern, &"a".repeat(5_000)));
        assert!(!glob_matches(&format!("{pattern}b"), &"a".repeat(100_000)));
    }
}
