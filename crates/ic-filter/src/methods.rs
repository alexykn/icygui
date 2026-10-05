//! Methods on values (`"text".lower()`, `host.groups.len()`), following the
//! prototypes in `lib/base/{string,array,dictionary,number,boolean}-script.cpp`.
//!
//! Strings are handled as bytes, like Icinga's `std::string`: lengths and
//! positions count bytes. A result that would split a UTF-8 character
//! replaces the broken bytes with U+FFFD.

use crate::functions::text;
use crate::ops;
use crate::value::Value;

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
/// of the same name shadows the method, and methods can't be called on
/// `null`.
pub(crate) fn resolve(receiver: &Value, name: &str) -> Result<Method, String> {
    let methods: &[(&str, Method)] = match receiver {
        Value::Null => return Err(format!("cannot call method '{name}' on null")),
        Value::Dict(entries) if entries.contains_key(name) => {
            return Err(format!("'{name}' is a key of the dictionary, not a method"));
        }
        Value::String(_) => &STRING_METHODS,
        Value::Array(_) => &ARRAY_METHODS,
        Value::Dict(_) => &DICT_METHODS,
        Value::Bool(_) | Value::Number(_) => &SCALAR_METHODS,
    };
    methods
        .iter()
        .find(|(method_name, _)| *method_name == name)
        .map(|(_, method)| *method)
        .ok_or_else(|| {
            let available: Vec<&str> = methods
                .iter()
                .map(|(method_name, _)| *method_name)
                .collect();
            format!(
                "unknown method '{name}' for type '{}' (available: {})",
                receiver.type_name(),
                available.join(", ")
            )
        })
}

/// Calls a resolved method.
pub(crate) fn invoke(method: Method, receiver: &Value, args: &[Value]) -> Result<Value, String> {
    match (method, receiver) {
        (Method::ToString, value) => Ok(match value {
            Value::String(_) => value.clone(),
            other => Value::from(other.to_icinga_string()),
        }),
        (Method::StringLen | Method::ArrayLen | Method::DictLen, value) => {
            let length = match value {
                Value::String(text) => text.len(),
                Value::Array(items) => items.len(),
                Value::Dict(entries) => entries.len(),
                _ => 0,
            };
            #[expect(
                clippy::cast_precision_loss,
                reason = "lengths far below 2^53 are exact"
            )]
            Ok(Value::Number(length as f64))
        }
        (_, Value::String(text)) => string_method(method, text, args),
        (Method::ArrayContains, Value::Array(items)) => {
            let [value] = exact::<1>("contains", args)?;
            Ok(Value::Bool(ops::contains(items, value)))
        }
        (Method::ArrayJoin, Value::Array(items)) => {
            let [separator] = exact::<1>("join", args)?;
            // Array#join adds items with `+`: numbers add up, anything with
            // a string concatenates, and an empty array gives null.
            let mut result = Value::Null;
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    result = ops::add(&result, separator)?;
                }
                result = ops::add(&result, item)?;
            }
            Ok(result)
        }
        (Method::DictContains, Value::Dict(entries)) => {
            let [key] = exact::<1>("contains", args)?;
            Ok(Value::Bool(entries.contains_key(text(key).as_ref())))
        }
        (Method::DictGet, Value::Dict(entries)) => {
            let [key] = exact::<1>("get", args)?;
            Ok(entries.get(text(key).as_ref()).cloned().unwrap_or_default())
        }
        (Method::DictKeys, Value::Dict(entries)) => {
            Ok(entries.keys().map(|key| Value::str(key)).collect())
        }
        (Method::DictValues, Value::Dict(entries)) => Ok(entries.values().cloned().collect()),
        _ => Err(format!(
            "method {method:?} doesn't apply to type '{}'",
            receiver.type_name()
        )),
    }
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

fn string_method(method: Method, text_value: &str, args: &[Value]) -> Result<Value, String> {
    let bytes = text_value.as_bytes();
    match method {
        Method::StringContains => {
            let [needle] = exact::<1>("contains", args)?;
            Ok(Value::Bool(text_value.contains(text(needle).as_ref())))
        }
        Method::StringStartsWith => {
            let [prefix] = exact::<1>("starts_with", args)?;
            Ok(Value::Bool(text_value.starts_with(text(prefix).as_ref())))
        }
        Method::StringEndsWith => {
            let [suffix] = exact::<1>("ends_with", args)?;
            Ok(Value::Bool(text_value.ends_with(text(suffix).as_ref())))
        }
        Method::StringFind => {
            let needle = text(at_least_one("find", args)?).into_owned();
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
            #[expect(
                clippy::cast_precision_loss,
                reason = "positions far below 2^53 are exact"
            )]
            let found =
                find_bytes(bytes, needle.as_bytes(), start).map_or(-1.0, |index| index as f64);
            Ok(Value::Number(found))
        }
        Method::StringLower => Ok(Value::from(text_value.to_ascii_lowercase())),
        Method::StringUpper => Ok(Value::from(text_value.to_ascii_uppercase())),
        Method::StringTrim => Ok(Value::str(text_value.trim_matches(is_c_space))),
        Method::StringSplit => {
            let [delimiters] = exact::<1>("split", args)?;
            let delimiters = text(delimiters);
            let delimiters = delimiters.as_bytes();
            Ok(bytes
                .split(|byte| delimiters.contains(byte))
                .map(|part| Value::from(String::from_utf8_lossy(part).into_owned()))
                .collect())
        }
        Method::StringSubstr => {
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
                    // A negative count converts to a huge size_t in C++:
                    // the rest of the string.
                    if count < 0.0 || count.is_nan() {
                        usize::MAX
                    } else {
                        byte_position(count)
                    }
                }
                None => usize::MAX,
            };
            let end = start.saturating_add(count).min(bytes.len());
            Ok(Value::from(
                String::from_utf8_lossy(&bytes[start..end]).into_owned(),
            ))
        }
        Method::StringReplace => {
            let [search, replacement] = exact::<2>("replace", args)?;
            let search = text(search);
            if search.is_empty() {
                return Ok(Value::str(text_value));
            }
            Ok(Value::from(
                text_value.replace(search.as_ref(), &text(replacement)),
            ))
        }
        _ => Err(format!("method {method:?} doesn't apply to type 'String'")),
    }
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

/// `std::string::find(needle, start)` on bytes.
fn find_bytes(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if start > haystack.len() {
        return None;
    }
    if needle.is_empty() {
        return Some(start);
    }
    haystack[start..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|index| index + start)
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
        invoke(resolve(receiver, name)?, receiver, args)
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
        let dict = Value::from(std::collections::BTreeMap::from([
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
        let shadowing = Value::from(std::collections::BTreeMap::from([(
            "len".to_owned(),
            n(5.0),
        )]));
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
        assert_eq!(
            call(&Value::Null, "len", &[]).unwrap_err(),
            "cannot call method 'len' on null"
        );
    }

    #[test]
    fn byte_helpers() {
        assert_eq!(find_bytes(b"abc", b"c", 0), Some(2));
        assert_eq!(find_bytes(b"abc", b"abcd", 0), None);
        assert_eq!(find_bytes(b"abc", b"", 3), Some(3));
        assert_eq!(find_bytes(b"abc", b"a", 4), None);
        assert_eq!(byte_position(2.9), 2);
        assert_eq!(byte_position(f64::NAN), usize::MAX);
        assert_eq!(byte_position(1e300), usize::MAX);
        assert!(is_c_space('\u{b}'));
        assert!(!is_c_space('\u{a0}'));
    }
}
