//! Parser and evaluator for the subset of the Icinga 2 filter language used by
//! dashboards and notification rules (`host.vars.role == "db" && service.state != 0`).
//!
//! Filters are parsed once with [`Filter::parse`] and evaluated against a
//! [`Scope`] that resolves variables: [`HostScope`] and [`ServiceScope`] expose
//! `ic-model` objects under Icinga's attribute names, [`VarsScope`] provides API
//! `filter_vars`, and [`Chain`] combines scopes. The crate is pure: no I/O.
//!
//! # Language
//!
//! The grammar, operator precedence and value semantics follow Icinga 2's own
//! implementation (`config_lexer.ll`, `config_parser.yy`, `expression.cpp`,
//! `value-operators.cpp`), so a filter behaves here the way it does in an API
//! query, apart from the differences listed below:
//!
//! - literals: numbers, durations (`500ms`, `30s`, `5m`, `1h`, `2d`), strings
//!   with escapes, `{{{multi-line}}}` strings, `true`, `false`, `null`, arrays
//!   and dictionaries;
//! - operators, loosest first: `? :`, `||`, `&&`, `|`, `^`, `&`, `==` `!=`,
//!   `in` `!in`, `<` `<=` `>` `>=`, `<<` `>>`, `+` `-`, `*` `/` `%`, the prefix
//!   operators `!` `~` `-` `+`, and member access `.`, indexers `[…]` and calls;
//! - `&&` and `||` short-circuit and return one of their operands, as in Icinga;
//! - functions: `match`, `regex`, `cidr_match`, `len`, `typeof`, `string`,
//!   `number`, `bool`, `intersection`, `union`, `range`, `keys`, `get_time`,
//!   and the type conversions `String()`, `Number()` and `Boolean()`;
//! - methods on strings, arrays, dictionaries, numbers and booleans (see
//!   [`Filter`] for the list).
//!
//! Host and service attributes have Icinga's names and values. Note that, as
//! in Icinga, pending services have `state` 3 and pending hosts `state` 1;
//! `problem` is false for them.
//!
//! # Deliberate differences from Icinga
//!
//! - Unknown variables evaluate to `null` instead of failing, so a dashboard
//!   filter doesn't break on objects that lack a variable. Missing dictionary
//!   keys and out-of-range array indexes are `null` too.
//! - For the same reason, methods called on `null` don't fail: `null` counts
//!   as an empty value that contains nothing. `host.vars.tags.contains("x")`
//!   is false and `host.vars.tags.len()` is 0 for hosts without `tags`, so
//!   `!host.vars.tags.contains("x")` matches them.
//! - Assignments, loops, function definitions and other statements are
//!   rejected when parsing: filters are single expressions.
//! - Glob, regular expression and CIDR patterns given as literals are compiled
//!   while parsing, so an invalid literal pattern is a [`ParseError`] rather
//!   than an evaluation error.
//! - `regex()` uses the `regex` crate's syntax, which is Perl-like but has no
//!   look-around or back-references; patterns using them are rejected. Like
//!   Icinga's Boost.Regex it works on bytes, with `^`/`$` matching at line
//!   breaks and `.` matching newlines.
//! - Dictionaries compare equal when their contents are equal (Icinga compares
//!   object identity).
//! - Types (`typeof(x)` and the globals `Object`, `Boolean`, `Number`,
//!   `String`, `Array`, `Dictionary`) are dictionaries holding the type's
//!   `name`. `typeof(x) == Number` and `typeof(x).name == "Number"` work as in
//!   Icinga and `typeof(x) == "Number"` is false as there, but
//!   `string(typeof(x))` and `typeof(typeof(x))` describe a dictionary.
//! - `starts_with` and `ends_with` string methods are extensions.
//! - `range()` and the dictionary method `get()` work; Icinga's API refuses
//!   them in filters because they aren't marked safe for its sandbox.
//! - One evaluation may create at most 16 MiB of text and 100 000 array items
//!   and dictionary entries, and `range()` at most 10 000 numbers, so that no
//!   filter can exhaust memory (`.replace()` chains grow tenfold per call);
//!   an evaluation that needs more fails. Values read from objects and
//!   literals don't count.
//! - A glob with a non-ASCII character right after `*` (`*ü*`) matches as
//!   documented; x86 builds of Icinga never match it (a signedness bug).
//! - Where Icinga's behaviour is undefined (division of integers by zero
//!   after truncation, shifting by 32 or more, a null right side of an array
//!   `-`), the result is defined: an error, x86-style wrapping, and "remove
//!   nothing" respectively.
//! - Unknown attributes of hosts and services (`host.nonexistent`) are `null`
//!   rather than errors.
//! - An empty filter matches everything.
//!
//! # Icinga quirks that are kept
//!
//! These come from Icinga's lexer and grammar, so a filter parses here
//! exactly as in an API query. Parse errors explain them.
//!
//! - A line break ends the expression, except inside parentheses: wrap
//!   multi-line filters in `( … )`.
//! - `a == b == c` and `a < b < c` are syntax errors; `&` binds looser than
//!   `==`, so `x & 1 == 1` is `x & (1 == 1)`.
//! - `!in` is a token even when letters follow, so `!inactive` is `!in`
//!   followed by `active`; write `! inactive`.
//! - `<` followed by `>` with no space in between is an include path, so
//!   `a<b&&c>d` doesn't parse; put spaces around comparison operators.
//! - `}}` closes a function, so nested dictionaries need `} }`.
//! - Keywords (`default`, `in`, `true`, …) can't be member names; write
//!   `vars.@default` or `vars["default"]`.
//!
//! Filters can nest up to 64 levels, which keeps hostile input from
//! exhausting the stack.

mod ast;
mod budget;
mod eval;
mod functions;
mod lexer;
mod methods;
mod ops;
mod parser;
mod pattern;
mod scope;
mod value;

use std::fmt;
use std::str::FromStr;

use ic_model::Timestamp;

pub use scope::{Chain, HostScope, Scope, ServiceScope, VarsScope};
pub use value::Value;

/// A parsed filter expression.
///
/// Parse once, evaluate many times: literal glob, regex and CIDR patterns are
/// compiled while parsing. A `Filter` is cheap to clone and can be shared
/// between threads.
///
/// Supported methods (called as `value.method(args)`):
///
/// - strings: `contains`, `find`, `len`, `lower`, `upper`, `trim`, `split`,
///   `substr`, `starts_with`, `ends_with`, `replace`, `to_string`;
/// - arrays: `contains`, `len`, `join`, `to_string`;
/// - dictionaries: `contains`, `get`, `keys`, `values`, `len`, `to_string`;
/// - numbers and booleans: `to_string`;
/// - `null` (a missing variable or key): all of the above, treating `null`
///   as empty. `contains`, `starts_with` and `ends_with` are false, `len` is
///   0, `find` is -1, `split`, `keys` and `values` are `[]`, `join` and `get`
///   are `null`, and the other string methods return `""`.
///
/// Lengths and string positions count bytes, as in Icinga.
#[derive(Clone)]
pub struct Filter {
    source: String,
    root: Option<ast::Expr>,
}

impl Filter {
    /// Parses a filter. An empty source (only whitespace, line breaks and
    /// comments) is valid and matches everything.
    ///
    /// # Errors
    ///
    /// Returns a [`ParseError`] with the byte offset of the problem when the
    /// source is not a single valid expression, uses a statement (assignments,
    /// loops, function definitions, …), or contains an invalid literal
    /// pattern.
    pub fn parse(source: &str) -> Result<Filter, ParseError> {
        let root = parser::parse(source)?;
        Ok(Filter {
            source: source.to_owned(),
            root,
        })
    }

    /// The source text the filter was parsed from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether the filter has no expression and therefore matches everything.
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// Evaluates the filter against `scope`. An empty filter evaluates to
    /// `true`. `get_time()` reads the system clock.
    ///
    /// # Errors
    ///
    /// Returns an [`EvalError`] when an operator or function is applied to
    /// values it doesn't support (`true + 1`, `"a" in "b"`, `len()`), an
    /// unknown function or method is called, a dynamic pattern is invalid,
    /// or the evaluation would create more data than its limit allows.
    pub fn evaluate(&self, scope: &dyn Scope) -> Result<Value, EvalError> {
        self.run(scope, None)
    }

    /// Like [`Filter::evaluate`], with `get_time()` returning `now`, so that a
    /// batch of objects is evaluated against the same instant.
    ///
    /// # Errors
    ///
    /// The same as [`Filter::evaluate`].
    pub fn evaluate_at(&self, scope: &dyn Scope, now: Timestamp) -> Result<Value, EvalError> {
        self.run(scope, Some(now.as_unix_seconds()))
    }

    /// Whether the filter matches: evaluates it and takes the result's
    /// truthiness. Evaluation errors count as no match.
    pub fn matches(&self, scope: &dyn Scope) -> bool {
        self.evaluate(scope).is_ok_and(|value| value.is_truthy())
    }

    /// Like [`Filter::matches`], with `get_time()` returning `now`.
    pub fn matches_at(&self, scope: &dyn Scope, now: Timestamp) -> bool {
        self.evaluate_at(scope, now)
            .is_ok_and(|value| value.is_truthy())
    }

    fn run(&self, scope: &dyn Scope, now: Option<f64>) -> Result<Value, EvalError> {
        match &self.root {
            None => Ok(Value::Bool(true)),
            Some(root) => eval::Evaluator::new(scope, now, &self.source).eval(root),
        }
    }
}

impl fmt::Debug for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Filter")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Filter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

/// Filters are equal when they were parsed from the same source.
impl PartialEq for Filter {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for Filter {}

impl FromStr for Filter {
    type Err = ParseError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Filter::parse(source)
    }
}

/// A filter that could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message} (at byte {offset})")]
pub struct ParseError {
    /// What is wrong, for people.
    pub message: String,
    /// Byte offset into the source where the problem starts, for the editor's
    /// error marker. Equal to the source length for "unexpected end" errors.
    pub offset: usize,
}

impl ParseError {
    /// The 1-based line and column (in characters) of [`ParseError::offset`]
    /// within `source`, for showing the position to people.
    pub fn line_column(&self, source: &str) -> (usize, usize) {
        let mut end = self.offset.min(source.len());
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        let before = &source[..end];
        let line = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let column = before[line_start..].chars().count() + 1;
        (line, column)
    }
}

/// A filter that failed while being evaluated.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct EvalError {
    /// What went wrong, including the part of the filter that failed.
    pub message: String,
}

impl EvalError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        EvalError {
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_are_send_sync_and_cheap_to_share() {
        fn assert_send_sync<T: Send + Sync + Clone>() {}
        assert_send_sync::<Filter>();
        assert_send_sync::<Value>();
    }

    #[test]
    fn empty_filters_match_everything() {
        for source in [
            "",
            "   ",
            "\n\t\n",
            "# just a comment",
            "/* nothing */ // here",
        ] {
            let filter = Filter::parse(source).unwrap();
            assert!(filter.is_empty(), "{source:?}");
            assert_eq!(
                filter.evaluate(&Chain { scopes: &[] }).unwrap(),
                Value::Bool(true)
            );
            assert!(filter.matches(&Chain { scopes: &[] }));
        }
    }

    #[test]
    fn filter_round_trips_its_source() {
        let filter: Filter = "host.name == \"web\"".parse().unwrap();
        assert_eq!(filter.source(), "host.name == \"web\"");
        assert_eq!(filter.to_string(), "host.name == \"web\"");
        assert_eq!(filter, Filter::parse("host.name == \"web\"").unwrap());
        assert!(format!("{filter:?}").contains("host.name"));
    }

    #[test]
    fn parse_error_positions() {
        let source = "a ==\n  ä +";
        let error = Filter::parse(source).unwrap_err();
        assert_eq!(error.offset, 4);
        assert_eq!(error.line_column(source), (1, 5));
        let error = ParseError {
            message: String::new(),
            offset: 9,
        };
        assert_eq!(error.line_column(source), (2, 4));
        let error = ParseError {
            message: String::new(),
            offset: 8,
        };
        assert_eq!(
            error.line_column(source),
            (2, 3),
            "offsets inside a character snap back to its start"
        );
        assert!(
            ParseError {
                message: "boom".into(),
                offset: 3
            }
            .to_string()
            .contains("byte 3")
        );
    }
}
