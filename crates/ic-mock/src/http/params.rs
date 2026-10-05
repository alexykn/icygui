//! Request parameters as Icinga builds them (`HttpUtility::FetchRequestParameters`):
//! the JSON body, then every URL query parameter as an array of strings
//! (overriding body keys of the same name).

use percent_encoding::percent_decode_str;
use serde_json::{Map, Value as Json};

use crate::filter::format_number;

/// The merged parameters of one request.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Params {
    map: Map<String, Json>,
}

/// Decodes a URL component like Icinga's `Utility::UnescapeString`:
/// percent-decoding only (`+` stays `+`).
pub(crate) fn unescape(text: &str) -> String {
    percent_decode_str(text).decode_utf8_lossy().into_owned()
}

/// Parses a query string like Icinga's `Url::ParseQuery`: `a=b&c=d`,
/// keys may end in `[]`, values are percent-decoded.
///
/// # Errors
/// Malformed queries (`=value` without a key, `[]` in the middle of a key).
pub(crate) fn parse_query(query: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    for token in query.split('&').filter(|t| !t.is_empty()) {
        let (key, value) = match token.split_once('=') {
            Some(("", _)) => return Err(format!("Invalid query parameter: {token}")),
            Some((key, value)) => (key, value),
            None => (token, ""),
        };
        let key = match key.find("[]") {
            Some(position) if position + 2 == key.len() && position > 0 => &key[..position],
            Some(_) => return Err(format!("Invalid query parameter: {token}")),
            None => key,
        };
        pairs.push((unescape(key), unescape(value)));
    }
    Ok(pairs)
}

impl Params {
    /// Merges a JSON body and the query parameters.
    ///
    /// # Errors
    /// The body isn't a JSON object (or `null`); the message follows
    /// Icinga's "Invalid request body" errors.
    pub(crate) fn parse(body: &[u8], query: &[(String, String)]) -> Result<Self, String> {
        let mut map = if body.iter().all(u8::is_ascii_whitespace) {
            Map::new()
        } else {
            match serde_json::from_slice::<Json>(body) {
                Ok(Json::Object(map)) => map,
                Ok(Json::Null) => Map::new(),
                Ok(other) => {
                    return Err(format!(
                        "Error: Cannot convert value of type '{}' to an object.",
                        icinga_type_name(&other)
                    ));
                }
                Err(error) => {
                    return Err(format!(
                        "Error: [json.exception.parse_error.101] parse error: {error}"
                    ));
                }
            }
        };
        let mut grouped: Vec<(String, Vec<Json>)> = Vec::new();
        for (key, value) in query {
            match grouped.iter_mut().find(|(k, _)| k == key) {
                Some((_, values)) => values.push(Json::String(value.clone())),
                None => grouped.push((key.clone(), vec![Json::String(value.clone())])),
            }
        }
        for (key, values) in grouped {
            map.insert(key, Json::Array(values));
        }
        Ok(Self { map })
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        self.map.get(key)
    }

    pub(crate) fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub(crate) fn set(&mut self, key: &str, value: Json) {
        self.map.insert(key.to_owned(), value);
    }

    /// `HttpUtility::GetLastParameter`: the last element of an array, or the
    /// value itself. `None` for missing keys and empty arrays.
    pub(crate) fn last(&self, key: &str) -> Option<&Json> {
        match self.map.get(key)? {
            Json::Array(items) => items.last(),
            other => Some(other),
        }
    }

    /// The last value as a string (Icinga's `operator String()`).
    pub(crate) fn last_string(&self, key: &str) -> String {
        self.last(key).map_or_else(String::new, to_icinga_string)
    }

    /// The last value as a boolean (Icinga's `ToBool`: non-empty strings are
    /// true, even `"false"`).
    pub(crate) fn last_bool(&self, key: &str) -> bool {
        self.last(key).is_some_and(to_bool)
    }

    /// The last value as a number.
    ///
    /// # Errors
    /// Strings that aren't numbers (Icinga throws there).
    pub(crate) fn last_f64(&self, key: &str) -> Result<f64, String> {
        self.last(key).map_or(Ok(0.0), to_f64)
    }

    /// `pretty` asks for indented JSON.
    pub(crate) fn pretty(&self) -> bool {
        self.last_bool("pretty")
    }

    /// `verbose` adds `diagnostic_information` to errors.
    pub(crate) fn verbose(&self) -> bool {
        self.last_bool("verbose")
    }
}

/// Icinga's type names, for error messages.
pub(crate) fn icinga_type_name(value: &Json) -> &'static str {
    match value {
        Json::Null => "Empty",
        Json::Bool(_) => "Boolean",
        Json::Number(_) => "Number",
        Json::String(_) => "String",
        Json::Array(_) => "Array",
        Json::Object(_) => "Dictionary",
    }
}

/// `Value::operator String()`.
pub(crate) fn to_icinga_string(value: &Json) -> String {
    match value {
        Json::Null => String::new(),
        Json::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
        Json::Number(n) => format_number(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => s.clone(),
        Json::Array(_) => "Object of type 'Array'".to_owned(),
        Json::Object(_) => "Object of type 'Dictionary'".to_owned(),
    }
}

/// `Value::ToBool`.
pub(crate) fn to_bool(value: &Json) -> bool {
    match value {
        Json::Null => false,
        Json::Bool(b) => *b,
        Json::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Json::String(s) => !s.is_empty(),
        Json::Array(items) => !items.is_empty(),
        Json::Object(map) => !map.is_empty(),
    }
}

/// `Value::operator double()`.
///
/// # Errors
/// Values that can't be converted.
pub(crate) fn to_f64(value: &Json) -> Result<f64, String> {
    match value {
        Json::Null => Ok(0.0),
        Json::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Json::Number(n) => Ok(n.as_f64().unwrap_or(0.0)),
        Json::String(s) => s
            .trim()
            .parse::<f64>()
            .map_err(|_| format!("Can't convert '{s}' to a floating point number.")),
        other => Err(format!(
            "Cannot convert value of type '{}' to a floating point number.",
            icinga_type_name(other)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn query(text: &str) -> Vec<(String, String)> {
        parse_query(text).unwrap()
    }

    #[test]
    fn query_values_are_arrays_and_override_the_body() {
        let params = Params::parse(
            br#"{"attrs": ["name"], "filter": "x", "pretty": true}"#,
            &query("attrs=state&attrs=name&filter=host.name%3D%3D%22a%20b%22"),
        )
        .unwrap();
        assert_eq!(params.get("attrs"), Some(&json!(["state", "name"])));
        assert_eq!(params.last_string("filter"), "host.name==\"a b\"");
        assert!(params.pretty());
    }

    #[test]
    fn plus_is_not_a_space() {
        let params = Params::parse(b"", &query("filter=a+b")).unwrap();
        assert_eq!(params.last_string("filter"), "a+b");
    }

    #[test]
    fn brackets_and_bad_keys() {
        assert_eq!(
            query("types[]=CheckResult"),
            vec![("types".into(), "CheckResult".into())]
        );
        assert!(parse_query("=x").is_err());
        assert!(parse_query("a[]b=1").is_err());
        assert_eq!(query("flag"), vec![("flag".into(), String::new())]);
    }

    #[test]
    fn bodies_must_be_objects() {
        assert!(Params::parse(b"[1]", &[]).is_err());
        assert!(Params::parse(b"{nope", &[]).is_err());
        assert!(Params::parse(b"null", &[]).unwrap().get("x").is_none());
        assert!(Params::parse(b"  ", &[]).is_ok());
    }

    #[test]
    fn icinga_conversions() {
        assert!(to_bool(&json!("false")), "non-empty strings are true");
        assert!(!to_bool(&json!("")));
        assert!(!to_bool(&json!(0)));
        assert_eq!(to_f64(&json!("12.5")), Ok(12.5));
        assert!(to_f64(&json!("soon")).is_err());
        assert_eq!(to_icinga_string(&json!(2.0)), "2");
        assert_eq!(to_icinga_string(&json!(2.5)), "2.500000");
        let params = Params::parse(br#"{"a": []}"#, &[]).unwrap();
        assert!(
            params.last("a").is_none(),
            "empty arrays have no last value"
        );
    }
}
