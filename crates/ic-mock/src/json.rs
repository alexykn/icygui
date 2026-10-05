//! JSON encoding the way Icinga writes it.
//!
//! Icinga stores every number as a double. Older versions (and the API
//! documentation) write them with a fractional part (`200.0`); Icinga 2.13+
//! writes integral values without one (`200`). [`NumberFormat`] picks the
//! style; every response and event goes through [`encode`] so the choice is
//! applied consistently.

use serde_json::{Number, Value};

use crate::config::NumberFormat;

/// A JSON number from a double (`null` for NaN and infinities, which
/// Icinga's encoder also writes as `null`).
pub(crate) fn num(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// A JSON number from an integer, stored as Icinga stores it (a double).
pub(crate) fn int(value: impl Into<i64>) -> Value {
    #[expect(
        clippy::cast_precision_loss,
        reason = "Icinga stores integers as doubles; the mock's values are far below 2^53"
    )]
    num(value.into() as f64)
}

/// Rewrites every number in `value` to the given style and sorts object
/// keys (Icinga's dictionaries are ordered maps). Sorting matters when
/// another crate in the build enables `serde_json/preserve_order`, which
/// would otherwise keep insertion order.
pub(crate) fn normalize(value: &mut Value, format: NumberFormat) {
    match value {
        Value::Number(number) => {
            let Some(float) = number.as_f64() else {
                return;
            };
            *value = match format {
                NumberFormat::Float => num(float),
                NumberFormat::Integral => integral(float),
            };
        }
        Value::Array(items) => {
            for item in items {
                normalize(item, format);
            }
        }
        Value::Object(map) => {
            map.sort_keys();
            for item in map.values_mut() {
                normalize(item, format);
            }
        }
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

/// Icinga 2.13+'s `JsonEncoder::NumberFloat`: integral doubles are written
/// as integers.
fn integral(float: f64) -> Value {
    if float.fract() == 0.0 && float.abs() < 9.0e15 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "checked integral and within i64 range above"
        )]
        let int = float as i64;
        Value::Number(Number::from(int))
    } else {
        num(float)
    }
}

/// Serializes `value` with Icinga's number style, optionally pretty-printed
/// (Icinga indents with four spaces).
pub(crate) fn encode(mut value: Value, format: NumberFormat, pretty: bool) -> Vec<u8> {
    normalize(&mut value, format);
    if pretty {
        let mut out = Vec::new();
        let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
        let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
        if serde::Serialize::serialize(&value, &mut serializer).is_ok() {
            return out;
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| b"null".to_vec())
}

/// The start of a `{"results": [...]}` document written entry by entry.
pub(crate) const RESULTS_START: &[u8] = b"{\"results\":[";
/// The end of a `{"results": [...]}` document.
pub(crate) const RESULTS_END: &[u8] = b"]}";

/// Appends `entry` (normalized, compact) to `out`, after a comma unless it
/// is the first entry; `null` if it can't be written.
pub(crate) fn push_result(out: &mut Vec<u8>, mut entry: Value, format: NumberFormat, first: bool) {
    if !first {
        out.push(b',');
    }
    normalize(&mut entry, format);
    if serde_json::to_writer(&mut *out, &entry).is_err() {
        out.extend_from_slice(b"null");
    }
}

/// Serializes `{"results": [...]}` one entry at a time, so a large answer
/// never exists as a value tree and as text at once. Pretty output takes
/// the simple route.
pub(crate) fn encode_results<I>(entries: I, format: NumberFormat, pretty: bool) -> Vec<u8>
where
    I: IntoIterator<Item = Value>,
{
    if pretty {
        let mut body = serde_json::Map::new();
        body.insert(
            "results".into(),
            Value::Array(entries.into_iter().collect()),
        );
        return encode(Value::Object(body), format, true);
    }
    let mut out = RESULTS_START.to_vec();
    for (index, entry) in entries.into_iter().enumerate() {
        push_result(&mut out, entry, format, index == 0);
    }
    out.extend_from_slice(RESULTS_END);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn float_style_writes_fractions() {
        let body = encode(
            json!({ "code": 200, "list": [1, 2.5] }),
            NumberFormat::Float,
            false,
        );
        assert_eq!(
            String::from_utf8(body).unwrap(),
            r#"{"code":200.0,"list":[1.0,2.5]}"#
        );
    }

    #[test]
    fn integral_style_drops_fractions() {
        let body = encode(
            json!({ "code": 200.0, "t": 1_700_000_000.25, "n": -3.0 }),
            NumberFormat::Integral,
            false,
        );
        assert_eq!(
            String::from_utf8(body).unwrap(),
            r#"{"code":200,"n":-3,"t":1700000000.25}"#
        );
    }

    #[test]
    fn non_finite_numbers_become_null() {
        assert_eq!(num(f64::NAN), Value::Null);
        assert_eq!(int(5), json!(5.0));
    }

    #[test]
    fn keys_are_sorted_whatever_the_map_order() {
        let mut map = serde_json::Map::new();
        map.insert("type".into(), json!("x"));
        map.insert("b".into(), json!({ "z": 1, "a": 2 }));
        map.insert("a".into(), json!(1));
        let body = encode(Value::Object(map), NumberFormat::Integral, false);
        assert_eq!(
            String::from_utf8(body).unwrap(),
            r#"{"a":1,"b":{"a":2,"z":1},"type":"x"}"#
        );
    }

    #[test]
    fn results_stream_like_the_whole_document() {
        let entries = vec![json!({ "name": "a", "code": 1 }), json!({ "name": "b" })];
        for pretty in [false, true] {
            let streamed = encode_results(entries.clone(), NumberFormat::Float, pretty);
            let whole = encode(json!({ "results": entries }), NumberFormat::Float, pretty);
            assert_eq!(streamed, whole);
        }
        assert_eq!(
            encode_results(Vec::new(), NumberFormat::Float, false),
            b"{\"results\":[]}"
        );
    }

    #[test]
    fn pretty_uses_four_spaces() {
        let body = String::from_utf8(encode(json!({ "a": 1 }), NumberFormat::Float, true)).unwrap();
        assert_eq!(body, "{\n    \"a\": 1.0\n}");
    }
}
