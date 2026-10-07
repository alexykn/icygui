//! Icinga's targeted filters: for `type` `Host` or `Service`,
//! `FilterUtility::GetFilterTargets` doesn't evaluate a filter that only
//! compares names with constants (`ApplyRule::GetTargetHosts`,
//! `GetTargetServices`):
//!
//! ```text
//! host.name == "db-01" || "web-01" == host.name || host.name == name_var
//! host.name == "db-01" && service.name == "load" || service.name == "ping4" && host.name == "web-01"
//! ```
//!
//! It looks the named objects up directly instead, so the results come in
//! the filter's order, a name given twice is there twice, and names
//! without an object are left out. A constant is a string literal or the
//! name of a `filter_vars` entry that holds a string. `||` may join any
//! number of terms; for services each term is a `&&` of exactly the two
//! comparisons, in either order. Parentheses don't matter, and
//! `host["name"]` is the same as `host.name`.
//!
//! Icinga matches its syntax tree. This module reads the source with a
//! small lexer that knows only what such filters are made of (identifiers,
//! `.`, `[ ]`, `( )`, `==`, `&&`, `||`, string literals without escape
//! sequences, spaces and tabs) and leaves every other filter to the
//! evaluator, which finds the same objects in object order. So a targeted
//! filter with a comment, a line break, an escape sequence or an
//! `@`-identifier is evaluated, where Icinga would still look its names up.

use std::collections::BTreeMap;

use ic_filter::Value;

use crate::model::ObjKind;

/// Icinga's keywords: never variables (`config_lexer.ll`).
const KEYWORDS: [&str; 40] = [
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
    "in",
];

/// The full names of the objects a targeted filter names, in its order
/// and with duplicates, whether they exist or not. `None` when the filter
/// isn't one (or `kind` is neither `Host` nor `Service`): it must be
/// evaluated.
pub(super) fn targets(
    source: &str,
    kind: ObjKind,
    vars: &BTreeMap<String, Value>,
) -> Option<Vec<String>> {
    let tree = Parser::new(lex(source)?).filter()?;
    let mut names = Vec::new();
    let found = match kind {
        ObjKind::Host => target_hosts(&tree, vars, &mut names),
        ObjKind::Service => target_services(&tree, vars, &mut names),
        _ => false,
    };
    found.then_some(names)
}

/// `ApplyRule::GetTargetHosts`.
fn target_hosts(node: &Node<'_>, vars: &BTreeMap<String, Value>, names: &mut Vec<String>) -> bool {
    if let Node::Or(left, right) = node {
        return target_hosts(left, vars, names) && target_hosts(right, vars, names);
    }
    match compared_name(node, "host", vars) {
        Some(name) => {
            names.push(name.to_owned());
            true
        }
        None => false,
    }
}

/// `ApplyRule::GetTargetServices`.
fn target_services(
    node: &Node<'_>,
    vars: &BTreeMap<String, Value>,
    names: &mut Vec<String>,
) -> bool {
    if let Node::Or(left, right) = node {
        return target_services(left, vars, names) && target_services(right, vars, names);
    }
    match target_service(node, vars) {
        Some(name) => {
            names.push(name);
            true
        }
        None => false,
    }
}

/// `ApplyRule::GetTargetService`: `host.name == … && service.name == …`,
/// either way round.
fn target_service(node: &Node<'_>, vars: &BTreeMap<String, Value>) -> Option<String> {
    let Node::And(first, second) = node else {
        return None;
    };
    let (host, other) = match compared_name(first, "host", vars) {
        Some(host) => (host, second),
        None => (compared_name(second, "host", vars)?, first),
    };
    let service = compared_name(other, "service", vars)?;
    Some(format!("{host}!{service}"))
}

/// `ApplyRule::GetComparedName`: the constant `<variable>.name` is
/// compared with. Only the first side that is `<variable>.name` counts.
fn compared_name<'a>(
    node: &'a Node<'_>,
    variable: &str,
    vars: &'a BTreeMap<String, Value>,
) -> Option<&'a str> {
    let Node::Equal(left, right) = node else {
        return None;
    };
    if is_name_indexer(left, variable) {
        constant_string(right, vars)
    } else if is_name_indexer(right, variable) {
        constant_string(left, vars)
    } else {
        None
    }
}

/// `ApplyRule::IsNameIndexer`: `<variable>.name` or `<variable>["name"]`.
fn is_name_indexer(node: &Node<'_>, variable: &str) -> bool {
    matches!(node, Node::Index(target, index)
        if matches!(**target, Node::Variable(name) if name == variable)
            && matches!(**index, Node::String("name")))
}

/// `ApplyRule::GetConstString`: a string literal, or a `filter_vars` entry
/// that holds a string.
fn constant_string<'a>(node: &'a Node<'_>, vars: &'a BTreeMap<String, Value>) -> Option<&'a str> {
    match node {
        Node::String(text) => Some(*text),
        Node::Variable(name) => match vars.get(*name) {
            Some(Value::String(text)) => Some(&**text),
            _ => None,
        },
        _ => None,
    }
}

/// The tokens of a targeted filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token<'s> {
    Identifier(&'s str),
    String(&'s str),
    Dot,
    LeftBracket,
    RightBracket,
    LeftParen,
    RightParen,
    Equal,
    And,
    Or,
}

/// Splits `source` into tokens; `None` for anything a targeted filter
/// can't contain (see the module docs).
fn lex(source: &str) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut rest = source;
    while let Some(first) = rest.chars().next() {
        let (token, length) = match first {
            ' ' | '\t' => {
                rest = &rest[1..];
                continue;
            }
            '.' => (Token::Dot, 1),
            '[' => (Token::LeftBracket, 1),
            ']' => (Token::RightBracket, 1),
            '(' => (Token::LeftParen, 1),
            ')' => (Token::RightParen, 1),
            '=' if rest.starts_with("==") => (Token::Equal, 2),
            '&' if rest.starts_with("&&") => (Token::And, 2),
            '|' if rest.starts_with("||") => (Token::Or, 2),
            '"' => {
                let end = rest[1..].find(['"', '\\', '\n', '\r'])? + 1;
                if !rest[end..].starts_with('"') {
                    return None;
                }
                (Token::String(&rest[1..end]), end + 1)
            }
            '{' if rest.starts_with("{{{") => {
                let end = rest[3..].find("}}}")? + 3;
                (Token::String(&rest[3..end]), end + 3)
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let length = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                let word = &rest[..length];
                if KEYWORDS.contains(&word) {
                    return None;
                }
                (Token::Identifier(word), length)
            }
            _ => return None,
        };
        tokens.push(token);
        rest = &rest[length..];
    }
    Some(tokens)
}

/// The syntax tree of a targeted filter, as Icinga builds it (parentheses
/// leave no node).
#[derive(Debug, PartialEq, Eq)]
enum Node<'s> {
    Or(Box<Node<'s>>, Box<Node<'s>>),
    And(Box<Node<'s>>, Box<Node<'s>>),
    Equal(Box<Node<'s>>, Box<Node<'s>>),
    /// `target.name` or `target[index]`.
    Index(Box<Node<'s>>, Box<Node<'s>>),
    Variable(&'s str),
    String(&'s str),
}

/// A recursive-descent parser with Icinga's precedence: `||` below `&&`
/// below `==` (which doesn't chain) below `.` and `[ ]`.
struct Parser<'s> {
    tokens: Vec<Token<'s>>,
    position: usize,
}

impl<'s> Parser<'s> {
    fn new(tokens: Vec<Token<'s>>) -> Self {
        Self {
            tokens,
            position: 0,
        }
    }

    /// The whole filter: one expression and nothing after it.
    fn filter(mut self) -> Option<Node<'s>> {
        let node = self.or()?;
        (self.position == self.tokens.len()).then_some(node)
    }

    fn peek(&self) -> Option<Token<'s>> {
        self.tokens.get(self.position).copied()
    }

    fn next(&mut self) -> Option<Token<'s>> {
        let token = self.peek()?;
        self.position += 1;
        Some(token)
    }

    fn eat(&mut self, token: Token<'s>) -> bool {
        let found = self.peek() == Some(token);
        if found {
            self.position += 1;
        }
        found
    }

    fn or(&mut self) -> Option<Node<'s>> {
        let mut node = self.and()?;
        while self.eat(Token::Or) {
            node = Node::Or(Box::new(node), Box::new(self.and()?));
        }
        Some(node)
    }

    fn and(&mut self) -> Option<Node<'s>> {
        let mut node = self.equal()?;
        while self.eat(Token::And) {
            node = Node::And(Box::new(node), Box::new(self.equal()?));
        }
        Some(node)
    }

    fn equal(&mut self) -> Option<Node<'s>> {
        let left = self.postfix()?;
        if !self.eat(Token::Equal) {
            return Some(left);
        }
        let right = self.postfix()?;
        // `==` is non-associative in Icinga: `a == b == c` doesn't parse.
        (self.peek() != Some(Token::Equal)).then(|| Node::Equal(Box::new(left), Box::new(right)))
    }

    fn postfix(&mut self) -> Option<Node<'s>> {
        let mut node = self.primary()?;
        loop {
            if self.eat(Token::Dot) {
                let Some(Token::Identifier(name)) = self.next() else {
                    return None;
                };
                node = Node::Index(Box::new(node), Box::new(Node::String(name)));
            } else if self.eat(Token::LeftBracket) {
                let index = self.or()?;
                if !self.eat(Token::RightBracket) {
                    return None;
                }
                node = Node::Index(Box::new(node), Box::new(index));
            } else {
                return Some(node);
            }
        }
    }

    fn primary(&mut self) -> Option<Node<'s>> {
        match self.next()? {
            Token::Identifier(name) => Some(Node::Variable(name)),
            Token::String(text) => Some(Node::String(text)),
            Token::LeftParen => {
                let node = self.or()?;
                self.eat(Token::RightParen).then_some(node)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("h".to_owned(), Value::from("db-01")),
            ("s".to_owned(), Value::from("load")),
            ("n".to_owned(), Value::Number(1.0)),
        ])
    }

    fn hosts(source: &str) -> Option<Vec<String>> {
        targets(source, ObjKind::Host, &vars())
    }

    fn services(source: &str) -> Option<Vec<String>> {
        targets(source, ObjKind::Service, &vars())
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn hosts_in_filter_order_with_duplicates() {
        assert_eq!(hosts(r#"host.name == "a""#), Some(names(&["a"])));
        assert_eq!(
            hosts(r#"host.name == "b" || "a" == host.name || host.name == h || host.name == "b""#),
            Some(names(&["b", "a", "db-01", "b"]))
        );
        assert_eq!(
            hosts(r#"(host.name == "b" || (host["name"] == {{{a}}}))"#),
            Some(names(&["b", "a"])),
            "parentheses leave no node; indexers count"
        );
        assert_eq!(
            hosts("\thost . name==\"x y\"  "),
            Some(names(&["x y"])),
            "spaces and tabs"
        );
        assert_eq!(
            hosts(r#"host.name == "{{{" || host.name == {{{"x"}}}"#),
            Some(names(&["{{{", "\"x\""]))
        );
    }

    #[test]
    fn services_by_host_and_service_name() {
        assert_eq!(
            services(
                r#"host.name == "a" && service.name == "ping4" || service.name == "load" && host.name == "b" || host.name == h && s == service.name"#
            ),
            Some(names(&["a!ping4", "b!load", "db-01!load"]))
        );
        assert_eq!(services(r#"host.name == "a""#), None, "a host alone");
        assert_eq!(
            services(r#"host.name == "a" && service.name == "x" && service.name == "y""#),
            None,
            "three comparisons"
        );
        assert_eq!(
            services(r#"service.name == "x" && service.name == "y""#),
            None
        );
        assert_eq!(hosts(r#"host.name == "a" && service.name == "x""#), None);
    }

    #[test]
    fn other_filters_are_evaluated() {
        for source in [
            "",
            "   ",
            "host.name",
            r#"host.name != "a""#,
            r#"host.name == "a" || host.state == "1""#,
            r#"host.name == "a" || host.name == "b" && host.vars"#,
            r#"host.name == "a" && true"#,
            r#"host.display_name == "a""#,
            r#"obj.name == "a""#,
            r#"host.name.x == "a""#,
            r#"service.name == "a""#,
            r"host.name == host.name",
            r"host.name == 1",
            "host.name == n",
            "host.name == undefined_var",
            r#"host.name == "a" == "b""#,
            r#"host.name == "a\x""#,
            r#"host.name == "a\"b""#,
            "host.name == \"a\"\n|| host.name == \"b\"",
            r#"host.name == "a" // comment"#,
            r#"host.name == "a" # comment"#,
            r#"host.name == "a";"#,
            r#"host.name == "a" ||"#,
            r#"host.name == "a")"#,
            r#"(host.name == "a""#,
            r#"host.name == "a" ||| host.name == "b""#,
            r#"host.name === "a""#,
            r#"host.name == "a" & host.name == "b""#,
            r#"host.name == ["a"]"#,
            r#"host.name == f("a")"#,
            r#"@host.name == "a""#,
            r#"this.name == "a""#,
            r"host.name == null",
            r#"host.name == "unterminated"#,
            r"host.name == {{{unterminated",
        ] {
            assert_eq!(hosts(source), None, "{source:?}");
        }
        assert_eq!(
            targets(r#"host.name == "a""#, ObjKind::Comment, &vars()),
            None,
            "only hosts and services"
        );
    }

    #[test]
    fn the_first_name_indexer_decides() {
        // `GetComparedName` takes the right side as the constant once the
        // left side is `host.name`, and doesn't try the other way round.
        assert_eq!(hosts("host.name == host.name"), None);
        assert_eq!(hosts(r#"host["name"] == h"#), Some(names(&["db-01"])));
        assert_eq!(hosts(r#"h == host[("name")]"#), Some(names(&["db-01"])));
    }
}
