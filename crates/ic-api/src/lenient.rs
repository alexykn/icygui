//! Forgiving JSON field types for the wire structs.
//!
//! Icinga writes whole numbers as JSON integers and others as floats, uses
//! `null` for unset values, and older versions lack some attributes. A
//! [`L<T>`] field accepts any JSON value and converts it as well as it can,
//! so one odd attribute never fails a whole response.

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// A field that accepts any JSON value; see [`FromJson`] for the rules.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct L<T>(pub(crate) T);

impl<'de, T: FromJson> Deserialize<'de> for L<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(|value| Self(T::from_json(value)))
    }
}

/// Conversion from an arbitrary JSON value, never failing.
pub(crate) trait FromJson: Sized {
    fn from_json(value: Value) -> Self;
}

/// A number from a JSON number, a numeric string or a boolean.
pub(crate) fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse::<f64>().ok(),
        Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
    .filter(|number| number.is_finite())
}

/// Missing, `null` or garbage is `0.0`.
impl FromJson for f64 {
    fn from_json(value: Value) -> Self {
        number(&value).unwrap_or(0.0)
    }
}

/// Missing, `null` or garbage is `None`.
impl FromJson for Option<f64> {
    fn from_json(value: Value) -> Self {
        number(&value)
    }
}

/// Strings as they are; numbers and booleans as text; anything else empty.
impl FromJson for String {
    fn from_json(value: Value) -> Self {
        match value {
            Value::String(text) => text,
            Value::Number(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            Value::Null | Value::Array(_) | Value::Object(_) => Self::new(),
        }
    }
}

/// Booleans, non-zero numbers, `"true"`/`"1"`; anything else is `None`.
impl FromJson for Option<bool> {
    fn from_json(value: Value) -> Self {
        match value {
            Value::Bool(flag) => Some(flag),
            Value::Number(number) => number.as_f64().map(|n| n != 0.0),
            Value::String(text) => match text.trim() {
                "true" | "1" => Some(true),
                "false" | "0" => Some(false),
                _ => None,
            },
            Value::Null | Value::Array(_) | Value::Object(_) => None,
        }
    }
}

/// Like `Option<bool>`, with `false` for anything unrecognised.
impl FromJson for bool {
    fn from_json(value: Value) -> Self {
        Option::<bool>::from_json(value).unwrap_or(false)
    }
}

/// Arrays of strings (other elements skipped); a lone string is a
/// one-element list; anything else is empty.
impl FromJson for Vec<String> {
    fn from_json(value: Value) -> Self {
        match value {
            Value::Array(items) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text),
                    _ => None,
                })
                .collect(),
            Value::String(text) => vec![text],
            _ => Vec::new(),
        }
    }
}

/// Objects as they are; anything else (`null` vars) is empty.
impl FromJson for serde_json::Map<String, Value> {
    fn from_json(value: Value) -> Self {
        match value {
            Value::Object(map) => map,
            _ => Self::new(),
        }
    }
}

/// Keeps the raw value; `null` and missing are `None`.
impl FromJson for Option<Value> {
    fn from_json(value: Value) -> Self {
        (!value.is_null()).then_some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Sample {
        a: L<f64>,
        b: L<Option<f64>>,
        c: L<String>,
        d: L<Option<bool>>,
        e: L<Vec<String>>,
        f: L<serde_json::Map<String, Value>>,
    }

    #[test]
    fn accepts_integers_floats_strings_and_nulls() {
        let sample: Sample = serde_json::from_str(
            r#"{"a": 2, "b": 1.5, "c": 3, "d": 1, "e": ["x", 1, "y"], "f": null}"#,
        )
        .unwrap();
        assert!((sample.a.0 - 2.0).abs() < f64::EPSILON);
        assert_eq!(sample.b.0, Some(1.5));
        assert_eq!(sample.c.0, "3");
        assert_eq!(sample.d.0, Some(true));
        assert_eq!(sample.e.0, ["x", "y"]);
        assert!(sample.f.0.is_empty());

        let sample: Sample = serde_json::from_str(
            r#"{"a": "4.5", "b": null, "c": null, "d": "nope", "e": "solo", "f": {"k": 1}}"#,
        )
        .unwrap();
        assert!((sample.a.0 - 4.5).abs() < f64::EPSILON);
        assert_eq!(sample.b.0, None);
        assert_eq!(sample.c.0, "");
        assert_eq!(sample.d.0, None);
        assert_eq!(sample.e.0, ["solo"]);
        assert_eq!(sample.f.0.len(), 1);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let sample: Sample = serde_json::from_str("{}").unwrap();
        assert!(sample.a.0.abs() < f64::EPSILON);
        assert_eq!(sample.b.0, None);
        assert_eq!(sample.d.0, None);
    }

    #[test]
    fn garbage_numbers_are_ignored() {
        assert_eq!(number(&Value::from("abc")), None);
        assert_eq!(number(&serde_json::json!([1])), None);
        assert_eq!(number(&Value::from(true)), Some(1.0));
    }
}
