//! Tokenizer following Icinga's `config_lexer.ll`.
//!
//! The lexer mirrors flex's longest-match rules, including two surprising
//! ones that the parser turns into helpful errors: `!in` is a token even when
//! more letters follow (`!inactive` is `!in` + `active`), and `<…>` without
//! spaces is an include path (`a<b>c`). Line breaks are tokens (statement
//! separators) except inside parentheses, as in Icinga.

use crate::ParseError;

/// Keywords that can't be used as identifiers (escape them with `@`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Keyword {
    Object,
    Template,
    Include,
    IncludeRecursive,
    IncludeZones,
    Library,
    Const,
    Var,
    This,
    Globals,
    Locals,
    Use,
    Using,
    Apply,
    Default,
    To,
    Where,
    Import,
    Assign,
    Ignore,
    Function,
    Return,
    Break,
    Continue,
    For,
    If,
    Else,
    While,
    Throw,
    Try,
    Except,
    IgnoreOnError,
    CurrentFilename,
    CurrentLine,
    Debugger,
    Namespace,
}

impl Keyword {
    const ALL: [Keyword; 36] = [
        Keyword::Object,
        Keyword::Template,
        Keyword::Include,
        Keyword::IncludeRecursive,
        Keyword::IncludeZones,
        Keyword::Library,
        Keyword::Const,
        Keyword::Var,
        Keyword::This,
        Keyword::Globals,
        Keyword::Locals,
        Keyword::Use,
        Keyword::Using,
        Keyword::Apply,
        Keyword::Default,
        Keyword::To,
        Keyword::Where,
        Keyword::Import,
        Keyword::Assign,
        Keyword::Ignore,
        Keyword::Function,
        Keyword::Return,
        Keyword::Break,
        Keyword::Continue,
        Keyword::For,
        Keyword::If,
        Keyword::Else,
        Keyword::While,
        Keyword::Throw,
        Keyword::Try,
        Keyword::Except,
        Keyword::IgnoreOnError,
        Keyword::CurrentFilename,
        Keyword::CurrentLine,
        Keyword::Debugger,
        Keyword::Namespace,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Keyword::Object => "object",
            Keyword::Template => "template",
            Keyword::Include => "include",
            Keyword::IncludeRecursive => "include_recursive",
            Keyword::IncludeZones => "include_zones",
            Keyword::Library => "library",
            Keyword::Const => "const",
            Keyword::Var => "var",
            Keyword::This => "this",
            Keyword::Globals => "globals",
            Keyword::Locals => "locals",
            Keyword::Use => "use",
            Keyword::Using => "using",
            Keyword::Apply => "apply",
            Keyword::Default => "default",
            Keyword::To => "to",
            Keyword::Where => "where",
            Keyword::Import => "import",
            Keyword::Assign => "assign",
            Keyword::Ignore => "ignore",
            Keyword::Function => "function",
            Keyword::Return => "return",
            Keyword::Break => "break",
            Keyword::Continue => "continue",
            Keyword::For => "for",
            Keyword::If => "if",
            Keyword::Else => "else",
            Keyword::While => "while",
            Keyword::Throw => "throw",
            Keyword::Try => "try",
            Keyword::Except => "except",
            Keyword::IgnoreOnError => "ignore_on_error",
            Keyword::CurrentFilename => "current_filename",
            Keyword::CurrentLine => "current_line",
            Keyword::Debugger => "debugger",
            Keyword::Namespace => "namespace",
        }
    }

    fn from_word(word: &str) -> Option<Keyword> {
        Keyword::ALL
            .into_iter()
            .find(|keyword| keyword.as_str() == word)
    }
}

/// A token kind.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Number(f64),
    Str(String),
    Ident(String),
    Keyword(Keyword),
    True,
    False,
    Null,
    In,
    NotIn,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Caret,
    Amp,
    Pipe,
    Tilde,
    Bang,
    AndAnd,
    OrOr,
    EqEq,
    NotEq,
    Lt,
    Gt,
    Le,
    Ge,
    Shl,
    Shr,
    Dot,
    Comma,
    Semicolon,
    Colon,
    Question,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    /// `=`.
    Assign,
    /// `+=`, `-=`, …
    CompoundAssign(&'static str),
    /// `=>`.
    Arrow,
    /// `{{`, which starts a function in Icinga.
    LambdaOpen,
    /// `}}`, which ends a function in Icinga.
    LambdaClose,
    /// `<…>`, an include path.
    AngleString,
    Newline,
    Eof,
    /// Where the lexer failed; stands for the lexer's error.
    Invalid,
}

impl Tok {
    /// How the token is described in error messages.
    pub(crate) fn describe(&self) -> String {
        match self {
            Tok::Number(_) => "number".to_owned(),
            Tok::Str(_) => "string".to_owned(),
            Tok::Ident(name) => format!("identifier '{name}'"),
            Tok::Keyword(keyword) => format!("keyword '{}'", keyword.as_str()),
            Tok::Newline => "line break".to_owned(),
            Tok::Eof => "end of filter".to_owned(),
            Tok::AngleString => "include path".to_owned(),
            Tok::Invalid => "invalid input".to_owned(),
            other => format!("'{}'", other.symbol()),
        }
    }

    fn symbol(&self) -> &'static str {
        match self {
            Tok::True => "true",
            Tok::False => "false",
            Tok::Null => "null",
            Tok::In => "in",
            Tok::NotIn => "!in",
            Tok::Plus => "+",
            Tok::Minus => "-",
            Tok::Star => "*",
            Tok::Slash => "/",
            Tok::Percent => "%",
            Tok::Caret => "^",
            Tok::Amp => "&",
            Tok::Pipe => "|",
            Tok::Tilde => "~",
            Tok::Bang => "!",
            Tok::AndAnd => "&&",
            Tok::OrOr => "||",
            Tok::EqEq => "==",
            Tok::NotEq => "!=",
            Tok::Lt => "<",
            Tok::Gt => ">",
            Tok::Le => "<=",
            Tok::Ge => ">=",
            Tok::Shl => "<<",
            Tok::Shr => ">>",
            Tok::Dot => ".",
            Tok::Comma => ",",
            Tok::Semicolon => ";",
            Tok::Colon => ":",
            Tok::Question => "?",
            Tok::LParen => "(",
            Tok::RParen => ")",
            Tok::LBracket => "[",
            Tok::RBracket => "]",
            Tok::LBrace => "{",
            Tok::RBrace => "}",
            Tok::Assign => "=",
            Tok::CompoundAssign(op) => op,
            Tok::Arrow => "=>",
            Tok::LambdaOpen => "{{",
            Tok::LambdaClose => "}}",
            Tok::Number(_)
            | Tok::Str(_)
            | Tok::Ident(_)
            | Tok::Keyword(_)
            | Tok::AngleString
            | Tok::Newline
            | Tok::Eof
            | Tok::Invalid => "",
        }
    }
}

/// A token and its byte range in the source.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Token {
    pub(crate) tok: Tok,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// Splits `source` into tokens, ending with [`Tok::Eof`].
///
/// On an error, the tokens before it are returned, ending with
/// [`Tok::Invalid`] at the error's offset, together with the error. The
/// parser reports a syntax error in front of it first, as Icinga's
/// incremental lexer and parser do.
pub(crate) fn tokenize(source: &str) -> (Vec<Token>, Option<ParseError>) {
    let mut lexer = Lexer {
        source,
        bytes: source.as_bytes(),
        pos: 0,
        newline_modes: Vec::new(),
        tokens: Vec::new(),
    };
    let error = lexer.run().err();
    let mut tokens = lexer.tokens;
    let (tok, offset) = match &error {
        Some(error) => (Tok::Invalid, error.offset),
        None => (Tok::Eof, source.len()),
    };
    tokens.push(Token {
        tok,
        start: offset,
        end: offset,
    });
    (tokens, error)
}

struct Lexer<'s> {
    source: &'s str,
    bytes: &'s [u8],
    pos: usize,
    /// Whether line breaks are ignored, per open `(` (yes) and `{` (no).
    newline_modes: Vec<bool>,
    tokens: Vec<Token>,
}

fn error(message: impl Into<String>, offset: usize) -> ParseError {
    ParseError {
        message: message.into(),
        offset,
    }
}

impl Lexer<'_> {
    fn run(&mut self) -> Result<(), ParseError> {
        while self.pos < self.bytes.len() {
            self.next_token()?;
        }
        Ok(())
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    fn starts_with(&self, text: &str) -> bool {
        self.bytes[self.pos..].starts_with(text.as_bytes())
    }

    fn push(&mut self, tok: Tok, start: usize) {
        self.tokens.push(Token {
            tok,
            start,
            end: self.pos,
        });
    }

    /// Emits a token made of the next `len` bytes.
    fn symbol(&mut self, tok: Tok, len: usize) {
        let start = self.pos;
        self.pos += len;
        self.push(tok, start);
    }

    fn next_token(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let byte = self.bytes[start];
        match byte {
            b' ' | b'\t' => self.pos += 1,
            b'\r' | b'\n' => {
                while matches!(self.peek(0), Some(b'\r' | b'\n')) {
                    self.pos += 1;
                }
                if !self.newline_modes.last().copied().unwrap_or(false) {
                    self.push(Tok::Newline, start);
                }
            }
            b'#' => self.skip_line(),
            b'/' if self.peek(1) == Some(b'/') => self.skip_line(),
            b'/' if self.peek(1) == Some(b'*') => self.skip_block_comment()?,
            b'"' => self.string()?,
            b'{' if self.starts_with("{{{") => self.heredoc()?,
            b'0'..=b'9' => self.number(),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.word(false),
            b'@' if self
                .peek(1)
                .is_some_and(|next| next.is_ascii_alphabetic() || next == b'_') =>
            {
                self.word(true);
            }
            _ => self.operator()?,
        }
        Ok(())
    }

    fn skip_line(&mut self) {
        while self.peek(0).is_some_and(|byte| byte != b'\n') {
            self.pos += 1;
        }
    }

    fn skip_block_comment(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        match self.source[start + 2..].find("*/") {
            Some(end) => {
                self.pos = start + 2 + end + 2;
                Ok(())
            }
            None => Err(error("unterminated comment: '/*' without '*/'", start)),
        }
    }

    fn word(&mut self, escaped: bool) {
        let start = self.pos;
        if escaped {
            self.pos += 1;
        }
        let name_start = self.pos;
        while self
            .peek(0)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            self.pos += 1;
        }
        let word = &self.source[name_start..self.pos];
        let tok = if escaped {
            Tok::Ident(word.to_owned())
        } else {
            match word {
                "true" => Tok::True,
                "false" => Tok::False,
                "null" => Tok::Null,
                "in" => Tok::In,
                _ => Keyword::from_word(word)
                    .map_or_else(|| Tok::Ident(word.to_owned()), Tok::Keyword),
            }
        };
        self.push(tok, start);
    }

    /// `[0-9]+(\.[0-9]+)?` with an optional duration suffix.
    fn number(&mut self) {
        let start = self.pos;
        self.skip_digits();
        if self.peek(0) == Some(b'.') && self.peek(1).is_some_and(|byte| byte.is_ascii_digit()) {
            self.pos += 1;
            self.skip_digits();
        }
        let digits = &self.source[start..self.pos];
        // Digits with an optional fraction always parse; a failure is
        // impossible, but don't panic over it.
        let value: f64 = digits.parse().unwrap_or(f64::NAN);
        // Same operations, in the same order, as Icinga's lexer actions.
        let value = if self.starts_with("ms") {
            self.pos += 2;
            value / 1000.0
        } else {
            match self.peek(0) {
                Some(b'd') => {
                    self.pos += 1;
                    value * 60.0 * 60.0 * 24.0
                }
                Some(b'h') => {
                    self.pos += 1;
                    value * 60.0 * 60.0
                }
                Some(b'm') => {
                    self.pos += 1;
                    value * 60.0
                }
                Some(b's') => {
                    self.pos += 1;
                    value
                }
                _ => value,
            }
        };
        self.push(Tok::Number(value), start);
    }

    fn skip_digits(&mut self) {
        while self.peek(0).is_some_and(|byte| byte.is_ascii_digit()) {
            self.pos += 1;
        }
    }

    /// A `"…"` string with escape sequences.
    fn string(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        self.pos += 1;
        let mut text: Vec<u8> = Vec::new();
        loop {
            let Some(byte) = self.peek(0) else {
                return Err(error("unterminated string: missing closing '\"'", start));
            };
            match byte {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\n' => {
                    return Err(error(
                        "unterminated string: line breaks inside \"…\" must be written as \\n \
                         (or use a {{{…}}} string)",
                        start,
                    ));
                }
                b'\\' => self.escape(&mut text)?,
                _ => {
                    text.push(byte);
                    self.pos += 1;
                }
            }
        }
        let text = String::from_utf8(text).map_err(|_| {
            error(
                "string is not valid UTF-8 (octal escapes must spell out UTF-8 bytes)",
                start,
            )
        })?;
        self.push(Tok::Str(text), start);
        Ok(())
    }

    fn escape(&mut self, text: &mut Vec<u8>) -> Result<(), ParseError> {
        let backslash = self.pos;
        let Some(next) = self.peek(1) else {
            return Err(error(
                "unterminated string: missing closing '\"'",
                backslash,
            ));
        };
        if next.is_ascii_digit() {
            return self.numeric_escape(text);
        }
        let byte = match next {
            b'n' | b'\n' => b'\n',
            b'\\' => b'\\',
            b'"' => b'"',
            b't' => b'\t',
            b'r' => b'\r',
            b'b' => 0x08,
            b'f' => 0x0c,
            _ => {
                let shown = self
                    .source
                    .get(backslash + 1..)
                    .and_then(|rest| rest.chars().next())
                    .unwrap_or(char::REPLACEMENT_CHARACTER);
                return Err(error(
                    format!(
                        "bad escape sequence '\\{shown}' (valid: \\\" \\\\ \\n \\t \\r \\b \\f and octal \\NNN)"
                    ),
                    backslash,
                ));
            }
        };
        text.push(byte);
        self.pos += 2;
        Ok(())
    }

    /// `\NNN` octal escapes. Flex prefers the longer `\[0-9]+` error rule, so
    /// a run of digits longer than the octal escape (`\18`, `\0777`) is an
    /// error, as in Icinga.
    fn numeric_escape(&mut self, text: &mut Vec<u8>) -> Result<(), ParseError> {
        let backslash = self.pos;
        let digits_start = backslash + 1;
        let digits = self.bytes[digits_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        let octal = self.bytes[digits_start..]
            .iter()
            .take(3)
            .take_while(|byte| (b'0'..=b'7').contains(*byte))
            .count();
        let run = &self.source[backslash..digits_start + digits];
        if octal == 0 || digits > octal {
            return Err(error(format!("bad escape sequence '{run}'"), backslash));
        }
        let value = self.bytes[digits_start..digits_start + octal]
            .iter()
            .fold(0_u32, |acc, byte| acc * 8 + u32::from(byte - b'0'));
        let Ok(byte) = u8::try_from(value) else {
            return Err(error(
                format!("octal escape '{run}' is out of bounds (the maximum is \\377)"),
                backslash,
            ));
        };
        text.push(byte);
        self.pos = digits_start + octal;
        Ok(())
    }

    /// A `{{{…}}}` string: raw text up to the first `}}}`.
    fn heredoc(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let content_start = start + 3;
        let Some(length) = self.source[content_start..].find("}}}") else {
            return Err(error(
                "unterminated multi-line string: '{{{' without '}}}'",
                start,
            ));
        };
        let text = self.source[content_start..content_start + length].to_owned();
        self.pos = content_start + length + 3;
        self.push(Tok::Str(text), start);
        Ok(())
    }

    /// `<` starts an include path when a `>` follows before any space.
    fn angle_string_len(&self) -> Option<usize> {
        self.bytes[self.pos + 1..]
            .iter()
            .position(|byte| matches!(byte, b' ' | b'>'))
            .filter(|&offset| self.bytes[self.pos + 1 + offset] == b'>')
            .map(|offset| offset + 2)
    }

    fn operator(&mut self) -> Result<(), ParseError> {
        let start = self.pos;
        let byte = self.bytes[start];
        let next = self.peek(1);
        match byte {
            b'!' if self.starts_with("!in") => self.symbol(Tok::NotIn, 3),
            b'!' if next == Some(b'=') => self.symbol(Tok::NotEq, 2),
            b'!' => self.symbol(Tok::Bang, 1),
            b'=' if next == Some(b'=') => self.symbol(Tok::EqEq, 2),
            b'=' if next == Some(b'>') => self.symbol(Tok::Arrow, 2),
            b'=' => self.symbol(Tok::Assign, 1),
            b'<' => {
                if let Some(len) = self.angle_string_len() {
                    self.symbol(Tok::AngleString, len);
                } else if next == Some(b'<') {
                    self.symbol(Tok::Shl, 2);
                } else if next == Some(b'=') {
                    self.symbol(Tok::Le, 2);
                } else {
                    self.symbol(Tok::Lt, 1);
                }
            }
            b'>' if next == Some(b'>') => self.symbol(Tok::Shr, 2),
            b'>' if next == Some(b'=') => self.symbol(Tok::Ge, 2),
            b'>' => self.symbol(Tok::Gt, 1),
            b'&' if next == Some(b'&') => self.symbol(Tok::AndAnd, 2),
            b'|' if next == Some(b'|') => self.symbol(Tok::OrOr, 2),
            b'+' | b'-' | b'*' | b'/' | b'%' | b'^' | b'&' | b'|' if next == Some(b'=') => {
                let op = match byte {
                    b'+' => "+=",
                    b'-' => "-=",
                    b'*' => "*=",
                    b'/' => "/=",
                    b'%' => "%=",
                    b'^' => "^=",
                    b'&' => "&=",
                    _ => "|=",
                };
                self.symbol(Tok::CompoundAssign(op), 2);
            }
            b'+' => self.symbol(Tok::Plus, 1),
            b'-' => self.symbol(Tok::Minus, 1),
            b'*' => self.symbol(Tok::Star, 1),
            b'/' => self.symbol(Tok::Slash, 1),
            b'%' => self.symbol(Tok::Percent, 1),
            b'^' => self.symbol(Tok::Caret, 1),
            b'&' => self.symbol(Tok::Amp, 1),
            b'|' => self.symbol(Tok::Pipe, 1),
            b'~' => self.symbol(Tok::Tilde, 1),
            b'.' => self.symbol(Tok::Dot, 1),
            b',' => self.symbol(Tok::Comma, 1),
            b';' => self.symbol(Tok::Semicolon, 1),
            b':' => self.symbol(Tok::Colon, 1),
            b'?' => self.symbol(Tok::Question, 1),
            b'(' => {
                self.newline_modes.push(true);
                self.symbol(Tok::LParen, 1);
            }
            b')' => {
                self.newline_modes.pop();
                self.symbol(Tok::RParen, 1);
            }
            b'[' => self.symbol(Tok::LBracket, 1),
            b']' => self.symbol(Tok::RBracket, 1),
            b'{' if next == Some(b'{') => self.symbol(Tok::LambdaOpen, 2),
            b'{' => {
                self.newline_modes.push(false);
                self.symbol(Tok::LBrace, 1);
            }
            b'}' if next == Some(b'}') => self.symbol(Tok::LambdaClose, 2),
            b'}' => {
                self.newline_modes.pop();
                self.symbol(Tok::RBrace, 1);
            }
            _ => return Err(self.unexpected_character(start)),
        }
        Ok(())
    }

    fn unexpected_character(&self, start: usize) -> ParseError {
        let ch = self
            .source
            .get(start..)
            .and_then(|rest| rest.chars().next())
            .unwrap_or(char::REPLACEMENT_CHARACTER);
        let hint = match ch {
            '\'' => " (strings use double quotes)",
            '@' => " ('@' must be followed by a name, as in @default)",
            '$' => " (runtime macros are not expanded in filters)",
            _ if ch.is_whitespace() => " (only spaces, tabs and line breaks separate tokens)",
            _ => "",
        };
        error(
            format!("unexpected character '{}'{hint}", ch.escape_debug()),
            start,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(source: &str) -> Vec<Tok> {
        let (tokens, error) = tokenize(source);
        assert_eq!(error, None, "{source}");
        tokens.into_iter().map(|token| token.tok).collect()
    }

    fn lex_error(source: &str) -> ParseError {
        let (tokens, error) = tokenize(source);
        let error = error.unwrap();
        let last = tokens.last().unwrap();
        assert_eq!((&last.tok, last.start), (&Tok::Invalid, error.offset));
        error
    }

    #[test]
    fn numbers_and_durations() {
        let cases = [
            ("0", 0.0),
            ("42", 42.0),
            ("27.3", 27.3),
            ("500ms", 0.5),
            ("1.5ms", 0.0015),
            ("30s", 30.0),
            ("5m", 300.0),
            ("2.5m", 150.0),
            ("1h", 3600.0),
            ("1.5h", 5400.0),
            ("2d", 172_800.0),
            ("0.1h", 0.1 * 60.0 * 60.0),
        ];
        for (source, expected) in cases {
            assert_eq!(
                toks(source),
                vec![Tok::Number(expected), Tok::Eof],
                "{source}"
            );
        }
        // `5.` and `.5` are not numbers with fractions.
        assert_eq!(
            toks("5.x"),
            vec![Tok::Number(5.0), Tok::Dot, Tok::Ident("x".into()), Tok::Eof]
        );
        assert_eq!(toks(".5"), vec![Tok::Dot, Tok::Number(5.0), Tok::Eof]);
        // Longest match: `5min` is `5m` followed by the keyword `in`.
        assert_eq!(toks("5min"), vec![Tok::Number(300.0), Tok::In, Tok::Eof]);
        assert_eq!(
            toks("1e5"),
            vec![Tok::Number(1.0), Tok::Ident("e5".into()), Tok::Eof]
        );
        let Tok::Number(huge) = &toks(&"9".repeat(400))[0] else {
            panic!("not a number");
        };
        assert!(huge.is_infinite());
    }

    #[test]
    fn strings_and_escapes() {
        let cases = [
            (r#""hello""#, "hello"),
            (r#""""#, ""),
            (r#""\"te\\st""#, "\"te\\st"),
            (r#""a\nb\tc\rd""#, "a\nb\tc\rd"),
            (r#""\b\f""#, "\u{8}\u{c}"),
            (r#""\101B""#, "AB"),
            (r#""\0""#, "\0"),
            (r#""\7""#, "\u{7}"),
            (r#""\303\244""#, "ä"),
            ("\"line\\\nbreak\"", "line\nbreak"),
            ("\"ä € 🎉\"", "ä € 🎉"),
            ("\"tab\there\"", "tab\there"),
            ("\"cr\rhere\"", "cr\rhere"),
            ("{{{multi\nline \"raw\" \\n}}}", "multi\nline \"raw\" \\n"),
            ("{{{}}}", ""),
            ("{{{a}}}}", "a"),
        ];
        for (source, expected) in cases {
            assert_eq!(toks(source)[0], Tok::Str(expected.to_owned()), "{source}");
        }
        assert_eq!(toks("{{{a}}}}")[1], Tok::RBrace);
    }

    #[test]
    fn string_errors() {
        let cases = [
            (r#"x == "abc"#, 5, "unterminated string"),
            ("\"a\nb\"", 0, "line breaks inside"),
            (r#""\'""#, 1, "bad escape sequence '\\''"),
            (r#""\x41""#, 1, "bad escape sequence '\\x'"),
            (r#""\8""#, 1, "bad escape sequence '\\8'"),
            (r#""\18""#, 1, "bad escape sequence '\\18'"),
            (r#""\0777""#, 1, "bad escape sequence '\\0777'"),
            (r#""\400""#, 1, "out of bounds"),
            (r#""\377""#, 0, "not valid UTF-8"),
            ("\"\\", 1, "unterminated string"),
            ("{{{abc", 0, "unterminated multi-line string"),
            ("a /* b", 2, "unterminated comment"),
        ];
        for (source, offset, message) in cases {
            let error = lex_error(source);
            assert_eq!(error.offset, offset, "{source}: {error:?}");
            assert!(error.message.contains(message), "{source}: {error:?}");
        }
    }

    #[test]
    fn words_keywords_and_escaped_identifiers() {
        assert_eq!(
            toks("host _x x9 @default default in index true false null"),
            vec![
                Tok::Ident("host".into()),
                Tok::Ident("_x".into()),
                Tok::Ident("x9".into()),
                Tok::Ident("default".into()),
                Tok::Keyword(Keyword::Default),
                Tok::In,
                Tok::Ident("index".into()),
                Tok::True,
                Tok::False,
                Tok::Null,
                Tok::Eof,
            ]
        );
        for keyword in Keyword::ALL {
            assert_eq!(toks(keyword.as_str())[0], Tok::Keyword(keyword));
        }
    }

    #[test]
    fn operators_use_longest_match() {
        assert_eq!(
            toks("== != = => < <= << > >= >> && & || | ! ~ + - * / % ^ += -= *= /= %= ^= &= |="),
            vec![
                Tok::EqEq,
                Tok::NotEq,
                Tok::Assign,
                Tok::Arrow,
                Tok::Lt,
                Tok::Le,
                Tok::Shl,
                Tok::Gt,
                Tok::Ge,
                Tok::Shr,
                Tok::AndAnd,
                Tok::Amp,
                Tok::OrOr,
                Tok::Pipe,
                Tok::Bang,
                Tok::Tilde,
                Tok::Plus,
                Tok::Minus,
                Tok::Star,
                Tok::Slash,
                Tok::Percent,
                Tok::Caret,
                Tok::CompoundAssign("+="),
                Tok::CompoundAssign("-="),
                Tok::CompoundAssign("*="),
                Tok::CompoundAssign("/="),
                Tok::CompoundAssign("%="),
                Tok::CompoundAssign("^="),
                Tok::CompoundAssign("&="),
                Tok::CompoundAssign("|="),
                Tok::Eof,
            ]
        );
        assert_eq!(
            toks("( ) [ ] { } , ; : ? . {{ }}"),
            vec![
                Tok::LParen,
                Tok::RParen,
                Tok::LBracket,
                Tok::RBracket,
                Tok::LBrace,
                Tok::RBrace,
                Tok::Comma,
                Tok::Semicolon,
                Tok::Colon,
                Tok::Question,
                Tok::Dot,
                Tok::LambdaOpen,
                Tok::LambdaClose,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn flex_quirks_are_kept() {
        // `!in` wins even when more letters follow.
        assert_eq!(
            toks("!inactive"),
            vec![Tok::NotIn, Tok::Ident("active".into()), Tok::Eof]
        );
        assert_eq!(toks("! inactive")[0], Tok::Bang);
        // `<…>` without spaces is an include path.
        assert_eq!(
            toks("a<b>c"),
            vec![
                Tok::Ident("a".into()),
                Tok::AngleString,
                Tok::Ident("c".into()),
                Tok::Eof
            ]
        );
        assert_eq!(toks("a <b >c")[1], Tok::Lt);
        assert_eq!(toks("<>")[0], Tok::AngleString);
        assert_eq!(toks("1<<2>1")[1], Tok::AngleString);
        // `}}` closes a function, so nested dictionaries need a space.
        assert_eq!(toks("}}")[0], Tok::LambdaClose);
    }

    #[test]
    fn comments_and_whitespace() {
        assert_eq!(
            toks("a # comment\nb // other\n/* block\n comment */ c"),
            vec![
                Tok::Ident("a".into()),
                Tok::Newline,
                Tok::Ident("b".into()),
                Tok::Newline,
                Tok::Ident("c".into()),
                Tok::Eof,
            ]
        );
        assert_eq!(toks("a / / b")[1], Tok::Slash);
    }

    #[test]
    fn line_breaks_are_tokens_except_inside_parentheses() {
        assert_eq!(
            toks("a\r\n\n b"),
            vec![
                Tok::Ident("a".into()),
                Tok::Newline,
                Tok::Ident("b".into()),
                Tok::Eof
            ]
        );
        assert_eq!(
            toks("(a\n&& b)"),
            vec![
                Tok::LParen,
                Tok::Ident("a".into()),
                Tok::AndAnd,
                Tok::Ident("b".into()),
                Tok::RParen,
                Tok::Eof
            ]
        );
        // Dictionaries make them significant again, even inside parentheses.
        assert!(toks("({\n})").contains(&Tok::Newline));
        assert!(!toks("([\n])").contains(&Tok::Newline));
        assert!(toks("[\n]").contains(&Tok::Newline));
    }

    #[test]
    fn token_spans() {
        let (tokens, _) = tokenize("ab  >= \"x\"");
        let spans: Vec<_> = tokens
            .iter()
            .map(|token| (token.start, token.end))
            .collect();
        assert_eq!(spans, vec![(0, 2), (4, 6), (7, 10), (10, 10)]);
    }

    #[test]
    fn unexpected_characters() {
        let cases = [
            ("'a'", "double quotes"),
            ("a @ b", "'@' must be followed"),
            ("$host$", "macros"),
            ("a\u{a0}b", "only spaces"),
            ("ä", "unexpected character 'ä'"),
            ("`", "unexpected character '`'"),
            ("\\", "unexpected character '\\\\'"),
        ];
        for (source, message) in cases {
            let error = lex_error(source);
            assert!(error.message.contains(message), "{source}: {error:?}");
        }
        assert_eq!(lex_error("ab ä").offset, 3);
    }

    #[test]
    fn describes_tokens() {
        assert_eq!(Tok::Ident("x".into()).describe(), "identifier 'x'");
        assert_eq!(Tok::Keyword(Keyword::If).describe(), "keyword 'if'");
        assert_eq!(Tok::NotIn.describe(), "'!in'");
        assert_eq!(Tok::CompoundAssign("+=").describe(), "'+='");
        assert_eq!(Tok::Eof.describe(), "end of filter");
        assert_eq!(Tok::Newline.describe(), "line break");
        assert_eq!(Tok::Number(1.0).describe(), "number");
        assert_eq!(Tok::Str(String::new()).describe(), "string");
        assert_eq!(Tok::AngleString.describe(), "include path");
        assert_eq!(Tok::Invalid.describe(), "invalid input");
    }
}
