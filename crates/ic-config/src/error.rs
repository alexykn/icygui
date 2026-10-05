//! The crate's error type, and helpers that keep error messages short: they
//! end up in dialogs, toasts and the log, so they never quote unbounded user
//! input.

use std::borrow::Cow;
use std::io;
use std::ops::Range;
use std::path::PathBuf;

use crate::validate::ValidationIssue;

/// The most characters of a user-supplied value (a URL, a `format` value, an
/// id) that a message quotes; longer values are cut short with `…`.
pub(crate) const MAX_VALUE_CHARS: usize = 120;

/// The most characters of the offending line that a parse error quotes.
const MAX_LINE_CHARS: usize = 120;

/// The most characters of each line of the TOML crate's own description of
/// an error, which can quote a whole value.
const MAX_MESSAGE_CHARS: usize = 300;

/// The most issues the message of [`ConfigError::Invalid`] lists; the
/// rest are counted.
const MAX_LISTED_ISSUES: usize = 10;

/// Everything that can go wrong finding, reading, writing or interpreting
/// settings.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The operating system reports no home directory, so the standard
    /// locations for settings, data and logs are unknown.
    #[error("cannot locate the settings directory: the system reports no home directory")]
    NoHomeDirectory,

    /// A file system operation failed.
    #[error("{action} {}: {source}", path.display())]
    Io {
        /// What was being done, such as `reading` or `replacing`.
        action: &'static str,
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The text is not valid TOML, or doesn't have the expected structure
    /// (wrong types, unknown enum values, missing required keys). The
    /// message says where, with a line and column when they are known.
    #[error("{message}")]
    Parse {
        /// What is wrong and where.
        message: String,
    },

    /// The `version` key is present but not a whole number.
    #[error("invalid format version: {0}")]
    InvalidVersion(String),

    /// The text was written by a newer version of icygui.
    #[error(
        "format version {found} is newer than this version of icygui supports ({supported}); \
         update icygui to use these settings"
    )]
    UnsupportedVersion {
        /// The version in the text.
        found: u64,
        /// The newest version this build understands.
        supported: u64,
    },

    /// The settings can't be written as TOML, for example because a path
    /// is not valid UTF-8.
    #[error("cannot write settings as TOML: {0}")]
    Serialize(String),

    /// A certificate fingerprint is not 32 bytes of hex.
    #[error("invalid SHA-256 fingerprint: {0}")]
    InvalidFingerprint(String),

    /// An environment's API URL can't be used.
    #[error("invalid API URL `{url}`: {reason}")]
    InvalidUrl {
        /// The URL as configured, made safe to show and log: a user name
        /// and password are replaced by `***`, a query and fragment by `?…`
        /// and `#…`, and a very long URL is cut short.
        url: String,
        /// Why it can't be used.
        reason: String,
    },

    /// Text given to [`import_groups`](crate::import_groups) is not a
    /// dashboard export.
    #[error("not an icygui dashboard export: {0}")]
    NotAnExport(String),

    /// The content breaks rules that [`Config::validate`] checks: an import
    /// with nameless groups or dashboards, or settings that
    /// [`ConfigStore::save`] refuses because they would put a secret into
    /// the file (a password in a URL, `user:password` as a username).
    ///
    /// [`Config::validate`]: crate::Config::validate
    /// [`ConfigStore::save`]: crate::ConfigStore::save
    #[error("invalid content: {}", join_issues(.0))]
    Invalid(Vec<ValidationIssue>),
}

impl ConfigError {
    /// A [`ConfigError::Io`] for `action` on `path`.
    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }

    /// A [`ConfigError::Parse`] from a TOML error.
    ///
    /// Given the `source` text the error refers to, the message has the
    /// TOML crate's layout (line, column, the line with a marker under the
    /// error) but quotes at most a short part of the line: the TOML crate
    /// quotes the whole line, which for a file that is one huge line (a
    /// minified JSON file picked by mistake) is megabytes.
    pub(crate) fn parse(error: &toml::de::Error, source: Option<&str>) -> Self {
        let message = match (source, error.span()) {
            (Some(source), Some(span)) => locate(
                source,
                span,
                &excerpt(error.message().trim_end(), MAX_MESSAGE_CHARS),
            ),
            // Without a position the description is a line or two
            // (message, then the key path).
            _ => error
                .to_string()
                .trim_end()
                .lines()
                .map(|line| excerpt(line, MAX_MESSAGE_CHARS))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        Self::Parse { message }
    }
}

/// `text` cut to at most `max_chars` characters, with `…` if it was longer.
pub(crate) fn excerpt(text: &str, max_chars: usize) -> Cow<'_, str> {
    match text.char_indices().nth(max_chars) {
        None => Cow::Borrowed(text),
        Some((cut, _)) => Cow::Owned(format!("{}…", &text[..cut])),
    }
}

/// The TOML crate's error layout for the error at `span` in `source`,
/// quoting at most [`MAX_LINE_CHARS`] of the line around it:
///
/// ```text
/// TOML parse error at line 3, column 9
///   |
/// 3 | theme = = "dark"
///   |         ^
/// string values must be quoted, expected literal string
/// ```
fn locate(source: &str, span: Range<usize>, description: &str) -> String {
    let start = floor_char_boundary(source, span.start);
    let end = floor_char_boundary(source, span.end).max(start);
    let line_start = source[..start].rfind('\n').map_or(0, |newline| newline + 1);
    let line_end = source[start..]
        .find('\n')
        .map_or(source.len(), |newline| start + newline);
    let line_number = source[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    let line = source[line_start..line_end].trim_end_matches('\r');
    let column = source[line_start..start].chars().count();
    let (shown, caret) = window(line, column);
    let highlight = source[start..end.min(line_end)]
        .chars()
        .count()
        .min(shown.chars().count().saturating_sub(caret))
        .max(1);
    let gutter = " ".repeat(line_number.to_string().len() + 1);
    format!(
        "TOML parse error at line {line_number}, column {}\n{gutter}|\n{line_number} | {shown}\n\
         {gutter}| {}{}\n{description}",
        column + 1,
        " ".repeat(caret),
        "^".repeat(highlight),
    )
}

/// At most [`MAX_LINE_CHARS`] characters of `line` around the character
/// at `column`, with `…` where it was cut, and where `column` ended up.
fn window(line: &str, column: usize) -> (Cow<'_, str>, usize) {
    let length = line.chars().count();
    if length <= MAX_LINE_CHARS {
        return (Cow::Borrowed(line), column);
    }
    let first = column
        .saturating_sub(MAX_LINE_CHARS / 2)
        .min(length - MAX_LINE_CHARS);
    let last = first + MAX_LINE_CHARS;
    let byte = |index: usize| {
        line.char_indices()
            .nth(index)
            .map_or(line.len(), |(byte, _)| byte)
    };
    let mut shown = String::new();
    if first > 0 {
        shown.push('…');
    }
    shown.push_str(&line[byte(first)..byte(last)]);
    if last < length {
        shown.push('…');
    }
    (Cow::Owned(shown), column - first + usize::from(first > 0))
}

/// The largest character boundary of `text` at or before `index`.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The first [`MAX_LISTED_ISSUES`] issues, then how many more there are.
fn join_issues(issues: &[ValidationIssue]) -> String {
    let listed = issues
        .iter()
        .take(MAX_LISTED_ISSUES)
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ");
    match issues.len().checked_sub(MAX_LISTED_ISSUES) {
        Some(more) if more > 0 => format!("{listed}; and {more} more"),
        _ => listed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_error(text: &str) -> toml::de::Error {
        toml::from_str::<toml::Table>(text).unwrap_err()
    }

    fn message(error: &ConfigError) -> &str {
        match error {
            ConfigError::Parse { message } => message,
            other => panic!("expected a parse error, got {other:?}"),
        }
    }

    #[test]
    fn invalid_content_lists_the_first_issues() {
        let issue = |index: usize| ValidationIssue {
            path: format!("groups[{index}].name"),
            message: "must not be empty".to_owned(),
        };
        let two = ConfigError::Invalid(vec![issue(0), issue(1)]);
        assert_eq!(
            two.to_string(),
            "invalid content: groups[0].name: must not be empty; groups[1].name: must not be empty"
        );
        let many = ConfigError::Invalid((0..100_000).map(issue).collect());
        let message = many.to_string();
        assert!(message.ends_with("groups[9].name: must not be empty; and 99990 more"));
        assert!(message.len() < 1_000, "{} bytes", message.len());
        let ten = ConfigError::Invalid((0..10).map(issue).collect());
        assert!(
            ten.to_string()
                .ends_with("groups[9].name: must not be empty")
        );
    }

    #[test]
    fn excerpts_cut_long_text_at_a_character() {
        assert_eq!(excerpt("short", 5), "short");
        assert_eq!(excerpt("longer", 5), "longe…");
        assert_eq!(excerpt("ümläüt", 3), "üml…");
        assert_eq!(excerpt("", 0), "");
        assert_eq!(excerpt("x", 0), "…");
    }

    #[test]
    fn short_lines_are_shown_like_the_toml_crate_does() {
        for text in [
            "version = 1\n[general]\ntheme = = \"dark\"\n",
            "a = 1\nb = \"unterminated\nc = 3\n",
            "[[groups]]\nname = 'x'\n[[groups]\n",
            "key = 1\nkey = 2\n",
            "name = \"ü\" oops\n",
        ] {
            let error = parse_error(text);
            let ours = ConfigError::parse(&error, Some(text));
            assert_eq!(message(&ours), error.to_string().trim_end(), "{text:?}");
        }
    }

    #[test]
    fn long_lines_are_quoted_around_the_error() {
        let filler = "x".repeat(1_000_000);
        let text = format!("version = 1\nfiller = \"{filler}\" oops = 1\n");
        let error = parse_error(&text);
        let message = message(&ConfigError::parse(&error, Some(&text))).to_owned();
        assert!(message.len() < 1_000, "{} bytes", message.len());
        let start = error.span().unwrap().start;
        let column = start - text[..start].rfind('\n').unwrap();
        let lines: Vec<&str> = message.lines().collect();
        assert_eq!(
            lines[0],
            format!("TOML parse error at line 2, column {column}"),
            "{message}"
        );
        // The marker sits under the error inside the excerpt.
        let caret = lines[3].find('^').unwrap();
        assert_eq!(
            lines[2].chars().nth(caret),
            text[start..].chars().next(),
            "{message}"
        );
        assert!(lines[2].starts_with("2 | …x"), "{message}");
        assert!(!message.contains(&"x".repeat(200)), "{message}");
    }

    #[test]
    fn errors_at_the_start_of_long_lines_are_quoted_from_the_start() {
        let text = format!("= {}", "y".repeat(10_000));
        let error = parse_error(&text);
        let message = message(&ConfigError::parse(&error, Some(&text))).to_owned();
        assert!(message.len() < 1_000, "{} bytes", message.len());
        let lines: Vec<&str> = message.lines().collect();
        assert!(lines[2].starts_with("1 | = yyy"), "{message}");
        assert!(lines[3].starts_with("  | ^"), "{message}");
    }

    #[derive(Debug, serde::Deserialize)]
    struct Flag {
        #[expect(dead_code, reason = "only the error is of interest")]
        flag: bool,
    }

    #[derive(Debug, serde::Deserialize)]
    struct Settings {
        #[expect(dead_code, reason = "only the error is of interest")]
        theme: crate::ThemeChoice,
    }

    #[test]
    fn long_values_in_descriptions_are_cut() {
        // The deserializer's description quotes the offending value.
        let text = format!("flag = \"{}\"", "z".repeat(100_000));
        let error = toml::from_str::<Flag>(&text).unwrap_err();
        let message = message(&ConfigError::parse(&error, Some(&text))).to_owned();
        assert!(message.len() < 1_000, "{} bytes", message.len());
        assert!(message.contains("invalid type: string \"zzz"), "{message}");

        // Without the text there is no excerpt to centre, but every line of
        // the description is still cut.
        let message = message_of(&ConfigError::parse(&error, None));
        assert!(message.len() < 2_000, "{} bytes", message.len());
    }

    fn message_of(error: &ConfigError) -> String {
        message(error).to_owned()
    }

    #[test]
    fn errors_without_a_position_keep_their_key_path() {
        let table: toml::Table = toml::from_str("theme = \"sepia\"").unwrap();
        let error = table.try_into::<Settings>().unwrap_err();
        let message = message_of(&ConfigError::parse(&error, None));
        assert!(message.contains("unknown variant `sepia`"), "{message}");
        assert!(message.contains("theme"), "{message}");
    }
}
