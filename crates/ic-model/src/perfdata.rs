//! Plugin performance data (`label=value[unit];warn;crit;min;max`) and
//! Nagios threshold ranges, parsed the way Icinga Web does.

use serde::{Deserialize, Serialize};

/// One performance data value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Perfdata {
    /// The label, without quotes.
    pub label: String,
    /// The measured value. `None` when the plugin reported `U` (unknown) or
    /// something unparsable.
    pub value: Option<f64>,
    /// Unit of measurement as reported (`s`, `ms`, `%`, `B`, `KB`, `c`, …).
    pub unit: String,
    /// Warning threshold.
    pub warn: Option<Threshold>,
    /// Critical threshold.
    pub crit: Option<Threshold>,
    /// Minimum possible value.
    pub min: Option<f64>,
    /// Maximum possible value.
    pub max: Option<f64>,
}

/// Which threshold a value crosses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PerfdataStatus {
    /// Within both thresholds, or no thresholds.
    Ok,
    /// Outside the warning range.
    Warning,
    /// Outside the critical range.
    Critical,
}

impl Perfdata {
    /// Whether the value crosses the critical or warning threshold.
    #[must_use]
    pub fn status(&self) -> PerfdataStatus {
        let Some(value) = self.value else {
            return PerfdataStatus::Ok;
        };
        if self.crit.as_ref().is_some_and(|t| t.is_violated(value)) {
            PerfdataStatus::Critical
        } else if self.warn.as_ref().is_some_and(|t| t.is_violated(value)) {
            PerfdataStatus::Warning
        } else {
            PerfdataStatus::Ok
        }
    }

    /// The value with its unit (`412s`, `1.8GiB`), or `U` if unknown.
    #[must_use]
    pub fn display_value(&self) -> String {
        self.value.map_or_else(
            || "U".to_owned(),
            |value| format!("{}{}", format_number(value), self.unit),
        )
    }
}

/// A Nagios threshold range (`10`, `10:`, `~:10`, `10:20`, `@10:20`).
///
/// The range describes the *good* values; a value outside it (or inside it,
/// for `@` ranges) violates the threshold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Threshold {
    /// The threshold as the plugin wrote it.
    pub raw: String,
    /// Lower bound; `None` means negative infinity (`~`).
    pub start: Option<f64>,
    /// Upper bound; `None` means positive infinity.
    pub end: Option<f64>,
    /// `@` ranges alert when the value is *inside* the range.
    pub inside: bool,
}

impl Threshold {
    /// Parses a threshold range. Returns `None` for an empty string.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let (inside, range) = match trimmed.strip_prefix('@') {
            Some(rest) => (true, rest),
            None => (false, trimmed),
        };
        let (start, end) = match range.split_once(':') {
            None => (Some(0.0), parse_number(range)),
            Some((start, end)) => {
                let start = match start.trim() {
                    "" => Some(0.0),
                    "~" => None,
                    value => parse_number(value),
                };
                let end = match end.trim() {
                    "" => None,
                    value => parse_number(value),
                };
                (start, end)
            }
        };
        Some(Self {
            raw: trimmed.to_owned(),
            start,
            end,
            inside,
        })
    }

    /// Whether `value` violates this threshold.
    #[must_use]
    pub fn is_violated(&self, value: f64) -> bool {
        let in_range = self.start.is_none_or(|start| start <= value)
            && self.end.is_none_or(|end| value <= end);
        in_range == self.inside
    }

    /// A short form for tables: the upper bound for the common `0:N` form,
    /// otherwise the raw range.
    #[must_use]
    pub fn display(&self) -> String {
        match (self.inside, self.start, self.end) {
            (false, Some(0.0), Some(end)) => format_number(end),
            _ => self.raw.clone(),
        }
    }
}

/// Parses a plugin's performance data string.
///
/// Labels may be quoted with `'` (a doubled `''` is a literal quote) and may
/// contain spaces when quoted; entries are separated by whitespace.
#[must_use]
pub fn parse_perfdata(input: &str) -> Vec<Perfdata> {
    let mut entries = Vec::new();
    let mut rest = input.trim_start();
    while !rest.is_empty() {
        let (label, after_label) = read_label(rest);
        let value_end = after_label
            .find(char::is_whitespace)
            .unwrap_or(after_label.len());
        let (value, remainder) = after_label.split_at(value_end);
        if !label.is_empty()
            && let Some(entry) = parse_entry(&label, value)
        {
            entries.push(entry);
        }
        rest = remainder.trim_start();
    }
    entries
}

/// Parses one already-split perfdata entry (`label=value;warn;crit;min;max`),
/// as Icinga's API returns them one string per entry.
#[must_use]
pub fn parse_perfdata_entry(entry: &str) -> Option<Perfdata> {
    let (label, value) = read_label(entry.trim());
    if label.is_empty() {
        return None;
    }
    parse_entry(&label, value.trim())
}

/// Reads a possibly quoted label up to `=`; returns the label and the text
/// after the `=`.
fn read_label(input: &str) -> (String, &str) {
    if let Some(quoted) = input.strip_prefix('\'') {
        let mut label = String::new();
        let mut chars = quoted.char_indices().peekable();
        while let Some((index, c)) = chars.next() {
            if c == '\'' {
                if chars.peek().is_some_and(|&(_, next)| next == '\'') {
                    label.push('\'');
                    chars.next();
                    continue;
                }
                let after_quote = &quoted[index + 1..];
                let after_equals = after_quote.split_once('=').map_or("", |(_, value)| value);
                return (label, after_equals);
            }
            label.push(c);
        }
        return (label, "");
    }
    match input.split_once('=') {
        Some((label, value)) => (label.trim().to_owned(), value),
        None => (String::new(), ""),
    }
}

fn parse_entry(label: &str, value: &str) -> Option<Perfdata> {
    let mut parts = value.split(';');
    let first = parts.next()?.trim();
    if first.is_empty() {
        return None;
    }
    let (value, unit) = split_value_unit(first);
    let mut next_part = || parts.next().map(str::trim).filter(|part| !part.is_empty());
    let warn = next_part().and_then(Threshold::parse);
    let crit = next_part().and_then(Threshold::parse);
    let min = next_part().and_then(parse_number);
    let max = next_part().and_then(parse_number);
    Some(Perfdata {
        label: label.to_owned(),
        value,
        unit,
        warn,
        crit,
        min,
        max,
    })
}

fn split_value_unit(raw: &str) -> (Option<f64>, String) {
    let bytes = raw.as_bytes();
    let digits_from = |mut index: usize| {
        while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
            index += 1;
        }
        index
    };
    let sign = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
    let mut end = digits_from(sign);
    // Comma as decimal separator (some plugins honour the locale).
    if end < bytes.len() && bytes[end] == b',' && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
    {
        end = digits_from(end + 1);
    }
    // An exponent only counts when digits follow it; `1EB` is one exabyte.
    if end < bytes.len() && matches!(bytes[end], b'e' | b'E') {
        let exponent_sign = usize::from(matches!(bytes.get(end + 1), Some(b'-' | b'+')));
        let digits_start = end + 1 + exponent_sign;
        let mut exponent_end = digits_start;
        while exponent_end < bytes.len() && bytes[exponent_end].is_ascii_digit() {
            exponent_end += 1;
        }
        if exponent_end > digits_start {
            end = exponent_end;
        }
    }
    let (number, unit) = raw.split_at(end);
    match parse_number(number) {
        Some(value) => (Some(value), unit.to_owned()),
        None => (None, String::new()),
    }
}

fn parse_number(raw: &str) -> Option<f64> {
    let raw = raw.trim().replace(',', ".");
    raw.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// Formats a number without trailing zeros (`412`, `0.42`, `1.8`).
#[must_use]
pub fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{value:.0}");
    }
    let formatted = format!("{value:.3}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(input: &str) -> Perfdata {
        let mut parsed = parse_perfdata(input);
        assert_eq!(parsed.len(), 1, "{input}");
        parsed.remove(0)
    }

    #[test]
    fn parses_a_full_entry() {
        let p = one("replication_lag=412s;60;300;0;3600");
        assert_eq!(p.label, "replication_lag");
        assert_eq!(p.value, Some(412.0));
        assert_eq!(p.unit, "s");
        assert_eq!(p.warn.as_ref().unwrap().display(), "60");
        assert_eq!(p.crit.as_ref().unwrap().display(), "300");
        assert_eq!(p.min, Some(0.0));
        assert_eq!(p.max, Some(3600.0));
        assert_eq!(p.status(), PerfdataStatus::Critical);
        assert_eq!(p.display_value(), "412s");
    }

    #[test]
    fn parses_several_entries_and_quoted_labels() {
        let parsed =
            parse_perfdata("'disk /var'=97%;80;90 time=0.004s 'it''s'=1 rta=0.42ms;100;200;0");
        let labels: Vec<_> = parsed.iter().map(|p| p.label.as_str()).collect();
        assert_eq!(labels, ["disk /var", "time", "it's", "rta"]);
        assert_eq!(parsed[0].status(), PerfdataStatus::Critical);
        assert_eq!(parsed[1].warn, None);
        assert_eq!(parsed[3].display_value(), "0.42ms");
    }

    #[test]
    fn unknown_values_and_garbage() {
        let p = one("load=U;5;10");
        assert_eq!(p.value, None);
        assert_eq!(p.display_value(), "U");
        assert_eq!(p.status(), PerfdataStatus::Ok);
        assert!(parse_perfdata("no equals sign").is_empty());
        assert!(parse_perfdata("").is_empty());
        assert!(parse_perfdata("label=").is_empty());
    }

    #[test]
    fn single_entries_from_the_api() {
        let p = parse_perfdata_entry("'active connections'=182;400;450;0").unwrap();
        assert_eq!(p.label, "active connections");
        assert_eq!(p.value, Some(182.0));
        assert_eq!(p.status(), PerfdataStatus::Ok);
    }

    #[test]
    fn negative_and_exponent_values() {
        assert_eq!(one("offset=-0.82s;0.5;1").value, Some(-0.82));
        assert_eq!(one("big=1e3B").value, Some(1000.0));
        assert_eq!(one("big=1e3B").unit, "B");
        assert_eq!(one("huge=1EB").value, Some(1.0));
        assert_eq!(one("huge=1EB").unit, "EB");
        assert_eq!(one("load=0,5").value, Some(0.5));
    }

    #[test]
    fn threshold_ranges_follow_the_plugin_guidelines() {
        let t = |raw: &str| Threshold::parse(raw).unwrap();
        // "10": alert if < 0 or > 10
        assert!(!t("10").is_violated(5.0));
        assert!(t("10").is_violated(11.0));
        assert!(t("10").is_violated(-1.0));
        // "10:": alert if < 10
        assert!(t("10:").is_violated(9.0));
        assert!(!t("10:").is_violated(1e9));
        // "~:10": alert if > 10
        assert!(!t("~:10").is_violated(-1e9));
        assert!(t("~:10").is_violated(11.0));
        // "10:20": alert if outside
        assert!(t("10:20").is_violated(9.0));
        assert!(!t("10:20").is_violated(15.0));
        // "@10:20": alert if inside
        assert!(t("@10:20").is_violated(15.0));
        assert!(!t("@10:20").is_violated(21.0));
        assert!(Threshold::parse("  ").is_none());
    }

    #[test]
    fn warning_only_when_critical_is_not_crossed() {
        let p = one("lag=70s;60;300");
        assert_eq!(p.status(), PerfdataStatus::Warning);
    }

    #[test]
    fn number_formatting_trims_zeros() {
        assert_eq!(format_number(412.0), "412");
        assert_eq!(format_number(0.42), "0.42");
        assert_eq!(format_number(1.8), "1.8");
        assert_eq!(format_number(0.0004), "0");
        assert_eq!(format_number(-3.5), "-3.5");
    }
}
