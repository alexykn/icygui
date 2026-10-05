//! API filter expressions (`filter`, `filter_vars`): a self-contained subset
//! of the Icinga 2 DSL.
//!
//! **Stand-in module.** The full filter language lives in the `ic-filter`
//! crate, which is written in parallel and is not available here yet. Once it
//! is merged, this module is replaced by a thin adapter over `ic-filter`
//! (`Filter::parse` + a `Scope` implementation over the mock's objects). The
//! rest of the crate only uses [`compile`], [`CompiledFilter::matches`] and the
//! [`Scope`] trait, so the swap stays local to this file.
//!
//! What is supported follows Icinga's grammar and semantics
//! (`config_lexer.ll`, `config_parser.yy`, `expression.cpp`,
//! `value-operators.cpp`):
//! - literals: numbers with duration suffixes (`5m`, `1h`, `30s`, `2d`,
//!   `500ms`), strings with escapes, `{{{ }}}` strings, `true`, `false`,
//!   `null`, arrays `[...]`;
//! - variables (the object under test, its joined objects, `filter_vars`,
//!   Icinga's state constants), member access, indexers, method calls;
//! - operators `!`, unary `-`/`+`, `~`, `*`, `/`, `%`, `+`, `-`, `<<`, `>>`,
//!   `<`, `>`, `<=`, `>=`, `==`, `!=`, `in`, `!in`, `&`, `^`, `|`, `&&`, `||`
//!   with Icinga's precedence and short-circuiting;
//! - functions `match`, `len`, `typeof`, `string`, `number`, `bool`,
//!   `get_time`, and the side-effect-free string, array and dictionary methods.
//!
//! Anything else that is valid Icinga syntax (assignments, dictionaries,
//! lambdas, statements, `regex`, ...) is reported as
//! [`FilterError::Unsupported`], which the HTTP layer turns into a 400 with a
//! clear message. Genuine syntax errors are [`FilterError::Syntax`] and behave
//! like Icinga's compiler errors.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::sync::OnceLock;

/// Why a filter could not be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FilterError {
    /// The expression is not valid Icinga syntax (Icinga rejects it too).
    Syntax(String),
    /// Valid Icinga syntax the mock does not implement.
    Unsupported(String),
}

impl fmt::Display for FilterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(message) => write!(f, "Error: {message}"),
            Self::Unsupported(message) => write!(
                f,
                "ic-mock does not support this filter expression: {message}. \
                 The mock implements a subset of the Icinga DSL until ic-filter replaces it."
            ),
        }
    }
}

/// A runtime error while evaluating a filter (Icinga's `ScriptError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EvalError(pub(crate) String);

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error: {}", self.0)
    }
}

fn eval_error<T>(message: impl Into<String>) -> Result<T, EvalError> {
    Err(EvalError(message.into()))
}

/// An opaque handle to an object of the scope (a host, a service, ...).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ObjectRef {
    /// Icinga type name (`Host`, `Service`, ...).
    pub(crate) type_name: &'static str,
    /// Full object name.
    pub(crate) name: Arc<str>,
}

/// A value during evaluation. Mirrors Icinga's `Value`.
#[derive(Clone, Debug)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(Arc<str>),
    Array(Arc<ArrayValue>),
    Dict(Arc<BTreeMap<String, Value>>),
    Object(ObjectRef),
    Function(Function),
}

/// An array with a lazily built index of its string members, so that
/// `x in names` stays fast for large `filter_vars` arrays.
#[derive(Debug, Default)]
pub(crate) struct ArrayValue {
    items: Vec<Value>,
    strings: OnceLock<HashSet<Arc<str>>>,
}

impl ArrayValue {
    fn new(items: Vec<Value>) -> Self {
        Self {
            items,
            strings: OnceLock::new(),
        }
    }

    fn contains(&self, needle: &Value) -> bool {
        if let Value::String(text) = needle
            && !text.is_empty()
            && self.items.len() > 8
        {
            let index = self.strings.get_or_init(|| {
                self.items
                    .iter()
                    .filter_map(|item| match item {
                        Value::String(s) => Some(Arc::clone(s)),
                        _ => None,
                    })
                    .collect()
            });
            return index.contains(text);
        }
        self.items.iter().any(|item| icinga_eq(item, needle))
    }
}

/// A callable: a global function or a method bound to a value.
#[derive(Clone, Debug)]
pub(crate) enum Function {
    Global(&'static str),
    Method(Box<Value>, &'static str),
}

impl Value {
    pub(crate) fn string(text: &str) -> Self {
        Self::String(Arc::from(text))
    }

    pub(crate) fn array(items: Vec<Value>) -> Self {
        Self::Array(Arc::new(ArrayValue::new(items)))
    }

    /// Converts JSON (as found in `vars`, `filter_vars` and event payloads).
    pub(crate) fn from_json(json: &serde_json::Value) -> Self {
        match json {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => Self::Number(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::String(s) => Self::string(s),
            serde_json::Value::Array(items) => {
                Self::array(items.iter().map(Self::from_json).collect())
            }
            serde_json::Value::Object(map) => Self::Dict(Arc::new(
                map.iter()
                    .map(|(key, value)| (key.clone(), Self::from_json(value)))
                    .collect(),
            )),
        }
    }

    /// Icinga's `Value::ToBool`.
    pub(crate) fn is_truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => *n != 0.0,
            Self::String(s) => !s.is_empty(),
            Self::Array(a) => !a.items.is_empty(),
            Self::Dict(d) => !d.is_empty(),
            Self::Object(_) | Self::Function(_) => true,
        }
    }

    /// Icinga's `IsEmpty`: null or the empty string.
    fn is_empty(&self) -> bool {
        match self {
            Self::Null => true,
            Self::String(s) => s.is_empty(),
            _ => false,
        }
    }

    fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "Empty",
            Self::Bool(_) => "Boolean",
            Self::Number(_) => "Number",
            Self::String(_) => "String",
            Self::Array(_) => "Array",
            Self::Dict(_) => "Dictionary",
            Self::Object(object) => object.type_name,
            Self::Function(_) => "Function",
        }
    }

    /// Icinga's `operator double()`.
    fn to_number(&self) -> Result<f64, EvalError> {
        match self {
            Self::Null => Ok(0.0),
            Self::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Self::Number(n) => Ok(*n),
            Self::String(s) => s.trim().parse::<f64>().map_or_else(
                |_| eval_error(format!("Can't convert '{s}' to a floating point number.")),
                Ok,
            ),
            other => eval_error(format!(
                "Can't convert value of type '{}' to a floating point number.",
                other.type_name()
            )),
        }
    }

    /// Icinga's `operator String()`.
    fn to_icinga_string(&self) -> String {
        match self {
            Self::Null => String::new(),
            Self::Bool(b) => if *b { "true" } else { "false" }.to_owned(),
            Self::Number(n) => format_number(*n),
            Self::String(s) => s.to_string(),
            Self::Array(a) => format!(
                "[ {} ]",
                a.items
                    .iter()
                    .map(Self::to_icinga_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Dict(_) => "Object of type 'Dictionary'".to_owned(),
            Self::Object(object) => object.name.to_string(),
            Self::Function(_) => "Object of type 'Function'".to_owned(),
        }
    }
}

/// Icinga's `Convert::ToString(double)`: integers without decimals,
/// everything else with six decimals.
pub(crate) fn format_number(value: f64) -> String {
    if value.fract() == 0.0 && value.is_finite() {
        format!("{value:.0}")
    } else {
        format!("{value:.6}")
    }
}

/// Icinga's `Value::operator==`.
fn icinga_eq(lhs: &Value, rhs: &Value) -> bool {
    use Value::{Array, Bool, Dict, Null, Number, Object, String};
    match (lhs, rhs) {
        (Number(a), Number(b)) => return a == b,
        (Bool(_) | Number(_), Bool(_) | Number(_)) => {
            return lhs.to_number().ok() == rhs.to_number().ok();
        }
        (String(a), String(b)) => return a == b,
        (String(_) | Null, String(_) | Null) if !(lhs.is_empty() && rhs.is_empty()) => {
            return lhs.to_icinga_string() == rhs.to_icinga_string();
        }
        _ => {}
    }
    if lhs.is_empty() != rhs.is_empty() {
        return false;
    }
    if lhs.is_empty() {
        return true;
    }
    match (lhs, rhs) {
        (Array(a), Array(b)) => {
            Arc::ptr_eq(a, b)
                || (a.items.len() == b.items.len()
                    && a.items.iter().zip(&b.items).all(|(x, y)| icinga_eq(x, y)))
        }
        (Dict(a), Dict(b)) => Arc::ptr_eq(a, b),
        (Object(a), Object(b)) => a == b,
        _ => false,
    }
}

/// Resolves variables and object fields for one evaluation.
pub(crate) trait Scope {
    /// A root variable: the object under test (`host`, `service`, `obj`,
    /// `downtime`, ...), its joined objects, or `event`. `None` falls back to
    /// `filter_vars`, then Icinga's constants and functions.
    fn variable(&self, name: &str) -> Option<Value>;

    /// A field of an object handle. `Ok(None)` means the type has no such
    /// field, which is an error in Icinga.
    ///
    /// # Errors
    /// Fields hidden from users (`no_user_view`) are an error in sandbox mode.
    fn field(&self, object: &ObjectRef, name: &str) -> Result<Option<Value>, EvalError>;
}

/// A compiled filter with its bound `filter_vars`.
#[derive(Debug)]
pub(crate) struct CompiledFilter {
    expr: Option<Expr>,
    vars: BTreeMap<String, Value>,
}

/// Compiles a filter and binds `filter_vars` (a JSON object).
///
/// An empty source compiles to a filter that matches nothing, like Icinga's
/// empty statement list.
///
/// # Errors
/// [`FilterError::Syntax`] for invalid syntax, [`FilterError::Unsupported`]
/// for valid Icinga syntax this stand-in does not implement.
pub(crate) fn compile(
    source: &str,
    filter_vars: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Result<CompiledFilter, FilterError> {
    let tokens = lex(source)?;
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.parse_program()?;
    let vars = filter_vars
        .map(|map| {
            map.iter()
                .map(|(key, value)| (key.clone(), Value::from_json(value)))
                .collect()
        })
        .unwrap_or_default();
    Ok(CompiledFilter { expr, vars })
}

impl CompiledFilter {
    /// Evaluates the filter and converts the result to a boolean.
    ///
    /// # Errors
    /// Runtime errors (undefined variables, invalid field access, type
    /// errors), which Icinga reports as failing the whole request.
    pub(crate) fn matches(&self, scope: &dyn Scope) -> Result<bool, EvalError> {
        match &self.expr {
            None => Ok(false),
            Some(expr) => Ok(Evaluator {
                scope,
                vars: &self.vars,
            }
            .eval(expr)?
            .is_truthy()),
        }
    }
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Str(String),
    Ident(String),
    True,
    False,
    Null,
    /// A keyword of the full language that filters can't use here.
    Keyword(String),
    Op(&'static str),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
    Newline,
    /// Valid Icinga punctuation the stand-in does not support (`{`, `=`, ...).
    Unsupported(&'static str),
}

const KEYWORDS: &[&str] = &[
    "object",
    "template",
    "include",
    "include_recursive",
    "include_zones",
    "library",
    "const",
    "var",
    "this",
    "globals",
    "locals",
    "use",
    "using",
    "apply",
    "default",
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
    "ignore_on_error",
    "current_filename",
    "current_line",
    "debugger",
    "namespace",
];

/// Operators, longest first (the lexer picks the longest match like flex).
const OPERATORS: &[(&str, &str)] = &[
    ("!in", "!in"),
    ("<<", "<<"),
    (">>", ">>"),
    ("<=", "<="),
    (">=", ">="),
    ("==", "=="),
    ("!=", "!="),
    ("&&", "&&"),
    ("||", "||"),
    ("+", "+"),
    ("-", "-"),
    ("*", "*"),
    ("/", "/"),
    ("%", "%"),
    ("^", "^"),
    ("&", "&"),
    ("|", "|"),
    ("<", "<"),
    (">", ">"),
    ("!", "!"),
    ("~", "~"),
];

/// Valid Icinga punctuation this stand-in does not implement (assignments,
/// blocks, lambdas, statements). Longest first.
const UNSUPPORTED_PUNCT: &[&str] = &["=>", "{{", "}}", "{", "}", "?", ":", ";"];

#[expect(
    clippy::too_many_lines,
    reason = "one flat match over the lexer rules reads best"
)]
fn lex(source: &str) -> Result<Vec<Token>, FilterError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let rest = &source[i..];
        let c = bytes[i];
        // Whitespace and comments.
        if c == b' ' || c == b'\t' {
            i += 1;
            continue;
        }
        if c == b'\r' || c == b'\n' {
            tokens.push(Token::Newline);
            i += 1;
            continue;
        }
        if rest.starts_with("//") || c == b'#' {
            i += rest.find('\n').unwrap_or(rest.len());
            continue;
        }
        if let Some(comment) = rest.strip_prefix("/*") {
            let Some(end) = comment.find("*/") else {
                return Err(FilterError::Syntax("End-of-file while in comment".into()));
            };
            i += end + 4;
            continue;
        }
        // Heredoc strings.
        if let Some(body) = rest.strip_prefix("{{{") {
            let Some(end) = body.find("}}}") else {
                return Err(FilterError::Syntax(
                    "End-of-file while in string literal".into(),
                ));
            };
            tokens.push(Token::Str(body[..end].to_owned()));
            i += 3 + end + 3;
            continue;
        }
        if c == b'"' {
            let (text, consumed) = lex_string(&rest[1..])?;
            tokens.push(Token::Str(text));
            i += 1 + consumed;
            continue;
        }
        if c.is_ascii_digit() {
            let (value, consumed) = lex_number(rest);
            tokens.push(Token::Number(value));
            i += consumed;
            continue;
        }
        let ident_start = |b: u8| b.is_ascii_alphabetic() || b == b'_';
        if ident_start(c) || (c == b'@' && bytes.get(i + 1).is_some_and(|b| ident_start(*b))) {
            // `@name` is an identifier that may shadow a keyword.
            let escaped = c == b'@';
            let start = usize::from(escaped);
            let len = rest[start..]
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
                .count();
            let word = &rest[start..start + len];
            tokens.push(match word {
                w if escaped => Token::Ident(w.to_owned()),
                "true" => Token::True,
                "false" => Token::False,
                "null" => Token::Null,
                "in" => Token::Op("in"),
                w if KEYWORDS.contains(&w) => Token::Keyword(w.to_owned()),
                w => Token::Ident(w.to_owned()),
            });
            i += start + len;
            continue;
        }
        // flex's `\<[^ \>]*\>`: an angle string (only used by `include`).
        if c == b'<'
            && rest[1..]
                .find([' ', '>'])
                .is_some_and(|at| rest.as_bytes()[1 + at] == b'>')
        {
            return Err(FilterError::Unsupported(
                "angle-bracket strings (`<...>`)".into(),
            ));
        }
        if let Some((text, op)) = OPERATORS.iter().find(|(text, _)| rest.starts_with(text)) {
            tokens.push(Token::Op(op));
            i += text.len();
            continue;
        }
        if let Some(punct) = UNSUPPORTED_PUNCT.iter().find(|p| rest.starts_with(**p)) {
            tokens.push(Token::Unsupported(punct));
            i += punct.len();
            continue;
        }
        if c == b'=' {
            tokens.push(Token::Unsupported("="));
            i += 1;
            continue;
        }
        let token = match c {
            b'(' => Token::LParen,
            b')' => Token::RParen,
            b'[' => Token::LBracket,
            b']' => Token::RBracket,
            b',' => Token::Comma,
            b'.' => Token::Dot,
            _ => {
                let ch = rest.chars().next().unwrap_or('?');
                return Err(FilterError::Syntax(format!(
                    "syntax error, unexpected '{ch}'"
                )));
            }
        };
        tokens.push(token);
        i += 1;
    }
    Ok(tokens)
}

/// Lexes the body of a `"..."` string; returns the text and the number of
/// bytes consumed including the closing quote.
fn lex_string(body: &str) -> Result<(String, usize), FilterError> {
    let mut text = String::new();
    let mut chars = body.char_indices();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '"' => return Ok((text, index + 1)),
            '\n' => return Err(FilterError::Syntax("Unterminated string literal".into())),
            '\\' => {
                let Some((_, escaped)) = chars.next() else {
                    break;
                };
                match escaped {
                    'n' | '\n' => text.push('\n'),
                    't' => text.push('\t'),
                    'r' => text.push('\r'),
                    'b' => text.push('\u{8}'),
                    'f' => text.push('\u{c}'),
                    '\\' => text.push('\\'),
                    '"' => text.push('"'),
                    '0'..='7' => {
                        let mut value = escaped.to_digit(8).unwrap_or(0);
                        for _ in 0..2 {
                            let mut lookahead = chars.clone();
                            match lookahead.next() {
                                Some((_, d @ '0'..='7')) => {
                                    value = value * 8 + d.to_digit(8).unwrap_or(0);
                                    chars = lookahead;
                                }
                                _ => break,
                            }
                        }
                        let Ok(byte) = u8::try_from(value) else {
                            return Err(FilterError::Syntax(format!(
                                "Constant is out of bounds: \\{value:o}"
                            )));
                        };
                        text.push(char::from(byte));
                    }
                    other => {
                        return Err(FilterError::Syntax(format!(
                            "Bad escape sequence found: \\{other}"
                        )));
                    }
                }
            }
            other => text.push(other),
        }
    }
    Err(FilterError::Syntax(
        "End-of-file while in string literal".into(),
    ))
}

/// Lexes `[0-9]+(\.[0-9]+)?` with an optional duration suffix.
fn lex_number(rest: &str) -> (f64, usize) {
    let bytes = rest.as_bytes();
    let mut end = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
    if bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end += 1 + bytes[end + 1..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
    }
    let value = rest[..end].parse::<f64>().unwrap_or(0.0);
    let suffix = &rest[end..];
    let (factor, len) = if suffix.starts_with("ms") {
        (0.001, 2)
    } else {
        match bytes.get(end) {
            Some(b'd') => (86_400.0, 1),
            Some(b'h') => (3_600.0, 1),
            Some(b'm') => (60.0, 1),
            Some(b's') => (1.0, 1),
            _ => (1.0, 0),
        }
    };
    (value * factor, end + len)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Expr {
    Literal(Value),
    Array(Vec<Expr>),
    Variable(String),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

/// Binary operator precedence (higher binds tighter), from `config_parser.yy`.
fn precedence(op: &str) -> Option<u8> {
    Some(match op {
        "||" => 1,
        "&&" => 2,
        "|" => 3,
        "^" => 4,
        "&" => 5,
        "==" | "!=" => 6,
        "in" | "!in" => 7,
        "<" | "<=" | ">" | ">=" => 8,
        "<<" | ">>" => 9,
        "+" | "-" => 10,
        "*" | "/" | "%" => 11,
        _ => return None,
    })
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        token
    }

    fn skip_newlines(&mut self) {
        while matches!(self.peek(), Some(Token::Newline)) {
            self.pos += 1;
        }
    }

    fn parse_program(&mut self) -> Result<Option<Expr>, FilterError> {
        self.skip_newlines();
        if self.peek().is_none() {
            return Ok(None);
        }
        let expr = self.parse_binary(1)?;
        self.skip_newlines();
        match self.peek() {
            None => Ok(Some(expr)),
            Some(Token::Unsupported(";")) => Err(FilterError::Unsupported(
                "more than one statement (only a single expression is supported)".into(),
            )),
            Some(token) => Err(unexpected(token)),
        }
    }

    fn parse_binary(&mut self, min_prec: u8) -> Result<Expr, FilterError> {
        let mut lhs = self.parse_unary()?;
        while let Some(Token::Op(op)) = self.peek() {
            let op = *op;
            let Some(prec) = precedence(op) else { break };
            if prec < min_prec {
                break;
            }
            self.pos += 1;
            self.skip_newlines_in_expression();
            let rhs = self.parse_binary(prec + 1)?;
            lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// Icinga ignores newlines inside parentheses only; after a binary
    /// operator a newline is a syntax error there too, but accepting it is
    /// harmless for API filters.
    fn skip_newlines_in_expression(&mut self) {
        self.skip_newlines();
    }

    fn parse_unary(&mut self) -> Result<Expr, FilterError> {
        match self.peek() {
            Some(Token::Op(op @ ("!" | "-" | "+" | "~"))) => {
                let op = *op;
                self.pos += 1;
                let operand = self.parse_unary()?;
                Ok(Expr::Unary(op, Box::new(operand)))
            }
            Some(Token::Op("&" | "*")) => Err(FilterError::Unsupported(
                "reference operators (`&x`, `*x`)".into(),
            )),
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, FilterError> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                Some(Token::Dot) => {
                    self.pos += 1;
                    match self.next() {
                        Some(Token::Ident(name) | Token::Keyword(name)) => {
                            expr = Expr::Member(Box::new(expr), name);
                        }
                        Some(Token::Op("in")) => {
                            expr = Expr::Member(Box::new(expr), "in".to_owned());
                        }
                        Some(token) => return Err(unexpected(&token)),
                        None => return Err(eof()),
                    }
                }
                Some(Token::LBracket) => {
                    self.pos += 1;
                    let index = self.parse_binary(1)?;
                    match self.next() {
                        Some(Token::RBracket) => {}
                        Some(token) => return Err(unexpected(&token)),
                        None => return Err(eof()),
                    }
                    expr = Expr::Index(Box::new(expr), Box::new(index));
                }
                Some(Token::LParen) => {
                    self.pos += 1;
                    let args = self.parse_list(&Token::RParen)?;
                    expr = Expr::Call(Box::new(expr), args);
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_list(&mut self, close: &Token) -> Result<Vec<Expr>, FilterError> {
        let mut items = Vec::new();
        self.skip_newlines();
        if self.peek() == Some(close) {
            self.pos += 1;
            return Ok(items);
        }
        loop {
            self.skip_newlines();
            items.push(self.parse_binary(1)?);
            self.skip_newlines();
            match self.next() {
                Some(Token::Comma) => {
                    self.skip_newlines();
                    // Icinga allows a trailing comma in arrays.
                    if self.peek() == Some(close) {
                        self.pos += 1;
                        return Ok(items);
                    }
                }
                Some(token) if &token == close => return Ok(items),
                Some(token) => return Err(unexpected(&token)),
                None => return Err(eof()),
            }
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, FilterError> {
        match self.next() {
            Some(Token::Number(n)) => Ok(Expr::Literal(Value::Number(n))),
            Some(Token::Str(s)) => Ok(Expr::Literal(Value::string(&s))),
            Some(Token::True) => Ok(Expr::Literal(Value::Bool(true))),
            Some(Token::False) => Ok(Expr::Literal(Value::Bool(false))),
            Some(Token::Null) => Ok(Expr::Literal(Value::Null)),
            Some(Token::Ident(name)) => Ok(Expr::Variable(name)),
            Some(Token::LParen) => {
                self.skip_newlines();
                let expr = self.parse_binary(1)?;
                self.skip_newlines();
                match self.next() {
                    Some(Token::RParen) => Ok(expr),
                    Some(token) => Err(unexpected(&token)),
                    None => Err(eof()),
                }
            }
            Some(Token::LBracket) => Ok(Expr::Array(self.parse_list(&Token::RBracket)?)),
            Some(Token::Keyword(word)) => Err(FilterError::Unsupported(format!(
                "the `{word}` keyword (only expressions are supported in filters)"
            ))),
            Some(Token::Unsupported(punct)) => Err(unsupported_punct(punct)),
            Some(token) => Err(unexpected(&token)),
            None => Err(eof()),
        }
    }
}

fn unsupported_punct(punct: &str) -> FilterError {
    FilterError::Unsupported(match punct {
        "{" | "}" => "dictionaries and blocks (`{ ... }`)".to_owned(),
        "{{" | "}}" => "lambdas (`{{ ... }}`)".to_owned(),
        "?" | ":" => "the conditional operator".to_owned(),
        ";" => "more than one statement".to_owned(),
        "@" => "`@` identifiers".to_owned(),
        other => format!("assignments (`{other}`)"),
    })
}

fn unexpected(token: &Token) -> FilterError {
    match token {
        Token::Unsupported(punct) => unsupported_punct(punct),
        Token::Keyword(word) => FilterError::Unsupported(format!(
            "the `{word}` keyword (only expressions are supported in filters)"
        )),
        other => FilterError::Syntax(format!("syntax error, unexpected {}", describe(other))),
    }
}

fn eof() -> FilterError {
    FilterError::Syntax("syntax error, unexpected end of file".into())
}

fn describe(token: &Token) -> String {
    match token {
        Token::Number(n) => format!("number ({})", format_number(*n)),
        Token::Str(s) => format!("string (\"{s}\")"),
        Token::Ident(name) => format!("identifier ({name})"),
        Token::True | Token::False => "boolean".to_owned(),
        Token::Null => "null".to_owned(),
        Token::Keyword(word) => word.clone(),
        Token::Op(op) => format!("'{op}'"),
        Token::LParen => "'('".to_owned(),
        Token::RParen => "')'".to_owned(),
        Token::LBracket => "'['".to_owned(),
        Token::RBracket => "']'".to_owned(),
        Token::Comma => "','".to_owned(),
        Token::Dot => "'.'".to_owned(),
        Token::Newline => "newline".to_owned(),
        Token::Unsupported(p) => format!("'{p}'"),
    }
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

/// Icinga constants and namespaced values filters commonly use.
#[expect(
    clippy::match_same_arms,
    reason = "one arm per group of Icinga constants keeps the table readable"
)]
fn constant(name: &str) -> Option<Value> {
    let number = |n: f64| Some(Value::Number(n));
    match name {
        "ServiceOK" | "HostUp" | "MatchAll" | "AcknowledgementNone" => number(0.0),
        "ServiceWarning" | "HostDown" | "MatchAny" | "AcknowledgementNormal" => number(1.0),
        "ServiceCritical" | "AcknowledgementSticky" => number(2.0),
        "ServiceUnknown" => number(3.0),
        // State filters (bit flags).
        "OK" => number(1.0),
        "Warning" => number(2.0),
        "Critical" => number(4.0),
        "Unknown" => number(8.0),
        "Up" => number(16.0),
        "Down" => number(32.0),
        // Notification type filters (bit flags).
        "DowntimeStart" => number(1.0),
        "DowntimeEnd" => number(2.0),
        "DowntimeRemoved" => number(4.0),
        "Custom" => number(8.0),
        "Acknowledgement" => number(16.0),
        "Problem" => number(32.0),
        "Recovery" => number(64.0),
        "FlappingStart" => number(128.0),
        "FlappingEnd" => number(256.0),
        "DowntimeNoChildren" | "DowntimeTriggeredChildren" | "DowntimeNonTriggeredChildren" => {
            Some(Value::string(name))
        }
        _ => None,
    }
}

const GLOBAL_FUNCTIONS: &[&str] = &[
    "match", "len", "typeof", "string", "number", "bool", "get_time",
];

/// Valid Icinga global functions this stand-in does not implement.
const UNSUPPORTED_FUNCTIONS: &[&str] = &[
    "regex",
    "cidr_match",
    "intersection",
    "union",
    "range",
    "keys",
    "random",
    "basename",
    "dirname",
    "escape_shell_arg",
    "escape_shell_cmd",
    "escape_create_process_arg",
    "parse_performance_data",
    "path_exists",
    "glob",
    "glob_recursive",
    "sleep",
    "log",
    "exit",
    "assert",
    "get_host",
    "get_service",
    "get_services",
    "get_user",
    "get_object",
    "get_objects",
    "get_check_command",
    "get_event_command",
    "get_template",
    "get_templates",
    "get_time_period",
    "get_host_group",
    "get_service_group",
    "get_user_group",
    "get_notification_command",
    "macro",
    "msi_get_component_path",
    "track_parents",
];

const STRING_METHODS: &[&str] = &[
    "len",
    "to_string",
    "substr",
    "upper",
    "lower",
    "split",
    "find",
    "contains",
    "replace",
    "reverse",
    "trim",
];
const ARRAY_METHODS: &[&str] = &[
    "len",
    "contains",
    "join",
    "reverse",
    "unique",
    "shallow_clone",
];
const ARRAY_UNSAFE_METHODS: &[&str] = &[
    "set", "get", "add", "remove", "clear", "freeze", "sort", "map", "reduce", "filter", "any",
    "all",
];
const DICT_METHODS: &[&str] = &["len", "contains", "keys", "values", "shallow_clone"];
const DICT_UNSAFE_METHODS: &[&str] = &["set", "get", "remove", "clear", "freeze"];

struct Evaluator<'a> {
    scope: &'a dyn Scope,
    vars: &'a BTreeMap<String, Value>,
}

impl Evaluator<'_> {
    fn eval(&self, expr: &Expr) -> Result<Value, EvalError> {
        match expr {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::Array(items) => Ok(Value::array(
                items
                    .iter()
                    .map(|item| self.eval(item))
                    .collect::<Result<_, _>>()?,
            )),
            Expr::Variable(name) => self.variable(name),
            Expr::Member(target, field) => {
                let target = self.eval(target)?;
                self.get_field(&target, field)
            }
            Expr::Index(target, index) => {
                let target = self.eval(target)?;
                let index = self.eval(index)?;
                self.index(&target, &index)
            }
            Expr::Call(callee, args) => {
                let callee = self.eval(callee)?;
                let args = args
                    .iter()
                    .map(|arg| self.eval(arg))
                    .collect::<Result<Vec<_>, _>>()?;
                Self::call(&callee, &args)
            }
            Expr::Unary(op, operand) => {
                let value = self.eval(operand)?;
                unary(op, &value)
            }
            Expr::Binary("&&", lhs, rhs) => {
                let left = self.eval(lhs)?;
                if left.is_truthy() {
                    self.eval(rhs)
                } else {
                    Ok(left)
                }
            }
            Expr::Binary("||", lhs, rhs) => {
                let left = self.eval(lhs)?;
                if left.is_truthy() {
                    Ok(left)
                } else {
                    self.eval(rhs)
                }
            }
            Expr::Binary(op @ ("in" | "!in"), lhs, rhs) => {
                let haystack = self.eval(rhs)?;
                let negate = *op == "!in";
                if haystack.is_empty() {
                    return Ok(Value::Bool(negate));
                }
                let Value::Array(array) = &haystack else {
                    return eval_error(format!(
                        "Invalid right side argument for 'in' operator: {}",
                        json_text(&haystack)
                    ));
                };
                let needle = self.eval(lhs)?;
                Ok(Value::Bool(array.contains(&needle) != negate))
            }
            Expr::Binary(op, lhs, rhs) => {
                let left = self.eval(lhs)?;
                let right = self.eval(rhs)?;
                binary(op, &left, &right)
            }
        }
    }

    fn variable(&self, name: &str) -> Result<Value, EvalError> {
        if let Some(value) = self.scope.variable(name) {
            return Ok(value);
        }
        if let Some(value) = self.vars.get(name) {
            return Ok(value.clone());
        }
        if let Some(value) = constant(name) {
            return Ok(value);
        }
        if let Some(function) = GLOBAL_FUNCTIONS.iter().find(|f| **f == name) {
            return Ok(Value::Function(Function::Global(function)));
        }
        if UNSUPPORTED_FUNCTIONS.contains(&name) {
            return eval_error(format!(
                "ic-mock does not support the '{name}' function in filters yet"
            ));
        }
        eval_error(format!(
            "Tried to access undefined script variable '{name}'"
        ))
    }

    fn get_field(&self, target: &Value, field: &str) -> Result<Value, EvalError> {
        match target {
            Value::Null => Ok(Value::Null),
            Value::Object(object) => match self.scope.field(object, field)? {
                Some(value) => Ok(value),
                None => eval_error(format!(
                    "Invalid field access (for value of type '{}'): '{field}'",
                    object.type_name
                )),
            },
            Value::Dict(dict) => {
                match dict.get(field) {
                    Some(value) => Ok(value.clone()),
                    None => Ok(method(target, field, DICT_METHODS, DICT_UNSAFE_METHODS)
                        .unwrap_or(Value::Null)),
                }
            }
            Value::Array(_) => method(target, field, ARRAY_METHODS, ARRAY_UNSAFE_METHODS)
                .map_or_else(|| invalid_field(target, field), Ok),
            Value::String(_) => method(target, field, STRING_METHODS, &[])
                .map_or_else(|| invalid_field(target, field), Ok),
            Value::Bool(_) | Value::Number(_) | Value::Function(_) => {
                method(target, field, &["to_string"], &[])
                    .map_or_else(|| invalid_field(target, field), Ok)
            }
        }
    }

    fn index(&self, target: &Value, index: &Value) -> Result<Value, EvalError> {
        match target {
            Value::Array(array) => {
                let position = index.to_number()?;
                let Some(item) = (position >= 0.0 && position.fract() == 0.0)
                    .then(|| {
                        #[expect(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "checked: a non-negative integer; huge values saturate and miss"
                        )]
                        let position = position as usize;
                        position
                    })
                    .and_then(|position| array.items.get(position))
                else {
                    return eval_error(format!(
                        "Array index '{}' is out of bounds.",
                        format_number(position)
                    ));
                };
                Ok(item.clone())
            }
            _ => self.get_field(target, &index.to_icinga_string()),
        }
    }

    fn call(callee: &Value, args: &[Value]) -> Result<Value, EvalError> {
        match callee {
            Value::Function(Function::Global(name)) => call_global(name, args),
            Value::Function(Function::Method(receiver, name)) => call_method(receiver, name, args),
            _ => eval_error("Argument is not a callable object."),
        }
    }
}

fn method(
    target: &Value,
    name: &str,
    safe: &[&'static str],
    unsafe_methods: &[&'static str],
) -> Option<Value> {
    if let Some(found) = safe.iter().find(|m| **m == name) {
        return Some(Value::Function(Function::Method(
            Box::new(target.clone()),
            found,
        )));
    }
    unsafe_methods
        .iter()
        .find(|m| **m == name)
        .map(|_| Value::Function(Function::Method(Box::new(target.clone()), "!unsafe")))
}

fn invalid_field(target: &Value, field: &str) -> Result<Value, EvalError> {
    eval_error(format!(
        "Invalid field access (for value of type '{}'): '{field}'",
        target.type_name()
    ))
}

fn json_text(value: &Value) -> String {
    match value {
        Value::String(s) => serde_json::Value::String(s.to_string()).to_string(),
        Value::Number(n) => format_number(*n),
        Value::Bool(b) => b.to_string(),
        other => other.to_icinga_string(),
    }
}

fn unary(op: &str, value: &Value) -> Result<Value, EvalError> {
    match op {
        "!" => Ok(Value::Bool(!value.is_truthy())),
        "-" => Ok(Value::Number(-value.to_number()?)),
        "+" => Ok(Value::Number(value.to_number()?)),
        "~" => Ok(Value::Number(f64_from_i64(!f64_to_i64(value.to_number()?)))),
        _ => eval_error(format!("unknown unary operator '{op}'")),
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "Icinga's integer operators truncate doubles the same way (static_cast<int>)"
)]
fn f64_to_i64(value: f64) -> i64 {
    value as i64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Icinga stores every number as a double"
)]
fn f64_from_i64(value: i64) -> f64 {
    value as f64
}

fn numeric_operands(op: &str, lhs: &Value, rhs: &Value) -> Result<(f64, f64), EvalError> {
    let numeric = |v: &Value| matches!(v, Value::Null | Value::Number(_));
    if numeric(lhs) && numeric(rhs) && !(matches!(lhs, Value::Null) && matches!(rhs, Value::Null)) {
        Ok((lhs.to_number()?, rhs.to_number()?))
    } else {
        cannot_apply(op, lhs, rhs)
    }
}

fn cannot_apply<T>(op: &str, lhs: &Value, rhs: &Value) -> Result<T, EvalError> {
    eval_error(format!(
        "Operator {op} cannot be applied to values of type '{}' and '{}'",
        lhs.type_name(),
        rhs.type_name()
    ))
}

fn binary(op: &str, lhs: &Value, rhs: &Value) -> Result<Value, EvalError> {
    match op {
        "==" => Ok(Value::Bool(icinga_eq(lhs, rhs))),
        "!=" => Ok(Value::Bool(!icinga_eq(lhs, rhs))),
        "<" | ">" | "<=" | ">=" => compare(op, lhs, rhs).map(Value::Bool),
        "+" => add(lhs, rhs),
        "-" => {
            if let (Value::Array(a), Value::Array(b)) = (lhs, rhs) {
                return Ok(Value::array(
                    a.items
                        .iter()
                        .filter(|item| !b.items.iter().any(|other| icinga_eq(item, other)))
                        .cloned()
                        .collect(),
                ));
            }
            let (a, b) = numeric_operands(op, lhs, rhs)?;
            Ok(Value::Number(a - b))
        }
        "*" => {
            let (a, b) = numeric_operands(op, lhs, rhs)?;
            Ok(Value::Number(a * b))
        }
        "/" => {
            if matches!(rhs, Value::Null) {
                return eval_error("Right-hand side argument for operator / is Empty.");
            }
            let (a, b) = numeric_operands(op, lhs, rhs)?;
            if b == 0.0 {
                return eval_error("Right-hand side argument for operator / is 0.");
            }
            Ok(Value::Number(a / b))
        }
        "%" => {
            if matches!(rhs, Value::Null) {
                return eval_error("Right-hand side argument for operator % is Empty.");
            }
            let (a, b) = numeric_operands(op, lhs, rhs)?;
            let (a, b) = (f64_to_i64(a), f64_to_i64(b));
            if b == 0 {
                return eval_error("Right-hand side argument for operator % is 0.");
            }
            Ok(Value::Number(f64_from_i64(a % b)))
        }
        "&" | "|" | "^" | "<<" | ">>" => {
            let (a, b) = numeric_operands(op, lhs, rhs)?;
            let (a, b) = (f64_to_i64(a), f64_to_i64(b));
            let shift = u32::try_from(b.clamp(0, 63)).unwrap_or(0);
            Ok(Value::Number(f64_from_i64(match op {
                "&" => a & b,
                "|" => a | b,
                "^" => a ^ b,
                "<<" => a << shift,
                _ => a >> shift,
            })))
        }
        _ => eval_error(format!("unknown operator '{op}'")),
    }
}

fn compare(op: &str, lhs: &Value, rhs: &Value) -> Result<bool, EvalError> {
    use std::cmp::Ordering;
    let ordering = match (lhs, rhs) {
        (Value::String(a), Value::String(b)) => a.as_bytes().cmp(b.as_bytes()),
        (Value::Array(a), Value::Array(b)) if matches!(op, "<" | ">") => {
            let mut result = Ordering::Equal;
            for i in 0..a.items.len().max(b.items.len()) {
                let x = a.items.get(i).cloned().unwrap_or(Value::Null);
                let y = b.items.get(i).cloned().unwrap_or(Value::Null);
                if compare("<", &x, &y)? {
                    result = Ordering::Less;
                    break;
                }
                if compare(">", &x, &y)? {
                    result = Ordering::Greater;
                    break;
                }
            }
            result
        }
        _ => {
            let (a, b) = numeric_operands(op, lhs, rhs)?;
            a.partial_cmp(&b).unwrap_or(Ordering::Equal)
        }
    };
    Ok(match op {
        "<" => ordering == Ordering::Less,
        ">" => ordering == Ordering::Greater,
        "<=" => ordering != Ordering::Greater,
        _ => ordering != Ordering::Less,
    })
}

fn add(lhs: &Value, rhs: &Value) -> Result<Value, EvalError> {
    let numeric = |v: &Value| matches!(v, Value::Null | Value::Number(_));
    let stringish = |v: &Value| matches!(v, Value::Null | Value::Number(_) | Value::String(_));
    let both_null = matches!(lhs, Value::Null) && matches!(rhs, Value::Null);
    if numeric(lhs) && numeric(rhs) && !both_null {
        return Ok(Value::Number(lhs.to_number()? + rhs.to_number()?));
    }
    if stringish(lhs) && stringish(rhs) && !both_null {
        return Ok(Value::string(&format!(
            "{}{}",
            lhs.to_icinga_string(),
            rhs.to_icinga_string()
        )));
    }
    match (lhs, rhs) {
        (Value::Array(_) | Value::Null, Value::Array(_) | Value::Null) if !both_null => {
            let mut items = Vec::new();
            for side in [lhs, rhs] {
                if let Value::Array(a) = side {
                    items.extend(a.items.iter().cloned());
                }
            }
            Ok(Value::array(items))
        }
        (Value::Dict(_) | Value::Null, Value::Dict(_) | Value::Null) if !both_null => {
            let mut merged = BTreeMap::new();
            for side in [lhs, rhs] {
                if let Value::Dict(d) = side {
                    merged.extend(d.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
            Ok(Value::Dict(Arc::new(merged)))
        }
        _ => cannot_apply("+", lhs, rhs),
    }
}

/// Glob matching as `Utility::Match`: `*` matches any sequence, `?` one
/// character.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            backtrack = Some((p, t));
            p += 1;
        } else if let Some((star, matched)) = backtrack {
            p = star + 1;
            t = matched + 1;
            backtrack = Some((star, matched + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Null)
}

fn call_global(name: &str, args: &[Value]) -> Result<Value, EvalError> {
    match name {
        "match" => {
            if args.len() < 2 {
                return eval_error("Too few arguments for match()");
            }
            let pattern = args[0].to_icinga_string();
            let mode = arg(args, 2).to_number()?;
            let match_any = (mode - 1.0).abs() < f64::EPSILON;
            match &args[1] {
                Value::Array(texts) => {
                    if texts.items.is_empty() {
                        return Ok(Value::Bool(false));
                    }
                    for text in &texts.items {
                        let matched = glob_match(&pattern, &text.to_icinga_string());
                        if match_any && matched {
                            return Ok(Value::Bool(true));
                        }
                        if !match_any && !matched {
                            return Ok(Value::Bool(false));
                        }
                    }
                    Ok(Value::Bool(!match_any))
                }
                text => Ok(Value::Bool(glob_match(&pattern, &text.to_icinga_string()))),
            }
        }
        "len" => Ok(len(&arg(args, 0))),
        "typeof" => Ok(Value::string(arg(args, 0).type_name())),
        "string" => Ok(Value::string(&arg(args, 0).to_icinga_string())),
        "number" => Ok(Value::Number(arg(args, 0).to_number()?)),
        "bool" => Ok(Value::Bool(arg(args, 0).is_truthy())),
        "get_time" => Ok(Value::Number(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64()),
        )),
        other => eval_error(format!("Tried to call unknown function '{other}'")),
    }
}

fn len(value: &Value) -> Value {
    let count = match value {
        Value::Null => 0,
        Value::String(s) => s.len(),
        Value::Array(a) => a.items.len(),
        Value::Dict(d) => d.len(),
        Value::Object(_) | Value::Function(_) => 1,
        Value::Bool(_) | Value::Number(_) => value.to_icinga_string().len(),
    };
    #[expect(
        clippy::cast_precision_loss,
        reason = "lengths of filter values are far below 2^52"
    )]
    Value::Number(count as f64)
}

fn call_method(receiver: &Value, name: &str, args: &[Value]) -> Result<Value, EvalError> {
    if name == "!unsafe" {
        return eval_error("Function is not marked as safe for sandbox mode.");
    }
    match (receiver, name) {
        (_, "len") => Ok(len(receiver)),
        (_, "to_string") => Ok(Value::string(&receiver.to_icinga_string())),
        (Value::String(s), "contains") => Ok(Value::Bool(
            s.contains(arg(args, 0).to_icinga_string().as_str()),
        )),
        (Value::String(s), "upper") => Ok(Value::string(&s.to_uppercase())),
        (Value::String(s), "lower") => Ok(Value::string(&s.to_lowercase())),
        (Value::String(s), "trim") => Ok(Value::string(s.trim())),
        (Value::String(s), "reverse") => Ok(Value::string(&s.chars().rev().collect::<String>())),
        (Value::String(s), "find") => {
            let needle = arg(args, 0).to_icinga_string();
            #[expect(
                clippy::cast_precision_loss,
                reason = "string offsets are far below 2^52"
            )]
            Ok(Value::Number(s.find(&needle).map_or(-1.0, |i| i as f64)))
        }
        (Value::String(s), "substr") => {
            let start = to_index(&arg(args, 0))?;
            let tail = s.get(start.min(s.len())..).unwrap_or("");
            let text = match args.get(1) {
                Some(length) => {
                    let length = to_index(length)?;
                    tail.get(..length.min(tail.len())).unwrap_or(tail)
                }
                None => tail,
            };
            Ok(Value::string(text))
        }
        (Value::String(s), "split") => {
            let delims = arg(args, 0).to_icinga_string();
            Ok(Value::array(
                s.split(|c| delims.contains(c)).map(Value::string).collect(),
            ))
        }
        (Value::String(s), "replace") => Ok(Value::string(&s.replace(
            arg(args, 0).to_icinga_string().as_str(),
            arg(args, 1).to_icinga_string().as_str(),
        ))),
        (Value::Array(a), "contains") => Ok(Value::Bool(a.contains(&arg(args, 0)))),
        (Value::Array(a), "join") => {
            let separator = match args.first() {
                Some(sep) => sep.to_icinga_string(),
                None => ",".to_owned(),
            };
            Ok(Value::string(
                &a.items
                    .iter()
                    .map(Value::to_icinga_string)
                    .collect::<Vec<_>>()
                    .join(&separator),
            ))
        }
        (Value::Array(a), "reverse") => Ok(Value::array(a.items.iter().rev().cloned().collect())),
        (Value::Array(a), "shallow_clone") => Ok(Value::array(a.items.clone())),
        (Value::Array(a), "unique") => {
            let mut unique: Vec<Value> = Vec::new();
            for item in &a.items {
                if !unique.iter().any(|seen| icinga_eq(seen, item)) {
                    unique.push(item.clone());
                }
            }
            Ok(Value::array(unique))
        }
        (Value::Dict(d), "contains") => Ok(Value::Bool(
            d.contains_key(&arg(args, 0).to_icinga_string()),
        )),
        (Value::Dict(d), "keys") => Ok(Value::array(d.keys().map(|k| Value::string(k)).collect())),
        (Value::Dict(d), "values") => Ok(Value::array(d.values().cloned().collect())),
        (Value::Dict(d), "shallow_clone") => Ok(Value::Dict(Arc::new((**d).clone()))),
        _ => eval_error(format!(
            "Invalid field access (for value of type '{}'): '{name}'",
            receiver.type_name()
        )),
    }
}

fn to_index(value: &Value) -> Result<usize, EvalError> {
    let number = value.to_number()?;
    if number < 0.0 {
        return eval_error("Index must not be negative.");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked non-negative above; huge values clamp to the string length"
    )]
    Ok(number as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scope with a `host` object and a `service` object backed by maps.
    struct TestScope {
        host: BTreeMap<String, Value>,
        service: Option<BTreeMap<String, Value>>,
    }

    impl Scope for TestScope {
        fn variable(&self, name: &str) -> Option<Value> {
            match name {
                "host" => Some(Value::Object(ObjectRef {
                    type_name: "Host",
                    name: Arc::from("web-01"),
                })),
                "service" => self.service.as_ref().map(|_| {
                    Value::Object(ObjectRef {
                        type_name: "Service",
                        name: Arc::from("web-01!http"),
                    })
                }),
                _ => None,
            }
        }

        fn field(&self, object: &ObjectRef, name: &str) -> Result<Option<Value>, EvalError> {
            let fields = match object.type_name {
                "Host" => Some(&self.host),
                _ => self.service.as_ref(),
            };
            if name == "state_raw" {
                return eval_error("not allowed in sandbox mode");
            }
            Ok(fields.and_then(|f| f.get(name).cloned()))
        }
    }

    fn scope() -> TestScope {
        let vars = serde_json::json!({ "role": "web", "disks": { "/var": { "warn": 80 } }, "tags": ["a", "b"] });
        let mut host = BTreeMap::new();
        host.insert("name".into(), Value::string("web-01"));
        host.insert("state".into(), Value::Number(1.0));
        host.insert("vars".into(), Value::from_json(&vars));
        host.insert(
            "groups".into(),
            Value::array(vec![Value::string("linux-servers"), Value::string("web")]),
        );
        host.insert("address".into(), Value::string(""));
        let mut service = BTreeMap::new();
        service.insert("name".into(), Value::string("http"));
        service.insert("__name".into(), Value::string("web-01!http"));
        service.insert("state".into(), Value::Number(2.0));
        TestScope {
            host,
            service: Some(service),
        }
    }

    fn eval(source: &str) -> Result<bool, EvalError> {
        compile(source, None).unwrap().matches(&scope())
    }

    fn eval_vars(source: &str, vars: &serde_json::Value) -> bool {
        compile(source, vars.as_object())
            .unwrap()
            .matches(&scope())
            .unwrap()
    }

    #[test]
    fn simple_comparisons() {
        assert!(eval(r#"host.name == "web-01""#).unwrap());
        assert!(!eval(r#"host.name == "web-02""#).unwrap());
        assert!(eval(r#"host.name == "web-01" && service.name == "http""#).unwrap());
        assert!(eval("service.state != ServiceOK").unwrap());
        assert!(eval("service.state == 2 && host.state == HostDown").unwrap());
        assert!(eval("service.state >= 2.0 && service.state < 3").unwrap());
    }

    #[test]
    fn filter_vars_and_in() {
        assert!(eval_vars(
            "host.name in names",
            &serde_json::json!({ "names": ["db-01", "web-01"] })
        ));
        let many: Vec<String> = (0..100).map(|i| format!("web-{i:02}")).collect();
        assert!(eval_vars(
            "host.name in names",
            &serde_json::json!({ "names": many })
        ));
        assert!(eval_vars(
            "service.__name in names",
            &serde_json::json!({ "names": ["web-01!http"] })
        ));
        assert!(!eval_vars(
            "service.__name !in names",
            &serde_json::json!({ "names": ["web-01!http"] })
        ));
        assert!(eval(r#""web" in host.groups"#).unwrap());
        assert!(
            !eval(r#""db" in host.vars.missing"#).unwrap(),
            "null rhs is false"
        );
        assert!(eval(r#""db" !in host.vars.missing"#).unwrap());
    }

    #[test]
    fn vars_and_indexers() {
        assert!(eval(r#"host.vars.role == "web""#).unwrap());
        assert!(eval(r#"host.vars.disks["/var"].warn == 80"#).unwrap());
        assert!(eval(r#"host.vars.tags[1] == "b""#).unwrap());
        assert!(!eval(r#"host.vars.nothing == "x""#).unwrap());
        assert!(eval("host.vars.nothing == null").unwrap());
        assert!(
            eval(r"host.address == null").unwrap(),
            "empty string equals null"
        );
        assert!(eval("!host.vars.nothing").unwrap());
    }

    #[test]
    fn functions_and_methods() {
        assert!(eval(r#"match("web*", host.name)"#).unwrap());
        assert!(!eval(r#"match("db*", host.name)"#).unwrap());
        assert!(eval(r#"match("li*", host.groups, MatchAny)"#).unwrap());
        assert!(!eval(r#"match("li*", host.groups)"#).unwrap());
        assert!(eval(r#"host.name.contains("eb")"#).unwrap());
        assert!(eval(r#"host.groups.contains("web")"#).unwrap());
        assert!(eval(r#"host.vars.contains("role")"#).unwrap());
        assert!(eval("len(host.groups) == 2").unwrap());
        assert!(eval(r#"host.name.upper() == "WEB-01""#).unwrap());
        assert!(eval(r#"typeof(host.name) == "String""#).unwrap());
        assert!(matches!(
            eval(r#"host.vars.get("role")"#),
            Err(EvalError(message)) if message.contains("sandbox")
        ));
    }

    #[test]
    fn precedence_and_short_circuit() {
        assert!(eval("1 + 2 * 3 == 7").unwrap());
        assert!(eval("(1 + 2) * 3 == 9").unwrap());
        assert!(
            eval("true || undefined_thing").unwrap(),
            "|| short-circuits"
        );
        assert!(!eval("false && undefined_thing").unwrap());
        assert!(eval("5m == 300 && 1h == 3600 && 500ms == 0.5 && 2d == 172800").unwrap());
        assert!(eval(r#""a" + 1 == "a1""#).unwrap());
        assert!(eval("-1 < 0").unwrap());
        assert!(eval("!false == true").unwrap());
    }

    #[test]
    fn runtime_errors() {
        assert!(
            matches!(eval("nothing == 1"), Err(EvalError(m)) if m.contains("undefined script variable 'nothing'"))
        );
        assert!(
            matches!(eval("host.bogus == 1"), Err(EvalError(m)) if m.contains("Invalid field access (for value of type 'Host'): 'bogus'"))
        );
        assert!(eval(r"host.name < 1").is_err());
        assert!(eval("host.state_raw == 1").is_err());
        assert!(
            matches!(eval(r#""x" in "y""#), Err(EvalError(m)) if m.contains("Invalid right side argument for 'in' operator"))
        );
    }

    #[test]
    fn syntax_and_unsupported() {
        assert!(matches!(
            compile("host.name ==", None),
            Err(FilterError::Syntax(_))
        ));
        assert!(matches!(
            compile(r#"host.name == "x"#, None),
            Err(FilterError::Syntax(_))
        ));
        assert!(matches!(
            compile("host.name $ 1", None),
            Err(FilterError::Syntax(_))
        ));
        assert!(matches!(
            compile("var x = 1", None),
            Err(FilterError::Unsupported(_))
        ));
        assert!(matches!(
            compile("x = 1", None),
            Err(FilterError::Unsupported(_))
        ));
        assert!(matches!(
            compile("{ a = 1 }", None),
            Err(FilterError::Unsupported(_))
        ));
        assert!(matches!(
            compile("{{ true }}", None),
            Err(FilterError::Unsupported(_))
        ));
        assert!(matches!(
            compile("1; 2", None),
            Err(FilterError::Unsupported(_))
        ));
        assert!(matches!(
            compile(r#"regex("^web", host.name)"#, None).unwrap().matches(&scope()),
            Err(EvalError(m)) if m.contains("regex")
        ));
    }

    #[test]
    fn empty_filter_matches_nothing() {
        assert!(!compile("", None).unwrap().matches(&scope()).unwrap());
        assert!(!compile("  \n ", None).unwrap().matches(&scope()).unwrap());
    }

    #[test]
    fn strings_and_comments() {
        assert!(
            compile("host.name == \"web\\x2d01\"", None).is_err(),
            "Icinga has no \\x escapes"
        );
        assert!(eval("host.name == {{{web-01}}}").unwrap());
        assert!(eval("host.name == \"web-01\" // trailing comment").unwrap());
        assert!(eval("/* leading */ host.name == \"web-01\"").unwrap());
        assert!(eval("\"\\101\" == \"A\"").unwrap(), "octal escapes");
    }

    #[test]
    fn globbing() {
        assert!(glob_match("*", ""));
        assert!(glob_match("web-*", "web-01"));
        assert!(glob_match("w?b-0*", "web-01"));
        assert!(!glob_match("web-?", "web-01"));
        assert!(glob_match("*replication*", "postgres-replication"));
        assert!(!glob_match("db*", "web"));
    }
}
