//! Patterns for `match()`, `regex()` and `cidr_match()`.
//!
//! - Globs follow Icinga's `match()` (`third-party/mmatch`) and are matched by
//!   `ic_model::Glob`, the one implementation the whole workspace shares
//!   (its documentation states the semantics: `*`, `?` as one byte, only
//!   `\*` and `\?` escapes, ASCII-only case folding, NUL ends the text).
//!   Matching is linear in the text; the one exception (a `?` run longer
//!   than 64 bytes) is charged to the evaluation's budget.
//!
//!   This was checked against Icinga's C implementation on 800,000 random
//!   patterns and texts (escapes, case, NUL, line breaks, UTF-8). The only
//!   difference is deliberate: on x86 builds of Icinga (signed `char`), a
//!   non-ASCII character directly after a `*` never matches (`*ä*` matches
//!   nothing), because the matcher compares `tolower()` of a negative `char`
//!   inconsistently. That is undefined behaviour in C and works on ARM
//!   builds; here such patterns match as documented.
//! - Regular expressions behave like Icinga's Boost.Regex in its default Perl
//!   mode: they work on bytes, search anywhere in the text, `^`/`$` also
//!   match at line breaks and `.` matches line breaks. The syntax is the
//!   `regex` crate's: no look-around and no back-references.
//! - CIDR patterns follow `Utility::CidrMatch`: IPv4 addresses are mapped to
//!   IPv6 (`::ffff:a.b.c.d`), a pattern without a prefix length must match
//!   exactly, and bits outside the prefix must be zero.

use std::fmt::Write as _;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};

use regex::bytes::{Regex, RegexBuilder};

use crate::ops::parse_integer;
use crate::value::preview_str;

/// Compiled patterns are capped at this size so a pathological pattern
/// can't use unbounded memory.
const REGEX_SIZE_LIMIT: usize = 4 * 1024 * 1024;

/// A compiled `match()` glob (see `ic_model::Glob` for the semantics).
#[derive(Debug)]
pub(crate) struct Glob(ic_model::Glob);

impl Glob {
    /// Compiles a glob. Every text is a valid glob, so this cannot fail; it
    /// returns a `Result` like the other pattern kinds.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "same shape as the other pattern constructors"
    )]
    pub(crate) fn new(pattern: &str) -> Result<Glob, String> {
        Ok(Glob(ic_model::Glob::new(pattern)))
    }

    /// Whether the whole `text` matches.
    pub(crate) fn is_match(&self, text: &str) -> bool {
        self.0.is_match(text)
    }

    /// The comparisons matching `text_len` bytes may take beyond the
    /// linear passes, to be charged to the evaluation's budget.
    pub(crate) fn extra_steps(&self, text_len: usize) -> usize {
        self.0.extra_steps(text_len)
    }
}

/// C string semantics: the text ends at the first NUL byte.
fn until_nul(text: &str) -> &str {
    text.find('\0').map_or(text, |end| &text[..end])
}

/// A compiled `regex()` pattern.
#[derive(Debug)]
pub(crate) struct IcingaRegex {
    regex: Regex,
}

impl IcingaRegex {
    /// Compiles a regular expression with Boost's default flags.
    pub(crate) fn new(pattern: &str) -> Result<IcingaRegex, String> {
        RegexBuilder::new(&escape_non_ascii(pattern))
            .unicode(false)
            .multi_line(true)
            .crlf(true)
            .dot_matches_new_line(true)
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map(|regex| IcingaRegex { regex })
            .map_err(|error| format!("invalid regular expression: {}", regex_error(&error)))
    }

    /// Whether the pattern matches anywhere in `text`.
    pub(crate) fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text.as_bytes())
    }
}

/// Boost matches bytes, so a non-ASCII character is a sequence of byte
/// literals (inside classes: a set of bytes). Spell them as `\xNN` escapes,
/// which the byte-oriented `regex` syntax accepts everywhere. A backslash
/// before a non-ASCII character escapes its first byte, which is a literal
/// anyway.
fn escape_non_ascii(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(next) if !next.is_ascii() => push_bytes(&mut out, next),
                Some(next) => {
                    out.push('\\');
                    out.push(next);
                }
                None => out.push('\\'),
            }
        } else if ch.is_ascii() {
            out.push(ch);
        } else {
            push_bytes(&mut out, ch);
        }
    }
    out
}

fn push_bytes(out: &mut String, ch: char) {
    let mut buffer = [0; 4];
    for byte in ch.encode_utf8(&mut buffer).bytes() {
        let _ = write!(out, r"\x{byte:02X}");
    }
}

/// The last line of the `regex` crate's error, which says what is wrong
/// (the lines above repeat the pattern and point at the problem).
fn regex_error(error: &regex::Error) -> String {
    let text = error.to_string();
    let last = text
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(&text);
    last.trim().trim_start_matches("error: ").to_owned()
}

/// A compiled `cidr_match()` pattern: an IPv6 network (IPv4 is mapped).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Cidr {
    network: u128,
    prefix: u32,
}

impl Cidr {
    /// Parses `address/prefix` or a bare address (exact match).
    pub(crate) fn new(pattern: &str) -> Result<Cidr, String> {
        let (address, prefix) = match pattern.split_once('/') {
            Some((address, prefix)) => (address, Some(parse_long(prefix)?)),
            None => (pattern, None),
        };
        let (network, is_v4) = parse_ip(address).ok_or_else(|| {
            format!(
                "invalid IP address '{}' in CIDR pattern",
                preview_str(address)
            )
        })?;
        let mut bits = prefix.unwrap_or(0);
        if is_v4 {
            if !(0..=32).contains(&bits) {
                return Err("mask must be between 0 and 32 for IPv4 CIDR masks".to_owned());
            }
            bits += 96;
        }
        if prefix.is_none() {
            bits = 128;
        }
        let prefix = u32::try_from(bits)
            .ok()
            .filter(|bits| *bits <= 128)
            .ok_or_else(|| "mask must be between 0 and 128 for IPv6 CIDR masks".to_owned())?;
        if network & host_mask(prefix) != 0 {
            return Err(format!(
                "masked-off bits must all be zero in CIDR pattern '{}'",
                preview_str(pattern)
            ));
        }
        Ok(Cidr { network, prefix })
    }

    /// Whether `ip` is in the network. Text that isn't an IP address doesn't
    /// match.
    pub(crate) fn contains(&self, ip: &str) -> bool {
        parse_ip(until_nul(ip))
            .is_some_and(|(address, _)| address & !host_mask(self.prefix) == self.network)
    }
}

/// The bits after the first `prefix` bits.
fn host_mask(prefix: u32) -> u128 {
    u128::MAX.checked_shr(prefix).unwrap_or(0)
}

/// `inet_pton`, IPv4 first (mapped into IPv6). Returns the address and
/// whether it was IPv4.
fn parse_ip(text: &str) -> Option<(u128, bool)> {
    if let Ok(v4) = text.parse::<Ipv4Addr>() {
        return Some((u128::from(v4.to_ipv6_mapped()), true));
    }
    text.parse::<Ipv6Addr>()
        .ok()
        .map(|v6| (u128::from(v6), false))
}

fn parse_long(text: &str) -> Result<i64, String> {
    parse_integer(text)
        .ok_or_else(|| format!("can't convert '{}' to an integer", preview_str(text)))
}

/// The outcome of compiling a pattern: the pattern or the error message.
type Compiled<T> = Result<Arc<T>, String>;

/// Caches the outcome of compiling the most recent pattern for a computed
/// pattern argument, so `match(host.vars.pattern, …)` compiles once per
/// distinct pattern in a row of objects rather than once per object. Failed
/// compiles are cached too: an invalid pattern can be slow to reject.
#[derive(Debug)]
pub(crate) struct PatternCache<T> {
    last: Mutex<Option<(Box<str>, Compiled<T>)>>,
}

impl<T> Default for PatternCache<T> {
    fn default() -> Self {
        PatternCache {
            last: Mutex::new(None),
        }
    }
}

/// A clone starts with an empty cache.
impl<T> Clone for PatternCache<T> {
    fn clone(&self) -> Self {
        PatternCache::default()
    }
}

impl<T> PatternCache<T> {
    /// The compiled pattern (or why it doesn't compile), from the cache or
    /// freshly compiled.
    pub(crate) fn get_or_compile(
        &self,
        pattern: &str,
        compile: impl FnOnce(&str) -> Result<T, String>,
    ) -> Compiled<T> {
        if let Ok(last) = self.last.lock()
            && let Some((cached, outcome)) = last.as_ref()
            && **cached == *pattern
        {
            return outcome.clone();
        }
        let outcome = compile(pattern).map(Arc::new);
        if let Ok(mut last) = self.last.lock() {
            *last = Some((pattern.into(), outcome.clone()));
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str, text: &str) -> bool {
        Glob::new(pattern).unwrap().is_match(text)
    }

    #[test]
    fn glob_is_icingas_match() {
        assert!(glob("PG_*", "pg_main"));
        assert!(glob("a\\*", "a*"));
        assert!(!glob("a\\*", "ab"));
        assert!(glob("??", "ä"));
        assert!(!glob("ä*", "Ä"));
    }

    #[test]
    fn regex_semantics() {
        let regex = |pattern: &str, text: &str| IcingaRegex::new(pattern).unwrap().is_match(text);
        assert!(regex("^Hello", "Hello World"));
        assert!(regex("^Linux", "Linux/Unix"));
        assert!(!regex("^Linux$", "Linux/Unix"));
        assert!(regex("^db-prod\\d+", "db-prod1"));
        assert!(regex("prod", "db-prod1"), "searches anywhere");
        assert!(
            regex("^OK", "WARNING\nOK - fine"),
            "^ matches after line breaks"
        );
        assert!(regex("fine$", "OK - fine\r\nmore"), "$ matches before CRLF");
        assert!(regex("a.b", "a\nb"), ". matches line breaks");
        assert!(!regex("(?i)GRÖSSE", "größe"), "case folding is ASCII only");
        assert!(regex("(?i)GR", "größe"));
        assert!(
            regex("[ö]", "ö") && regex("[ö]", "\u{c3}"),
            "classes are byte sets"
        );
        assert!(!regex("^.$", "ö"), ". is one byte");
        assert!(regex("^..$", "ö"));
        assert!(regex("^\\ö$", "ö"));
        assert!(regex("\\w+", "abc_1"));
        assert!(!regex("^\\w+$", "ä"), "\\w is ASCII only");
        assert!(regex("^$", ""));
        for invalid in ["(", "a(?=b)", "(a)\\1", "[z-a]"] {
            let error = IcingaRegex::new(invalid).unwrap_err();
            assert!(error.starts_with("invalid regular expression: "), "{error}");
            assert!(!error.contains('\n'), "{error}");
        }
        assert!(
            IcingaRegex::new("a(?=b)")
                .unwrap_err()
                .contains("look-around")
        );
    }

    #[test]
    fn cidr_semantics() {
        let cidr = |pattern: &str, ip: &str| Cidr::new(pattern).unwrap().contains(ip);
        assert!(cidr("192.168.56.0/24", "192.168.56.101"));
        assert!(!cidr("192.168.56.0/26", "192.168.56.101"));
        assert!(cidr("10.0.0.0/8", "10.255.0.1"));
        assert!(!cidr("10.0.0.0/8", "11.0.0.1"));
        assert!(cidr("10.0.0.1", "10.0.0.1"), "no prefix: exact");
        assert!(!cidr("10.0.0.1", "10.0.0.2"));
        assert!(cidr("0.0.0.0/0", "1.2.3.4"));
        assert!(!cidr("0.0.0.0/0", "::1"), "IPv4 /0 is ::ffff:0:0/96");
        assert!(cidr("::ffff:0:0/96", "1.2.3.4"), "IPv4 is mapped into IPv6");
        assert!(cidr("10.0.0.0/8", "::ffff:10.1.2.3"));
        assert!(cidr("::/0", "fe80::1"));
        assert!(cidr("2001:db8::/32", "2001:db8:1::5"));
        assert!(!cidr("2001:db8::/32", "2001:db9::5"));
        assert!(cidr("fe80::/10", "febf::1"));
        assert!(!cidr("fe80::/10", "fec0::1"));
        assert!(!cidr("10.0.0.0/8", "not an ip"));
        assert!(!cidr("10.0.0.0/8", ""));
        assert!(
            !cidr("10.0.0.0/8", "010.0.0.1"),
            "leading zeros are rejected"
        );
        assert!(cidr("10.0.0.0/8", "10.0.0.1\0junk"), "C strings end at NUL");
        assert!(cidr("10.0.0.0/+8", "10.0.0.1"));

        let errors = [
            ("192.168.56.1/24", "masked-off bits"),
            ("10.0.0.0/33", "between 0 and 32"),
            ("10.0.0.0/-1", "between 0 and 32"),
            ("::/129", "between 0 and 128"),
            ("::/-1", "between 0 and 128"),
            ("10.0.0.0/x", "can't convert 'x'"),
            ("10.0.0.0/", "can't convert ''"),
            ("10.0.0.0/ 8", "can't convert ' 8'"),
            ("10.0.0.0/99999999999999999999", "can't convert"),
            ("", "invalid IP address ''"),
            ("hostname/8", "invalid IP address 'hostname'"),
        ];
        for (pattern, message) in errors {
            let error = Cidr::new(pattern).unwrap_err();
            assert!(error.contains(message), "{pattern}: {error}");
        }
    }

    #[test]
    fn dynamic_patterns_are_cached() {
        let cache: PatternCache<Glob> = PatternCache::default();
        let mut compiled = 0;
        for pattern in ["a*", "a*", "b*", "b*", "a*"] {
            cache
                .get_or_compile(pattern, |pattern| {
                    compiled += 1;
                    Glob::new(pattern)
                })
                .unwrap();
        }
        assert_eq!(compiled, 3);
        let failing: PatternCache<Cidr> = PatternCache::default();
        assert!(failing.get_or_compile("x", Cidr::new).is_err());
        assert!(failing.clone().get_or_compile("::/0", Cidr::new).is_ok());
    }

    #[test]
    fn failed_compiles_are_cached_too() {
        let cache: PatternCache<IcingaRegex> = PatternCache::default();
        let mut compiled = 0;
        let mut errors = Vec::new();
        for pattern in ["(", "(", "(", "a", "("] {
            let outcome = cache.get_or_compile(pattern, |pattern| {
                compiled += 1;
                IcingaRegex::new(pattern)
            });
            errors.push(outcome.err());
        }
        assert_eq!(compiled, 3, "\"(\" twice, \"a\", \"(\" again");
        assert!(
            errors[0]
                .as_deref()
                .is_some_and(|error| error.contains("invalid regular expression"))
        );
        assert_eq!(errors[1], errors[0], "the cached error");
        assert_eq!(errors[3], None);
    }

    #[test]
    fn concurrent_use_of_one_cache() {
        let cache: PatternCache<Glob> = PatternCache::default();
        std::thread::scope(|scope| {
            for thread in 0..4 {
                let cache = &cache;
                scope.spawn(move || {
                    for round in 0..2_000 {
                        let pattern = if (round + thread) % 2 == 0 {
                            "a*"
                        } else {
                            "b*"
                        };
                        let glob = cache.get_or_compile(pattern, Glob::new).unwrap();
                        assert_eq!(glob.is_match("abc"), pattern == "a*");
                    }
                });
            }
        });
    }
}
