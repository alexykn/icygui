//! Values of the filter language and Icinga's conversions between them.

use std::collections::BTreeMap;
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
            Value::Array(_) | Value::Dict(_) => {
                let mut out = String::new();
                // Without a limit the text can't become too long.
                let _ = self.write_icinga_string(&mut Limited::new(&mut out, usize::MAX));
                out
            }
        }
    }

    /// [`Value::to_icinga_string`], or `None` when the text would be longer
    /// than `limit` bytes. Never writes much more than `limit` bytes, so
    /// a huge (or hugely repetitive) array costs no more than the limit.
    pub(crate) fn icinga_string_within(&self, limit: usize) -> Option<String> {
        let mut out = String::new();
        self.write_icinga_string(&mut Limited::new(&mut out, limit))
            .ok()
            .map(|()| out)
    }

    /// The value as `string()` writes it, shortened for an error message.
    pub(crate) fn icinga_preview(&self) -> String {
        let mut out = String::new();
        let complete = self
            .write_icinga_string(&mut Limited::new(&mut out, PREVIEW_BYTES))
            .is_ok();
        finish_preview(out, complete)
    }

    /// The value as compact JSON (Icinga's `JsonEncode`), shortened for an
    /// error message.
    pub(crate) fn json_preview(&self) -> String {
        let mut out = String::new();
        let complete = write_json(&mut Limited::new(&mut out, PREVIEW_BYTES), self).is_ok();
        finish_preview(out, complete)
    }

    fn write_icinga_string(&self, out: &mut Limited<'_>) -> Result<(), TooLong> {
        match self {
            Value::Null => Ok(()),
            Value::Bool(value) => out.push_str(bool_str(*value)),
            Value::Number(value) => out.push_str(&format_number(*value)),
            Value::String(text) => out.push_str(text),
            Value::Array(items) => emit_array(out, 1, items),
            Value::Dict(entries) => emit_dict(out, 1, entries),
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

/// Error messages show at most this many bytes of a value.
const PREVIEW_BYTES: usize = 80;

/// Text written into a string that must not grow beyond a limit.
struct Limited<'a> {
    out: &'a mut String,
    limit: usize,
}

/// The text reached the limit; what was written so far is cut there.
struct TooLong;

impl<'a> Limited<'a> {
    fn new(out: &'a mut String, limit: usize) -> Self {
        Limited { out, limit }
    }

    /// Appends `text`, or as much of it as fits.
    fn push_str(&mut self, text: &str) -> Result<(), TooLong> {
        let room = self.limit.saturating_sub(self.out.len());
        if text.len() <= room {
            self.out.push_str(text);
            Ok(())
        } else {
            self.out.push_str(&text[..floor_char_boundary(text, room)]);
            Err(TooLong)
        }
    }

    fn push(&mut self, ch: char) -> Result<(), TooLong> {
        self.push_str(ch.encode_utf8(&mut [0; 4]))
    }
}

/// The largest character boundary in `text` at or below `index`.
pub(crate) fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Ends a preview: complete text as is, cut text with an ellipsis.
fn finish_preview(mut text: String, complete: bool) -> String {
    if !complete {
        text.push('…');
    }
    text
}

/// `text` for an error message: at most [`PREVIEW_BYTES`] bytes of it.
pub(crate) fn preview_str(text: &str) -> String {
    let mut out = String::new();
    let complete = Limited::new(&mut out, PREVIEW_BYTES).push_str(text).is_ok();
    finish_preview(out, complete)
}

/// Icinga's `ConfigWriter::EmitValue`.
fn emit_value(out: &mut Limited<'_>, indent: usize, value: &Value) -> Result<(), TooLong> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(bool_str(*value)),
        Value::Number(value) => {
            if value.is_nan() {
                out.push_str(nan_str(*value))
            } else {
                out.push_str(&format!("{value:.6}"))
            }
        }
        Value::String(text) => emit_string(out, text),
        Value::Array(items) => emit_array(out, indent, items),
        Value::Dict(entries) => emit_dict(out, indent, entries),
    }
}

fn emit_array(out: &mut Limited<'_>, indent: usize, items: &[Value]) -> Result<(), TooLong> {
    out.push_str("[ ")?;
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(", ")?;
        }
        emit_value(out, indent, item)?;
    }
    if !items.is_empty() {
        out.push(' ')?;
    }
    out.push(']')
}

fn emit_dict(
    out: &mut Limited<'_>,
    indent: usize,
    entries: &BTreeMap<String, Value>,
) -> Result<(), TooLong> {
    out.push('{')?;
    for (key, value) in entries {
        out.push('\n')?;
        push_tabs(out, indent)?;
        emit_identifier(out, key)?;
        out.push_str(" = ")?;
        emit_value(out, indent + 1, value)?;
    }
    out.push('\n')?;
    push_tabs(out, indent.saturating_sub(1))?;
    out.push('}')
}

fn push_tabs(out: &mut Limited<'_>, count: usize) -> Result<(), TooLong> {
    for _ in 0..count {
        out.push('\t')?;
    }
    Ok(())
}

fn emit_identifier(out: &mut Limited<'_>, key: &str) -> Result<(), TooLong> {
    if CONFIG_KEYWORDS.contains(&key) {
        out.push('@')?;
        out.push_str(key)
    } else if is_identifier(key) {
        out.push_str(key)
    } else {
        emit_string(out, key)
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

fn emit_string(out: &mut Limited<'_>, text: &str) -> Result<(), TooLong> {
    out.push('"')?;
    write_escaped(out, text, |byte| {
        Some(match byte {
            b'\\' => "\\\\",
            b'\n' => "\\n",
            b'\t' => "\\t",
            b'\r' => "\\r",
            0x08 => "\\b",
            0x0c => "\\f",
            b'"' => "\\\"",
            _ => return None,
        })
    })?;
    out.push('"')
}

/// Writes `text` with the ASCII bytes `escape` maps replaced, copying the
/// runs in between at once.
fn write_escaped(
    out: &mut Limited<'_>,
    text: &str,
    escape: impl Fn(u8) -> Option<&'static str>,
) -> Result<(), TooLong> {
    let mut plain = 0;
    for (index, byte) in text.bytes().enumerate() {
        if let Some(escaped) = escape(byte) {
            // Escaped bytes are ASCII, so `index` is a character boundary.
            out.push_str(&text[plain..index])?;
            out.push_str(escaped)?;
            plain = index + 1;
        }
    }
    out.push_str(&text[plain..])
}

/// Compact JSON, as `serde_json` writes it.
fn write_json(out: &mut Limited<'_>, value: &Value) -> Result<(), TooLong> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(value) => out.push_str(bool_str(*value)),
        Value::Number(value) => out.push_str(&number_to_json(*value).to_string()),
        Value::String(text) => write_json_string(out, text),
        Value::Array(items) => {
            out.push('[')?;
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',')?;
                }
                write_json(out, item)?;
            }
            out.push(']')
        }
        Value::Dict(entries) => {
            out.push('{')?;
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push(',')?;
                }
                write_json_string(out, key)?;
                out.push(':')?;
                write_json(out, item)?;
            }
            out.push('}')
        }
    }
}

fn write_json_string(out: &mut Limited<'_>, text: &str) -> Result<(), TooLong> {
    /// `\u00XX` for the control characters JSON has no short escape for.
    const CONTROL: [&str; 32] = [
        "\\u0000", "\\u0001", "\\u0002", "\\u0003", "\\u0004", "\\u0005", "\\u0006", "\\u0007",
        "\\b", "\\t", "\\n", "\\u000b", "\\f", "\\r", "\\u000e", "\\u000f", "\\u0010", "\\u0011",
        "\\u0012", "\\u0013", "\\u0014", "\\u0015", "\\u0016", "\\u0017", "\\u0018", "\\u0019",
        "\\u001a", "\\u001b", "\\u001c", "\\u001d", "\\u001e", "\\u001f",
    ];
    out.push('"')?;
    write_escaped(out, text, |byte| match byte {
        b'"' => Some("\\\""),
        b'\\' => Some("\\\\"),
        control => CONTROL.get(usize::from(control)).copied(),
    })?;
    out.push('"')
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
        assert_eq!(Value::Number(2.0).to_json().to_string(), "2");
        assert_eq!(Value::Number(2.5).to_json().to_string(), "2.5");
        assert_eq!(Value::Number(f64::INFINITY).to_json().to_string(), "null");
        assert_eq!(Value::from("a\"b").to_json().to_string(), r#""a\"b""#);
    }

    #[test]
    fn json_previews_match_serde_json_and_are_short() {
        let value = Value::from_json(&json!({
            "a": [1, 2.5, null, true, "x\"y\\z\n\t\r\u{8}\u{c}\u{1}é"],
            "b": {},
        }));
        assert_eq!(value.json_preview(), value.to_json().to_string());
        assert_eq!(Value::from("b").json_preview(), r#""b""#);
        assert_eq!(Value::Number(5.0).json_preview(), "5");

        let long = Value::from("é".repeat(1_000));
        let preview = long.json_preview();
        assert!(preview.len() <= PREVIEW_BYTES + '…'.len_utf8(), "{preview}");
        assert!(preview.starts_with("\"éé") && preview.ends_with('…'));
        let many: Value = (0..100_000).map(|n| Value::Number(f64::from(n))).collect();
        assert!(many.json_preview().len() <= PREVIEW_BYTES + '…'.len_utf8());
    }

    #[test]
    fn icinga_previews_and_bounded_conversion() {
        let array = Value::from(vec![Value::from("dev"), Value::from("slack")]);
        assert_eq!(array.icinga_preview(), r#"[ "dev", "slack" ]"#);
        assert_eq!(
            array.icinga_string_within(100).as_deref(),
            Some(r#"[ "dev", "slack" ]"#)
        );
        assert_eq!(
            array.icinga_string_within(18).as_deref(),
            Some(r#"[ "dev", "slack" ]"#)
        );
        assert_eq!(array.icinga_string_within(17), None);
        assert_eq!(Value::Null.icinga_string_within(0).as_deref(), Some(""));

        // A value shared many times over is cut off at the limit, not
        // written out completely first.
        let shared = Value::from(vec![Value::from("x".repeat(1_000)); 1_000]);
        let nested = Value::from(vec![shared; 1_000]);
        assert_eq!(nested.icinga_string_within(10_000), None);
        let preview = nested.icinga_preview();
        assert!(preview.len() <= PREVIEW_BYTES + '…'.len_utf8() && preview.ends_with('…'));

        assert_eq!(preview_str("short"), "short");
        assert_eq!(
            preview_str(&"ü".repeat(100)),
            format!("{}…", "ü".repeat(40))
        );
        assert_eq!(floor_char_boundary("aü", 2), 1);
        assert_eq!(floor_char_boundary("aü", 9), 3);
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
