//! Values of the filter language and Icinga's conversions between them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

/// A value in Icinga's filter language: what variables resolve to and what
/// expressions evaluate to.
///
/// Cloning is cheap: strings, arrays and dictionaries are shared.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    /// `null`, Icinga's empty value: unknown variables, missing keys, the
    /// check result of a pending object.
    #[default]
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number. Icinga has a single number type, a double; states, counters,
    /// durations and timestamps are all numbers.
    Number(f64),
    /// A string.
    String(Arc<str>),
    /// An array.
    Array(Arc<Vec<Value>>),
    /// A dictionary. Keys are kept in byte order, as Icinga keeps them.
    Dict(Arc<BTreeMap<String, Value>>),
}

impl Value {
    /// Icinga's truthiness: `null`, `false`, `0`, `""`, `[]` and `{}` are
    /// false, everything else (including `NaN`) is true.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(value) => *value,
            Value::Number(value) => *value != 0.0,
            Value::String(value) => !value.is_empty(),
            Value::Array(items) => !items.is_empty(),
            Value::Dict(entries) => !entries.is_empty(),
        }
    }

    /// Converts JSON (custom variables, API `filter_vars`) into a value,
    /// keeping numbers, strings, booleans, arrays and dictionaries.
    pub fn from_json(json: &serde_json::Value) -> Value {
        match json {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(value) => Value::Bool(*value),
            serde_json::Value::Number(number) => number.as_f64().map_or(Value::Null, Value::Number),
            serde_json::Value::String(text) => Value::String(Arc::from(text.as_str())),
            serde_json::Value::Array(items) => {
                Value::Array(Arc::new(items.iter().map(Value::from_json).collect()))
            }
            serde_json::Value::Object(entries) => Value::Dict(Arc::new(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::from_json(value)))
                    .collect(),
            )),
        }
    }

    /// Converts the value to JSON. Non-finite numbers become `null`, and
    /// whole numbers are written without a fraction.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(value) => serde_json::Value::Bool(*value),
            Value::Number(value) => number_to_json(*value),
            Value::String(text) => serde_json::Value::String(text.to_string()),
            Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(Value::to_json).collect())
            }
            Value::Dict(entries) => serde_json::Value::Object(
                entries
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_json()))
                    .collect(),
            ),
        }
    }

    /// Icinga's name for the value's type, as used in its error messages:
    /// `Empty` (for `null`), `Boolean`, `Number`, `String`, `Array` or
    /// `Dictionary`.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "Empty",
            Value::Bool(_) => "Boolean",
            Value::Number(_) => "Number",
            Value::String(_) => "String",
            Value::Array(_) => "Array",
            Value::Dict(_) => "Dictionary",
        }
    }

    /// Icinga's conversion to a string, as done by `string(value)`: `null` is
    /// `""`, whole numbers have no fraction (`5`), other numbers six decimals
    /// (`2.500000`), arrays and dictionaries are written in config syntax.
    pub fn to_icinga_string(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(value) => bool_str(*value).to_owned(),
            Value::Number(value) => format_number(*value),
            Value::String(text) => text.to_string(),
            Value::Array(items) => {
                let mut out = String::new();
                emit_array(&mut out, 1, items);
                out
            }
            Value::Dict(entries) => {
                let mut out = String::new();
                emit_dict(&mut out, 1, entries);
                out
            }
        }
    }

    /// Whether this is `null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The boolean, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// The number, if this is one.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(value) => Some(*value),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    /// The items, if this is an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The entries, if this is a dictionary.
    pub fn as_dict(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Dict(entries) => Some(entries),
            _ => None,
        }
    }

    /// Icinga's `IsEmpty()`: `null` or the empty string. Many operators treat
    /// both alike.
    pub(crate) fn is_empty_value(&self) -> bool {
        match self {
            Value::Null => true,
            Value::String(text) => text.is_empty(),
            _ => false,
        }
    }

    /// The value as compact JSON, for error messages.
    pub(crate) fn to_json_text(&self) -> String {
        self.to_json().to_string()
    }

    /// A string value.
    pub(crate) fn str(text: &str) -> Value {
        Value::String(Arc::from(text))
    }

    /// An array value.
    pub(crate) fn array(items: Vec<Value>) -> Value {
        Value::Array(Arc::new(items))
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Number(value)
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Value::Number(f64::from(value))
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Value::Number(f64::from(value))
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::String(Arc::from(value))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::String(Arc::from(value))
    }
}

impl From<Arc<str>> for Value {
    fn from(value: Arc<str>) -> Self {
        Value::String(value)
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Self {
        Value::Array(Arc::new(items))
    }
}

impl From<BTreeMap<String, Value>> for Value {
    fn from(entries: BTreeMap<String, Value>) -> Self {
        Value::Dict(Arc::new(entries))
    }
}

impl From<&serde_json::Value> for Value {
    fn from(json: &serde_json::Value) -> Self {
        Value::from_json(json)
    }
}

impl FromIterator<Value> for Value {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        Value::Array(Arc::new(iter.into_iter().collect()))
    }
}

fn bool_str(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn number_to_json(value: f64) -> serde_json::Value {
    if !value.is_finite() {
        return serde_json::Value::Null;
    }
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "whole number checked to be within the exactly representable range"
        )]
        return serde_json::Value::from(value as i64);
    }
    serde_json::Number::from_f64(value).map_or(serde_json::Value::Null, serde_json::Value::Number)
}

/// Icinga's `Convert::ToString(double)`: whole numbers without a fraction,
/// others with six decimals (C++ `std::fixed`).
pub(crate) fn format_number(value: f64) -> String {
    if value.is_nan() {
        return nan_str(value).to_owned();
    }
    if value.is_infinite() || value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.6}")
    }
}

/// How C's `printf` writes NaN.
fn nan_str(value: f64) -> &'static str {
    if value.is_sign_negative() {
        "-nan"
    } else {
        "nan"
    }
}

/// The keywords Icinga's config writer escapes with `@`.
const CONFIG_KEYWORDS: [&str; 38] = [
    "object",
    "template",
    "include",
    "include_recursive",
    "include_zones",
    "library",
    "null",
    "true",
    "false",
    "const",
    "var",
    "this",
    "globals",
    "locals",
    "use",
    "using",
    "namespace",
    "default",
    "ignore_on_error",
    "current_filename",
    "current_line",
    "apply",
    "to",
    "where",
    "import",
    "assign",
    "ignore",
    "function",
    "return",
    "break",
    "continue",
    "for",
    "if",
    "else",
    "while",
    "throw",
    "try",
    "except",
];

/// Icinga's `ConfigWriter::EmitValue`.
fn emit_value(out: &mut String, indent: usize, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(bool_str(*value)),
        Value::Number(value) => {
            if value.is_nan() {
                out.push_str(nan_str(*value));
            } else {
                let _ = write!(out, "{value:.6}");
            }
        }
        Value::String(text) => emit_string(out, text),
        Value::Array(items) => emit_array(out, indent, items),
        Value::Dict(entries) => emit_dict(out, indent, entries),
    }
}

fn emit_array(out: &mut String, indent: usize, items: &[Value]) {
    out.push_str("[ ");
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        emit_value(out, indent, item);
    }
    if !items.is_empty() {
        out.push(' ');
    }
    out.push(']');
}

fn emit_dict(out: &mut String, indent: usize, entries: &BTreeMap<String, Value>) {
    out.push('{');
    for (key, value) in entries {
        out.push('\n');
        push_tabs(out, indent);
        emit_identifier(out, key);
        out.push_str(" = ");
        emit_value(out, indent + 1, value);
    }
    out.push('\n');
    push_tabs(out, indent.saturating_sub(1));
    out.push('}');
}

fn push_tabs(out: &mut String, count: usize) {
    for _ in 0..count {
        out.push('\t');
    }
}

fn emit_identifier(out: &mut String, key: &str) {
    if CONFIG_KEYWORDS.contains(&key) {
        out.push('@');
        out.push_str(key);
    } else if is_identifier(key) {
        out.push_str(key);
    } else {
        emit_string(out, key);
    }
}

/// Whether `text` is a valid identifier: `[a-zA-Z_][a-zA-Z0-9_]*`.
pub(crate) fn is_identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn emit_string(out: &mut String, text: &str) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '"' => out.push_str("\\\""),
            other => out.push(other),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dict(entries: &[(&str, Value)]) -> Value {
        Value::from(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn truthiness_follows_icinga() {
        let cases = [
            (Value::Null, false),
            (Value::Bool(false), false),
            (Value::Bool(true), true),
            (Value::Number(0.0), false),
            (Value::Number(-0.0), false),
            (Value::Number(0.5), true),
            (Value::Number(f64::NAN), true),
            (Value::from(""), false),
            (Value::from("0"), true),
            (Value::from("false"), true),
            (Value::from(Vec::new()), false),
            (Value::from(vec![Value::Null]), true),
            (dict(&[]), false),
            (dict(&[("a", Value::Null)]), true),
        ];
        for (value, truthy) in cases {
            assert_eq!(value.is_truthy(), truthy, "{value:?}");
        }
    }

    #[test]
    fn json_conversion_keeps_types() {
        let json = json!({
            "role": "postgres",
            "port": 5432,
            "ratio": 0.75,
            "negative": -3,
            "enabled": true,
            "nothing": null,
            "disks": { "/": { "warn": "80%" }, "/var": {} },
            "tags": ["db", 1, false, null, [2]],
        });
        let value = Value::from_json(&json);
        let entries = value.as_dict().unwrap();
        assert_eq!(entries["role"], Value::from("postgres"));
        assert_eq!(entries["port"], Value::Number(5432.0));
        assert_eq!(entries["ratio"], Value::Number(0.75));
        assert_eq!(entries["negative"], Value::Number(-3.0));
        assert_eq!(entries["enabled"], Value::Bool(true));
        assert_eq!(entries["nothing"], Value::Null);
        assert_eq!(
            entries["tags"],
            Value::from(vec![
                Value::from("db"),
                Value::Number(1.0),
                Value::Bool(false),
                Value::Null,
                Value::from(vec![Value::Number(2.0)]),
            ])
        );
        let disks = entries["disks"].as_dict().unwrap();
        assert_eq!(
            disks.keys().collect::<Vec<_>>(),
            ["/", "/var"],
            "keys are sorted"
        );
        assert_eq!(value.to_json(), json, "round trip");
        assert_eq!(
            Value::from(&json!(u64::MAX)),
            Value::Number(1.844_674_407_370_955_2e19)
        );
    }

    #[test]
    fn to_json_writes_whole_numbers_without_fraction() {
        assert_eq!(Value::Number(2.0).to_json_text(), "2");
        assert_eq!(Value::Number(2.5).to_json_text(), "2.5");
        assert_eq!(Value::Number(f64::INFINITY).to_json_text(), "null");
        assert_eq!(Value::from("a\"b").to_json_text(), r#""a\"b""#);
    }

    #[test]
    fn icinga_string_conversion() {
        let cases = [
            (Value::Null, ""),
            (Value::Bool(true), "true"),
            (Value::Bool(false), "false"),
            (Value::Number(5.0), "5"),
            (Value::Number(-3.0), "-3"),
            (Value::Number(-0.0), "-0"),
            (Value::Number(2.5), "2.500000"),
            (Value::Number(0.1), "0.100000"),
            (Value::Number(1e20), "100000000000000000000"),
            (Value::Number(1.0 / 3.0), "0.333333"),
            (Value::Number(0.007_812_5), "0.007812"),
            (Value::Number(f64::INFINITY), "inf"),
            (Value::Number(f64::NEG_INFINITY), "-inf"),
            (Value::Number(f64::NAN), "nan"),
            (Value::from("text"), "text"),
            (Value::from(Vec::new()), "[ ]"),
        ];
        for (value, expected) in cases {
            assert_eq!(value.to_icinga_string(), expected, "{value:?}");
        }
    }

    #[test]
    fn arrays_and_dictionaries_convert_to_config_syntax() {
        // The examples from Icinga's library reference (`to_string`).
        let array = Value::from(vec![Value::from("dev"), Value::from("slack")]);
        assert_eq!(array.to_icinga_string(), r#"[ "dev", "slack" ]"#);
        let nested = dict(&[("/", dict(&[])), ("/var", dict(&[]))]);
        assert_eq!(
            nested.to_icinga_string(),
            "{\n\t\"/\" = {\n\t}\n\t\"/var\" = {\n\t}\n}"
        );
        let mixed = Value::from(vec![
            Value::Number(1.0),
            Value::Null,
            Value::Bool(true),
            Value::from("a\"\\\n\t\r\u{8}\u{c}"),
        ]);
        assert_eq!(
            mixed.to_icinga_string(),
            r#"[ 1.000000, null, true, "a\"\\\n\t\r\b\f" ]"#
        );
        let keys = dict(&[
            ("plain_1", Value::Number(1.0)),
            ("default", Value::Number(2.0)),
            ("with space", Value::Number(3.0)),
            ("nested", dict(&[("x", Value::from(vec![Value::Null]))])),
        ]);
        assert_eq!(
            keys.to_icinga_string(),
            "{\n\t@default = 2.000000\n\tnested = {\n\t\tx = [ null ]\n\t}\n\tplain_1 = 1.000000\n\t\"with space\" = 3.000000\n}"
        );
    }

    #[test]
    fn accessors_and_type_names() {
        assert_eq!(Value::Null.type_name(), "Empty");
        assert_eq!(Value::Bool(true).type_name(), "Boolean");
        assert_eq!(Value::Number(1.0).type_name(), "Number");
        assert_eq!(Value::from("x").type_name(), "String");
        assert_eq!(Value::from(Vec::new()).type_name(), "Array");
        assert_eq!(dict(&[]).type_name(), "Dictionary");
        assert!(Value::Null.is_null());
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::Number(2.0).as_number(), Some(2.0));
        assert_eq!(Value::from("x").as_str(), Some("x"));
        assert_eq!(Value::from("x").as_number(), None);
        assert_eq!(
            [Value::Number(1.0)]
                .into_iter()
                .collect::<Value>()
                .as_array(),
            Some(&[Value::Number(1.0)][..])
        );
        assert!(dict(&[]).as_dict().is_some());
        assert!(Value::from("").is_empty_value());
        assert!(Value::Null.is_empty_value());
        assert!(!Value::Number(0.0).is_empty_value());
        assert_eq!(Value::from(3_u32), Value::Number(3.0));
        assert_eq!(Value::from(-3_i32), Value::Number(-3.0));
        assert_eq!(Value::from(String::from("s")), Value::from("s"));
        assert_eq!(Value::from(Arc::<str>::from("s")), Value::from("s"));
        assert_eq!(Value::default(), Value::Null);
    }

    #[test]
    fn identifiers() {
        assert!(is_identifier("a_1"));
        assert!(is_identifier("_x"));
        assert!(!is_identifier("1a"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("a-b"));
        assert!(!is_identifier("ä"));
    }
}
