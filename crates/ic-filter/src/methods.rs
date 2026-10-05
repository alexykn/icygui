//! Methods on values (`"text".lower()`, `host.groups.len()`), following the
//! prototypes in `lib/base/{string,array,dictionary,number,boolean}-script.cpp`.
//!
//! Strings are handled as bytes, like Icinga's `std::string`: lengths and
//! positions count bytes. A result that would split a UTF-8 character
//! replaces the broken bytes with U+FFFD.
//!
//! Unlike Icinga, a method called on `null` (a missing variable or key) is
//! not an error: `null` counts as an empty value that contains nothing (see
//! [`on_null`]). Dashboard filters such as
//! `!host.vars.tags.contains("prod")` then also match hosts without `tags`.

use std::collections::BTreeMap;

use crate::budget::Budget;
use crate::ops;
use crate::value::{Value, preview_str};

/// A method resolved for a receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    StringLen,
    StringContains,
    StringFind,
    StringLower,
    StringUpper,
    StringTrim,
    StringSplit,
    StringSubstr,
    StringStartsWith,
    StringEndsWith,
    StringReplace,
    ArrayLen,
    ArrayContains,
    ArrayJoin,
    DictLen,
    DictContains,
    DictGet,
    DictKeys,
    DictValues,
    ToString,
}

const STRING_METHODS: [(&str, Method); 12] = [
    ("contains", Method::StringContains),
    ("ends_with", Method::StringEndsWith),
    ("find", Method::StringFind),
    ("len", Method::StringLen),
    ("lower", Method::StringLower),
    ("replace", Method::StringReplace),
    ("split", Method::StringSplit),
    ("starts_with", Method::StringStartsWith),
    ("substr", Method::StringSubstr),
    ("to_string", Method::ToString),
    ("trim", Method::StringTrim),
    ("upper", Method::StringUpper),
];

const ARRAY_METHODS: [(&str, Method); 4] = [
    ("contains", Method::ArrayContains),
    ("join", Method::ArrayJoin),
    ("len", Method::ArrayLen),
    ("to_string", Method::ToString),
];

const DICT_METHODS: [(&str, Method); 6] = [
    ("contains", Method::DictContains),
    ("get", Method::DictGet),
    ("keys", Method::DictKeys),
    ("len", Method::DictLen),
    ("to_string", Method::ToString),
    ("values", Method::DictValues),
];

const SCALAR_METHODS: [(&str, Method); 1] = [("to_string", Method::ToString)];

/// Finds the method `name` for `receiver`. As in Icinga, a dictionary key
/// of the same name shadows the method. `null` has every method of strings,
/// arrays and dictionaries.
pub(crate) fn resolve(receiver: &Value, name: &str) -> Result<Method, String> {
    let methods: &[(&str, Method)] = match receiver {
        Value::Null => {
            return find(&STRING_METHODS, name)
                .or_else(|| find(&ARRAY_METHODS, name))
                .or_else(|| find(&DICT_METHODS, name))
                .ok_or_else(|| {
                    format!(
                        "unknown method '{}' (called on null; strings, arrays and \
                         dictionaries have: contains, ends_with, find, get, join, keys, len, \
                         lower, replace, split, starts_with, substr, to_string, trim, upper, \
                         values)",
                        preview_str(name)
                    )
                });
        }
        Value::Dict(entries) if entries.contains_key(name) => {
            return Err(format!(
                "'{}' is a key of the dictionary, not a method",
                preview_str(name)
            ));
        }
        Value::String(_) => &STRING_METHODS,
        Value::Array(_) => &ARRAY_METHODS,
        Value::Dict(_) => &DICT_METHODS,
        Value::Bool(_) | Value::Number(_) => &SCALAR_METHODS,
    };
    find(methods, name).ok_or_else(|| {
        let available: Vec<&str> = methods
            .iter()
            .map(|(method_name, _)| *method_name)
            .collect();
        format!(
            "unknown method '{}' for type '{}' (available: {})",
            preview_str(name),
            receiver.type_name(),
            available.join(", ")
        )
    })
}

fn find(methods: &[(&str, Method)], name: &str) -> Option<Method> {
    methods
        .iter()
        .find(|(method_name, _)| *method_name == name)
        .map(|(_, method)| *method)
}

/// Calls a resolved method. What it creates is charged to `budget`.
pub(crate) fn invoke(
    method: Method,
    receiver: &Value,
    args: &[Value],
    budget: &Budget,
) -> Result<Value, String> {
    match (method, receiver) {
        (_, Value::Null) => on_null(method, args),
        (Method::ToString, value) => match value {
            Value::String(_) => Ok(value.clone()),
            other => Ok(Value::from(budget.text_of(other)?.into_owned())),
        },
        (Method::StringLen | Method::ArrayLen | Method::DictLen, value) => {
            let length = match value {
                Value::String(text) => text.len(),
                Value::Array(items) => items.len(),
                Value::Dict(entries) => entries.len(),
                _ => 0,
            };
            Ok(count(length))
        }
        (_, Value::String(text)) => string_method(method, receiver, text, args, budget),
        (Method::ArrayContains, Value::Array(items)) => {
            let [value] = exact::<1>("contains", args)?;
            Ok(Value::Bool(ops::contains(items, value)))
        }
        (Method::ArrayJoin, Value::Array(items)) => {
            let [separator] = exact::<1>("join", args)?;
            join(items, separator, budget)
        }
        (Method::DictContains, Value::Dict(entries)) => {
            let [key] = exact::<1>("contains", args)?;
            Ok(Value::Bool(
                entries.contains_key(budget.text_of(key)?.as_ref()),
            ))
        }
        (Method::DictGet, Value::Dict(entries)) => {
            let [key] = exact::<1>("get", args)?;
            Ok(entries
                .get(budget.text_of(key)?.as_ref())
                .cloned()
                .unwrap_or_default())
        }
        (Method::DictKeys, Value::Dict(entries)) => keys(entries, budget),
        (Method::DictValues, Value::Dict(entries)) => {
            budget.items(entries.len())?;
            Ok(entries.values().cloned().collect())
        }
        _ => Err(format!(
            "method {method:?} doesn't apply to type '{}'",
            receiver.type_name()
        )),
    }
}

/// A length or position as a number.
fn count(length: usize) -> Value {
    #[expect(
        clippy::cast_precision_loss,
        reason = "lengths far below 2^53 are exact"
    )]
    Value::Number(length as f64)
}

/// The keys of a dictionary, as an array of strings.
pub(crate) fn keys(entries: &BTreeMap<String, Value>, budget: &Budget) -> Result<Value, String> {
    budget.items(entries.len())?;
    budget.text(entries.keys().map(String::len).sum())?;
    Ok(entries.keys().map(|key| Value::str(key)).collect())
}

/// A method called on `null`: `null` is an empty value that contains
/// nothing. The arguments are checked as usual.
///
/// | method | result |
/// |---|---|
/// | `contains`, `starts_with`, `ends_with` | `false` |
/// | `len` | `0` |
/// | `find` | `-1` |
/// | `lower`, `upper`, `trim`, `substr`, `replace`, `to_string` | `""` |
/// | `split`, `keys`, `values` | `[]` |
/// | `join`, `get` | `null` |
fn on_null(method: Method, args: &[Value]) -> Result<Value, String> {
    Ok(match method {
        Method::StringContains | Method::ArrayContains | Method::DictContains => {
            exact::<1>("contains", args)?;
            Value::Bool(false)
        }
        Method::StringStartsWith => {
            exact::<1>("starts_with", args)?;
            Value::Bool(false)
        }
        Method::StringEndsWith => {
            exact::<1>("ends_with", args)?;
            Value::Bool(false)
        }
        Method::StringLen | Method::ArrayLen | Method::DictLen => count(0),
        Method::StringFind => {
            at_least_one("find", args)?;
            if let Some(start) = args.get(1)
                && ops::to_number(start)? < 0.0
            {
                return Err("string index is out of range".to_owned());
            }
            Value::Number(-1.0)
        }
        Method::StringSubstr => {
            ops::to_number(at_least_one("substr", args)?)?;
            if let Some(length) = args.get(1) {
                ops::to_number(length)?;
            }
            Value::str("")
        }
        Method::StringReplace => {
            exact::<2>("replace", args)?;
            Value::str("")
        }
        Method::StringSplit => {
            exact::<1>("split", args)?;
            Value::array(Vec::new())
        }
        Method::ArrayJoin => {
            exact::<1>("join", args)?;
            Value::Null
        }
        Method::DictGet => {
            exact::<1>("get", args)?;
            Value::Null
        }
        Method::DictKeys | Method::DictValues => Value::array(Vec::new()),
        Method::StringLower | Method::StringUpper | Method::StringTrim | Method::ToString => {
            Value::str("")
        }
    })
}

/// Checks that exactly `N` arguments were given.
fn exact<'a, const N: usize>(name: &str, args: &'a [Value]) -> Result<&'a [Value; N], String> {
    args.try_into().map_err(|_| {
        format!(
            "{name}() takes exactly {N} argument{} ({} given)",
            if N == 1 { "" } else { "s" },
            args.len()
        )
    })
}

/// The first argument of a method with optional ones.
fn at_least_one<'a>(name: &str, args: &'a [Value]) -> Result<&'a Value, String> {
    args.first()
        .ok_or_else(|| format!("{name}() needs at least 1 argument"))
}

/// `Array#join`, which adds the items and separators with `+`: numbers add
/// up, anything with a string concatenates, and an empty array gives `null`.
///
/// Once the result is a string, adding `null`, a number or a string appends
/// to it in place, so joining takes linear rather than quadratic time.
fn join(items: &[Value], separator: &Value, budget: &Budget) -> Result<Value, String> {
    let mut joined = Joined::Value(Value::Null);
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            joined.add(separator, budget)?;
        }
        joined.add(item, budget)?;
    }
    Ok(match joined {
        Joined::Text(text) => Value::from(text),
        Joined::Value(value) => value,
    })
}

/// The running result of [`join`].
enum Joined {
    /// A string, kept growable.
    Text(String),
    /// Anything else.
    Value(Value),
}

impl Joined {
    /// `self = self + rhs`.
    fn add(&mut self, rhs: &Value, budget: &Budget) -> Result<(), String> {
        if let Joined::Text(text) = self
            && ops::is_text_operand(rhs)
        {
            // String + null/number/string concatenates.
            let more = ops::operand_text(rhs);
            budget.text(more.len())?;
            text.push_str(&more);
            return Ok(());
        }
        let lhs = match std::mem::replace(self, Joined::Value(Value::Null)) {
            Joined::Text(text) => Value::from(text),
            Joined::Value(value) => value,
        };
        *self = match ops::add(&lhs, rhs, budget)? {
            Value::String(text) => Joined::Text(text.to_string()),
            other => Joined::Value(other),
        };
        Ok(())
    }
}

fn string_method(
    method: Method,
    receiver: &Value,
    text_value: &str,
    args: &[Value],
    budget: &Budget,
) -> Result<Value, String> {
    match method {
        Method::StringContains => {
            let [needle] = exact::<1>("contains", args)?;
            Ok(Value::Bool(
                text_value.contains(budget.text_of(needle)?.as_ref()),
            ))
        }
        Method::StringStartsWith => {
            let [prefix] = exact::<1>("starts_with", args)?;
            Ok(Value::Bool(
                text_value.starts_with(budget.text_of(prefix)?.as_ref()),
            ))
        }
        Method::StringEndsWith => {
            let [suffix] = exact::<1>("ends_with", args)?;
            Ok(Value::Bool(
                text_value.ends_with(budget.text_of(suffix)?.as_ref()),
            ))
        }
        Method::StringFind => string_find(text_value, args, budget),
        Method::StringLower => {
            budget.text(text_value.len())?;
            Ok(Value::from(text_value.to_ascii_lowercase()))
        }
        Method::StringUpper => {
            budget.text(text_value.len())?;
            Ok(Value::from(text_value.to_ascii_uppercase()))
        }
        Method::StringTrim => {
            let trimmed = text_value.trim_matches(is_c_space);
            if trimmed.len() == text_value.len() {
                return Ok(receiver.clone());
            }
            budget.text(trimmed.len())?;
            Ok(Value::str(trimmed))
        }
        Method::StringSplit => string_split(text_value, args, budget),
        Method::StringSubstr => string_substr(text_value, args, budget),
        Method::StringReplace => string_replace(receiver, text_value, args, budget),
        _ => Err(format!("method {method:?} doesn't apply to type 'String'")),
    }
}

/// `String#find(str[, start])`: the byte position or -1.
fn string_find(text_value: &str, args: &[Value], budget: &Budget) -> Result<Value, String> {
    let needle = budget.text_of(at_least_one("find", args)?)?;
    let start = match args.get(1) {
        Some(start) => {
            let start = ops::to_number(start)?;
            if start < 0.0 {
                return Err("string index is out of range".to_owned());
            }
            byte_position(start)
        }
        None => 0,
    };
    Ok(find_bytes(text_value, &needle, start).map_or(Value::Number(-1.0), count))
}

/// `String#split(delims)`: splits at every byte that occurs in `delims`
/// (`boost::is_any_of`), keeping empty parts.
fn string_split(text_value: &str, args: &[Value], budget: &Budget) -> Result<Value, String> {
    let [delimiters] = exact::<1>("split", args)?;
    let delimiters = budget.text_of(delimiters)?;
    let mut is_delimiter = [false; 256];
    for &byte in delimiters.as_bytes() {
        is_delimiter[usize::from(byte)] = true;
    }
    let bytes = text_value.as_bytes();
    let separators = bytes
        .iter()
        .filter(|byte| is_delimiter[usize::from(**byte)])
        .count();
    budget.items(separators + 1)?;
    budget.text(bytes.len() - separators)?;
    Ok(bytes
        .split(|byte| is_delimiter[usize::from(*byte)])
        .map(|part| Value::from(String::from_utf8_lossy(part).into_owned()))
        .collect())
}

/// `String#substr(start[, len])` on bytes.
fn string_substr(text_value: &str, args: &[Value], budget: &Budget) -> Result<Value, String> {
    let bytes = text_value.as_bytes();
    let start = ops::to_number(at_least_one("substr", args)?)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "lengths far below 2^53 are exact"
    )]
    let length_f = bytes.len() as f64;
    if start.is_nan() || start < 0.0 || start >= length_f {
        return Err("string index is out of range".to_owned());
    }
    let start = byte_position(start);
    let count = match args.get(1) {
        Some(count) => {
            let count = ops::to_number(count)?;
            // A negative count converts to a huge size_t in C++: the rest of
            // the string.
            if count < 0.0 || count.is_nan() {
                usize::MAX
            } else {
                byte_position(count)
            }
        }
        None => usize::MAX,
    };
    let end = start.saturating_add(count).min(bytes.len());
    budget.text(end - start)?;
    Ok(Value::from(
        String::from_utf8_lossy(&bytes[start..end]).into_owned(),
    ))
}

/// `String#replace(search, replacement)`: every occurrence, left to right.
fn string_replace(
    receiver: &Value,
    text_value: &str,
    args: &[Value],
    budget: &Budget,
) -> Result<Value, String> {
    let [search, replacement] = exact::<2>("replace", args)?;
    let search = budget.text_of(search)?;
    if search.is_empty() {
        return Ok(receiver.clone());
    }
    let replacement = budget.text_of(replacement)?;
    let length = if replacement.len() <= search.len() {
        // The result is no longer than the text.
        text_value.len()
    } else {
        // Count matches only until the result is known to be too long, so
        // refusing a huge result is quick.
        let growth = replacement.len() - search.len();
        let left = budget.text_left();
        let mut length = text_value.len();
        for _ in text_value.matches(search.as_ref()) {
            length = length.saturating_add(growth);
            if length > left {
                break;
            }
        }
        length
    };
    budget.text(length)?;
    Ok(Value::from(
        text_value.replace(search.as_ref(), &replacement),
    ))
}

/// C's `isspace` in the "C" locale, which `boost::trim` uses.
fn is_c_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// A non-negative number as a byte position (truncated, saturating).
fn byte_position(number: f64) -> usize {
    if number.is_nan() || number >= 1.8e19 {
        return usize::MAX;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "non-negative and range checked; truncation is the intent"
    )]
    let position = number as usize;
    position
}

/// `std::string::find(needle, start)`: the byte position of the first
/// occurrence at or after `start`.
fn find_bytes(haystack: &str, needle: &str, start: usize) -> Option<usize> {
    if start > haystack.len() {
        return None;
    }
    if needle.is_empty() {
        return Some(start);
    }
    // A non-empty UTF-8 needle starts with a byte that begins a character,
    // so matches lie on character boundaries; `str::find` searches in
    // linear time.
    let mut from = start;
    while !haystack.is_char_boundary(from) {
        from += 1;
    }
    haystack[from..].find(needle).map(|index| index + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Value {
        Value::from(text)
    }

    fn n(value: f64) -> Value {
        Value::Number(value)
    }

    fn call(receiver: &Value, name: &str, args: &[Value]) -> Result<Value, String> {
        invoke(resolve(receiver, name)?, receiver, args, &Budget::new())
    }

    #[test]
    fn string_methods() {
        let cases: Vec<(&str, &str, Vec<Value>, Value)> = vec![
            ("Linux/Unix", "len", vec![], n(10.0)),
            ("äb", "len", vec![], n(3.0)),
            ("abc", "len", vec![n(1.0)], n(3.0)),
            ("abc", "contains", vec![s("bc")], Value::Bool(true)),
            ("abc", "contains", vec![s("x")], Value::Bool(false)),
            ("abc", "contains", vec![s("")], Value::Bool(true)),
            ("a1", "contains", vec![n(1.0)], Value::Bool(true)),
            ("abcabc", "find", vec![s("c")], n(2.0)),
            ("abcabc", "find", vec![s("c"), n(3.0)], n(5.0)),
            ("abcabc", "find", vec![s("x")], n(-1.0)),
            ("abc", "find", vec![s(""), n(3.0)], n(3.0)),
            ("abc", "find", vec![s(""), n(4.0)], n(-1.0)),
            ("abc", "find", vec![s("a"), n(99.0)], n(-1.0)),
            ("äb", "find", vec![s("b")], n(2.0)),
            ("MiXeD Ä", "lower", vec![], s("mixed Ä")),
            ("MiXeD ä", "upper", vec![], s("MIXED ä")),
            (" \t\n\u{b}\u{c}\rpadded \n", "trim", vec![], s("padded")),
            ("\u{a0}x\u{a0}", "trim", vec![], s("\u{a0}x\u{a0}")),
            (
                "a,b,,c",
                "split",
                vec![s(",")],
                Value::from(vec![s("a"), s("b"), s(""), s("c")]),
            ),
            (
                "a, b",
                "split",
                vec![s(", ")],
                Value::from(vec![s("a"), s(""), s("b")]),
            ),
            ("abc", "split", vec![s("")], Value::from(vec![s("abc")])),
            ("", "split", vec![s(",")], Value::from(vec![s("")])),
            ("hello", "substr", vec![n(1.0)], s("ello")),
            ("hello", "substr", vec![n(1.0), n(3.0)], s("ell")),
            ("hello", "substr", vec![n(1.9), n(2.9)], s("el")),
            ("hello", "substr", vec![n(4.0), n(10.0)], s("o")),
            ("hello", "substr", vec![n(1.0), n(-1.0)], s("ello")),
            ("hello", "substr", vec![s("2")], s("llo")),
            ("äb", "substr", vec![n(1.0)], s("\u{fffd}b")),
            ("pg_main", "starts_with", vec![s("pg_")], Value::Bool(true)),
            ("pg_main", "starts_with", vec![s("PG_")], Value::Bool(false)),
            (
                "db.example.com",
                "ends_with",
                vec![s(".com")],
                Value::Bool(true),
            ),
            ("a-b-c", "replace", vec![s("-"), s("+")], s("a+b+c")),
            ("aaa", "replace", vec![s("aa"), s("b")], s("ba")),
            ("abc", "replace", vec![s(""), s("x")], s("abc")),
            ("abc", "to_string", vec![], s("abc")),
        ];
        for (receiver, name, args, expected) in cases {
            assert_eq!(
                call(&s(receiver), name, &args).unwrap(),
                expected,
                "{receiver:?}.{name}({args:?})"
            );
        }
    }

    #[test]
    fn string_method_errors() {
        let cases: Vec<(&str, &str, Vec<Value>, &str)> = vec![
            (
                "abc",
                "contains",
                vec![],
                "contains() takes exactly 1 argument (0 given)",
            ),
            (
                "abc",
                "contains",
                vec![s("a"), s("b")],
                "takes exactly 1 argument (2 given)",
            ),
            (
                "abc",
                "replace",
                vec![s("a")],
                "replace() takes exactly 2 arguments (1 given)",
            ),
            ("abc", "find", vec![], "find() needs at least 1 argument"),
            (
                "abc",
                "find",
                vec![s("a"), n(-1.0)],
                "string index is out of range",
            ),
            (
                "abc",
                "substr",
                vec![],
                "substr() needs at least 1 argument",
            ),
            (
                "abc",
                "substr",
                vec![n(3.0)],
                "string index is out of range",
            ),
            (
                "abc",
                "substr",
                vec![n(-1.0)],
                "string index is out of range",
            ),
            (
                "abc",
                "substr",
                vec![n(f64::NAN)],
                "string index is out of range",
            ),
            ("", "substr", vec![n(0.0)], "string index is out of range"),
            ("abc", "substr", vec![s("x")], "can't convert 'x'"),
            (
                "abc",
                "nope",
                vec![],
                "unknown method 'nope' for type 'String' (available: contains, ends_with",
            ),
        ];
        for (receiver, name, args, message) in cases {
            let error = call(&s(receiver), name, &args).unwrap_err();
            assert!(
                error.contains(message),
                "{receiver:?}.{name}({args:?}): {error}"
            );
        }
    }

    #[test]
    fn array_methods() {
        let array = Value::from(vec![n(1.0), s("b"), Value::Null]);
        assert_eq!(call(&array, "len", &[]).unwrap(), n(3.0));
        assert_eq!(
            call(&array, "contains", &[s("b")]).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call(&array, "contains", &[Value::Bool(true)]).unwrap(),
            Value::Bool(true),
            "1 == true"
        );
        assert_eq!(
            call(&array, "contains", &[s("")]).unwrap(),
            Value::Bool(true),
            "null == \"\""
        );
        assert_eq!(
            call(&array, "contains", &[s("x")]).unwrap(),
            Value::Bool(false)
        );
        assert!(call(&array, "contains", &[]).is_err());
        let words = Value::from(vec![s("a"), s("b"), s("c")]);
        assert_eq!(call(&words, "join", &[s(", ")]).unwrap(), s("a, b, c"));
        assert_eq!(
            call(&Value::from(vec![n(1.0), n(2.0)]), "join", &[s(",")]).unwrap(),
            s("1,2")
        );
        assert_eq!(
            call(&Value::from(vec![n(1.0)]), "join", &[s(",")]).unwrap(),
            n(1.0),
            "a single number stays a number"
        );
        assert_eq!(
            call(&Value::from(vec![n(1.0), n(2.0)]), "join", &[n(0.0)]).unwrap(),
            n(3.0),
            "join adds with +"
        );
        assert_eq!(
            call(&Value::from(Vec::new()), "join", &[s(",")]).unwrap(),
            Value::Null
        );
        assert!(
            call(&Value::from(vec![Value::Null]), "join", &[s(",")]).is_err(),
            "null + null"
        );
        assert_eq!(
            call(&words, "to_string", &[]).unwrap(),
            s(r#"[ "a", "b", "c" ]"#)
        );
        assert!(
            call(&words, "lower", &[])
                .unwrap_err()
                .contains("unknown method 'lower' for type 'Array'")
        );
    }

    #[test]
    fn dictionary_methods() {
        let dict = Value::from(BTreeMap::from([
            ("b".to_owned(), n(2.0)),
            ("a".to_owned(), n(1.0)),
            ("1".to_owned(), s("one")),
        ]));
        assert_eq!(call(&dict, "len", &[]).unwrap(), n(3.0));
        assert_eq!(
            call(&dict, "contains", &[s("a")]).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call(&dict, "contains", &[n(1.0)]).unwrap(),
            Value::Bool(true),
            "keys are strings"
        );
        assert_eq!(
            call(&dict, "contains", &[s("z")]).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(call(&dict, "get", &[s("b")]).unwrap(), n(2.0));
        assert_eq!(call(&dict, "get", &[s("z")]).unwrap(), Value::Null);
        assert_eq!(
            call(&dict, "keys", &[]).unwrap(),
            Value::from(vec![s("1"), s("a"), s("b")])
        );
        assert_eq!(
            call(&dict, "values", &[]).unwrap(),
            Value::from(vec![s("one"), n(1.0), n(2.0)])
        );
        assert!(call(&dict, "get", &[]).is_err());
        let shadowing = Value::from(BTreeMap::from([("len".to_owned(), n(5.0))]));
        assert_eq!(
            call(&shadowing, "len", &[]).unwrap_err(),
            "'len' is a key of the dictionary, not a method"
        );
    }

    #[test]
    fn scalar_methods() {
        assert_eq!(call(&n(5.0), "to_string", &[]).unwrap(), s("5"));
        assert_eq!(call(&n(2.5), "to_string", &[]).unwrap(), s("2.500000"));
        assert_eq!(
            call(&Value::Bool(false), "to_string", &[]).unwrap(),
            s("false")
        );
        assert!(
            call(&n(5.0), "len", &[])
                .unwrap_err()
                .contains("type 'Number'")
        );
    }

    #[test]
    fn methods_on_null_treat_it_as_empty() {
        let null = Value::Null;
        let a = |items: &[Value]| Value::from(items.to_vec());
        let cases: Vec<(&str, Vec<Value>, Value)> = vec![
            ("contains", vec![s("x")], Value::Bool(false)),
            ("contains", vec![s("")], Value::Bool(false)),
            ("contains", vec![Value::Null], Value::Bool(false)),
            ("starts_with", vec![s("")], Value::Bool(false)),
            ("ends_with", vec![s("x")], Value::Bool(false)),
            ("len", vec![], n(0.0)),
            ("find", vec![s("x")], n(-1.0)),
            ("find", vec![s("x"), n(3.0)], n(-1.0)),
            ("lower", vec![], s("")),
            ("upper", vec![], s("")),
            ("trim", vec![], s("")),
            ("substr", vec![n(0.0)], s("")),
            ("substr", vec![n(5.0), n(2.0)], s("")),
            ("replace", vec![s("a"), s("b")], s("")),
            ("to_string", vec![], s("")),
            ("split", vec![s(",")], a(&[])),
            ("keys", vec![], a(&[])),
            ("values", vec![], a(&[])),
            ("join", vec![s(",")], Value::Null),
            ("get", vec![s("k")], Value::Null),
        ];
        for (name, args, expected) in cases {
            assert_eq!(
                call(&null, name, &args).unwrap(),
                expected,
                "null.{name}({args:?})"
            );
        }
        // Arguments are still checked, and unknown methods still fail.
        for (name, args, message) in [
            (
                "contains",
                vec![],
                "contains() takes exactly 1 argument (0 given)",
            ),
            (
                "replace",
                vec![s("a")],
                "replace() takes exactly 2 arguments",
            ),
            ("find", vec![], "find() needs at least 1 argument"),
            (
                "find",
                vec![s("x"), n(-1.0)],
                "string index is out of range",
            ),
            ("substr", vec![s("x")], "can't convert 'x'"),
            ("join", vec![], "join() takes exactly 1 argument"),
            ("nope", vec![], "unknown method 'nope' (called on null"),
        ] {
            let error = call(&null, name, &args).unwrap_err();
            assert!(error.contains(message), "null.{name}({args:?}): {error}");
        }
    }

    #[test]
    fn join_is_linear_and_keeps_icinga_semantics() {
        let a = |items: &[Value]| Value::from(items.to_vec());
        let d = |key: &str| Value::from(BTreeMap::from([(key.to_owned(), n(1.0))]));
        // An empty string followed by an array turns the result into an
        // array, as `"" + [1]` does.
        assert_eq!(
            call(&a(&[s(""), a(&[n(1.0)])]), "join", &[s("")]).unwrap(),
            a(&[n(1.0)])
        );
        assert_eq!(
            call(&a(&[s(""), d("k")]), "join", &[Value::Null]).unwrap(),
            d("k")
        );
        assert!(call(&a(&[s("x"), a(&[])]), "join", &[s("")]).is_err());
        assert!(call(&a(&[s("x"), Value::Bool(true)]), "join", &[s("")]).is_err());
        assert_eq!(
            call(&a(&[n(1.0), Value::Null, s("x")]), "join", &[n(2.0)]).unwrap(),
            s("5x"),
            "1 + 2 + null + 2 + \"x\""
        );
        assert_eq!(
            call(&a(&[a(&[n(1.0)]), a(&[n(2.0)])]), "join", &[a(&[])]).unwrap(),
            a(&[n(1.0), n(2.0)])
        );

        let numbers: Vec<Value> = (0..100_000).map(|index| n(f64::from(index))).collect();
        let start = std::time::Instant::now();
        let joined = call(&Value::from(numbers), "join", &[s(",")]).unwrap();
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        let text = joined.as_str().unwrap();
        assert!(text.starts_with("0,1,2,") && text.ends_with(",99999"));
    }

    #[test]
    fn methods_charge_what_they_create() {
        let budget = Budget::new();
        budget.text(crate::budget::MAX_TEXT - 10).unwrap();
        let call = |receiver: &Value, name: &str, args: &[Value]| {
            invoke(resolve(receiver, name)?, receiver, args, &budget)
        };
        let long = s(&"a".repeat(11));
        assert!(call(&long, "upper", &[]).is_err());
        assert!(call(&long, "replace", &[s("a"), s("b")]).is_err());
        assert_eq!(
            call(&long, "contains", &[s("aa")]).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call(&long, "trim", &[]).unwrap(),
            long,
            "nothing trimmed, nothing copied"
        );
        assert_eq!(call(&long, "replace", &[s(""), s("b")]).unwrap(), long);
        // 10 bytes left: two results of 5 bytes fit, a third doesn't.
        for _ in 0..2 {
            assert_eq!(
                call(&s("abc"), "replace", &[s("b"), s("xyz")]).unwrap(),
                s("axyzc")
            );
        }
        assert!(
            call(&s("abc"), "replace", &[s("b"), s("xyz")]).is_err(),
            "budget used up"
        );

        let budget = Budget::new();
        budget.items(crate::budget::MAX_ITEMS - 2).unwrap();
        let call = |receiver: &Value, name: &str, args: &[Value]| {
            invoke(resolve(receiver, name)?, receiver, args, &budget)
        };
        assert!(call(&s("a,b,c"), "split", &[s(",")]).is_err());
        assert_eq!(
            call(&s("a,b"), "split", &[s(",")]).unwrap(),
            Value::from(vec![s("a"), s("b")])
        );
    }

    #[test]
    fn replace_amplification_hits_the_limit() {
        let mut value = s("a");
        let mut steps = 0;
        let budget = Budget::new();
        let error = loop {
            match invoke(
                Method::StringReplace,
                &value,
                &[s("a"), s("aaaaaaaaaa")],
                &budget,
            ) {
                Ok(next) => value = next,
                Err(error) => break error,
            }
            steps += 1;
        };
        assert!(error.starts_with("evaluation limit reached"), "{error}");
        assert_eq!(steps, 7, "10^7 bytes fit into 16 MiB, 10^8 don't");
    }

    #[test]
    fn byte_helpers() {
        assert_eq!(find_bytes("abc", "c", 0), Some(2));
        assert_eq!(find_bytes("abc", "abcd", 0), None);
        assert_eq!(find_bytes("abc", "", 3), Some(3));
        assert_eq!(find_bytes("abc", "a", 4), None);
        assert_eq!(
            find_bytes("äbä", "ä", 1),
            Some(3),
            "from inside a character"
        );
        assert_eq!(find_bytes("äbä", "b", 1), Some(2));
        assert_eq!(find_bytes("äb", "", 1), Some(1));

        let haystack = format!("{}b", "a".repeat(1_000_000));
        let needle = format!("{}b", "a".repeat(10_000));
        let start = std::time::Instant::now();
        assert_eq!(find_bytes(&haystack, &needle, 0), Some(990_000));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(byte_position(2.9), 2);
        assert_eq!(byte_position(f64::NAN), usize::MAX);
        assert_eq!(byte_position(1e300), usize::MAX);
        assert!(is_c_space('\u{b}'));
        assert!(!is_c_space('\u{a0}'));
    }
}
