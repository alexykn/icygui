//! Pratt parser for filter expressions.
//!
//! Precedence and associativity follow the declarations in Icinga's
//! `config_parser.yy`, loosest first:
//!
//! | operators                | associativity |
//! |--------------------------|---------------|
//! | `? :`                    | right         |
//! | `\|\|`                   | left          |
//! | `&&`                     | left          |
//! | `\|`                     | left          |
//! | `^`                      | left          |
//! | `&`                      | left          |
//! | `==` `!=`                | none          |
//! | `in` `!in`               | left          |
//! | `<` `<=` `>` `>=`        | none          |
//! | `<<` `>>`                | left          |
//! | `+` `-`                  | left          |
//! | `*` `/` `%`              | left          |
//! | prefix `-` `+` `!` `~`   |               |
//! | `.` `[…]` `(…)`          |               |
//!
//! "None" means chaining is a syntax error (`a == b == c`). Line breaks end
//! the expression except inside parentheses, as in Icinga, and a filter is
//! one expression with an optional trailing `,` or `;`.

use std::sync::Arc;

use crate::ParseError;
use crate::ast::{BinaryOp, Expr, ExprKind, Function, PatternArg, Span, UnaryOp};
use crate::eval::Evaluator;
use crate::lexer::{Keyword, Tok, Token, tokenize};
use crate::pattern::{Cidr, Glob, IcingaRegex};
use crate::scope::Chain;
use crate::value::{Value, format_number};

/// Maximum height of the syntax tree and nesting of the parser's recursion.
/// Real filters stay far below; the bound keeps hostile input from
/// overflowing the stack.
pub(crate) const MAX_DEPTH: usize = 64;

/// Binding power of `? :`.
const TERNARY: u8 = 2;

/// Parses a filter. `None` means there is no expression (empty source).
pub(crate) fn parse(source: &str) -> Result<Option<Expr>, ParseError> {
    let (tokens, lex_error) = tokenize(source);
    let eof = Token {
        tok: Tok::Eof,
        start: source.len(),
        end: source.len(),
    };
    let result = Parser {
        tokens,
        eof,
        pos: 0,
        depth: 0,
    }
    .filter();
    match lex_error {
        // A syntax error before the lexer's error is reported first.
        Some(lex_error) => match result {
            Err(error) if error.offset < lex_error.offset => Err(error),
            _ => Err(lex_error),
        },
        None => result,
    }
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Infix {
    Or,
    And,
    BitOr,
    BitXor,
    BitAnd,
    Equal,
    NotEqual,
    In,
    NotIn,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    ShiftLeft,
    ShiftRight,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
}

/// Operators that can't be chained without parentheses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NonAssociative {
    Equality,
    Relational,
}

impl Infix {
    fn from_tok(tok: &Tok) -> Option<Infix> {
        Some(match tok {
            Tok::OrOr => Infix::Or,
            Tok::AndAnd => Infix::And,
            Tok::Pipe => Infix::BitOr,
            Tok::Caret => Infix::BitXor,
            Tok::Amp => Infix::BitAnd,
            Tok::EqEq => Infix::Equal,
            Tok::NotEq => Infix::NotEqual,
            Tok::In => Infix::In,
            Tok::NotIn => Infix::NotIn,
            Tok::Lt => Infix::Less,
            Tok::Le => Infix::LessEqual,
            Tok::Gt => Infix::Greater,
            Tok::Ge => Infix::GreaterEqual,
            Tok::Shl => Infix::ShiftLeft,
            Tok::Shr => Infix::ShiftRight,
            Tok::Plus => Infix::Add,
            Tok::Minus => Infix::Subtract,
            Tok::Star => Infix::Multiply,
            Tok::Slash => Infix::Divide,
            Tok::Percent => Infix::Modulo,
            _ => return None,
        })
    }

    /// Left and right binding power. Left-associative operators bind their
    /// right operand one level tighter.
    fn binding_power(self) -> (u8, u8) {
        let left = match self {
            Infix::Or => 4,
            Infix::And => 6,
            Infix::BitOr => 8,
            Infix::BitXor => 10,
            Infix::BitAnd => 12,
            Infix::Equal | Infix::NotEqual => 14,
            Infix::In | Infix::NotIn => 16,
            Infix::Less | Infix::LessEqual | Infix::Greater | Infix::GreaterEqual => 18,
            Infix::ShiftLeft | Infix::ShiftRight => 20,
            Infix::Add | Infix::Subtract => 22,
            Infix::Multiply | Infix::Divide | Infix::Modulo => 24,
        };
        (left, left + 1)
    }

    fn non_associative(self) -> Option<NonAssociative> {
        match self {
            Infix::Equal | Infix::NotEqual => Some(NonAssociative::Equality),
            Infix::Less | Infix::LessEqual | Infix::Greater | Infix::GreaterEqual => {
                Some(NonAssociative::Relational)
            }
            _ => None,
        }
    }

    fn binary_op(self) -> Option<BinaryOp> {
        Some(match self {
            Infix::BitOr => BinaryOp::BitOr,
            Infix::BitXor => BinaryOp::BitXor,
            Infix::BitAnd => BinaryOp::BitAnd,
            Infix::Equal => BinaryOp::Equal,
            Infix::NotEqual => BinaryOp::NotEqual,
            Infix::Less => BinaryOp::Less,
            Infix::LessEqual => BinaryOp::LessEqual,
            Infix::Greater => BinaryOp::Greater,
            Infix::GreaterEqual => BinaryOp::GreaterEqual,
            Infix::ShiftLeft => BinaryOp::ShiftLeft,
            Infix::ShiftRight => BinaryOp::ShiftRight,
            Infix::Add => BinaryOp::Add,
            Infix::Subtract => BinaryOp::Subtract,
            Infix::Multiply => BinaryOp::Multiply,
            Infix::Divide => BinaryOp::Divide,
            Infix::Modulo => BinaryOp::Modulo,
            Infix::Or | Infix::And | Infix::In | Infix::NotIn => return None,
        })
    }
}

/// Whether a token can start an operand.
fn starts_expression(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::Number(_)
            | Tok::Str(_)
            | Tok::Ident(_)
            | Tok::True
            | Tok::False
            | Tok::Null
            | Tok::LParen
            | Tok::LBracket
            | Tok::LBrace
            | Tok::Bang
            | Tok::Tilde
            | Tok::Minus
            | Tok::Plus
    )
}

fn error(message: impl Into<String>, offset: usize) -> ParseError {
    ParseError {
        message: message.into(),
        offset,
    }
}

const MULTI_LINE_HINT: &str =
    "a line break ends the filter expression here; wrap a multi-line filter in parentheses";

struct Parser {
    tokens: Vec<Token>,
    eof: Token,
    pos: usize,
    depth: usize,
}

/// Parsing functions on the recursive path (`binary`, `unary`,
/// `postfix_expression`, `primary` and the bracketed forms) are kept small:
/// tokens are moved rather than cloned, and building nodes and error
/// messages happens in separate functions. Unoptimised builds give every
/// local its own stack slot, so this keeps the deepest accepted filter well
/// within a thread's stack even in debug builds.
impl Parser {
    fn peek(&self) -> &Token {
        self.tokens.get(self.pos).unwrap_or(&self.eof)
    }

    fn at(&self, tok: &Tok) -> bool {
        self.peek().tok == *tok
    }

    /// Consumes the current token and returns its span.
    fn bump(&mut self) -> Span {
        let token = self.peek();
        let span = Span::new(token.start, token.end);
        if self.pos < self.tokens.len() {
            self.pos += 1;
        }
        span
    }

    /// Consumes the current token and returns it (tokens are never looked
    /// at again once consumed, so it is moved out).
    fn take(&mut self) -> Token {
        let Some(token) = self.tokens.get_mut(self.pos) else {
            return self.eof.clone();
        };
        self.pos += 1;
        let placeholder = Token {
            tok: Tok::Eof,
            start: token.start,
            end: token.end,
        };
        std::mem::replace(token, placeholder)
    }

    fn skip_newlines(&mut self) -> bool {
        let mut skipped = false;
        while self.at(&Tok::Newline) {
            self.bump();
            skipped = true;
        }
        skipped
    }

    /// Consumes `tok` or fails with "expected …".
    fn expect(&mut self, tok: &Tok, expected: &'static str) -> Result<Span, ParseError> {
        if self.at(tok) {
            Ok(self.bump())
        } else {
            Err(unexpected(self.peek(), expected))
        }
    }

    /// Enters one level of nesting.
    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(too_deep(self.peek().start));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// The whole filter: one expression, optionally followed by `,` or `;`.
    fn filter(mut self) -> Result<Option<Expr>, ParseError> {
        self.skip_newlines();
        if self.at(&Tok::Eof) {
            return Ok(None);
        }
        let expr = self.expression()?;
        let separated = matches!(self.peek().tok, Tok::Comma | Tok::Semicolon);
        if separated {
            self.bump();
        }
        let line_break = self.skip_newlines();
        if self.at(&Tok::Eof) {
            return Ok(Some(expr));
        }
        Err(trailing_error(self.peek(), separated, line_break))
    }

    fn expression(&mut self) -> Result<Expr, ParseError> {
        self.binary(0)
    }

    /// Operators binding at least as tightly as `min_power`.
    fn binary(&mut self, min_power: u8) -> Result<Expr, ParseError> {
        self.enter()?;
        let mut lhs = self.unary()?;
        loop {
            let tok = &self.peek().tok;
            if *tok == Tok::Question {
                if TERNARY < min_power {
                    break;
                }
                lhs = self.conditional(lhs)?;
                continue;
            }
            let Some(op) = Infix::from_tok(tok) else {
                break;
            };
            let (left_power, right_power) = op.binding_power();
            if left_power < min_power {
                break;
            }
            self.bump();
            let rhs = self.binary(right_power)?;
            lhs = infix(op, lhs, rhs)?;
            self.check_not_chained(op)?;
        }
        self.leave();
        Ok(lhs)
    }

    /// `condition ? then : otherwise`, after the condition.
    fn conditional(&mut self, condition: Expr) -> Result<Expr, ParseError> {
        self.bump();
        let then = self.binary(0)?;
        self.expect(&Tok::Colon, "':' of the conditional (?:) expression")?;
        let otherwise = self.binary(TERNARY)?;
        conditional_node(condition, then, otherwise)
    }

    /// `a == b == c` and `a < b < c` are syntax errors in Icinga.
    fn check_not_chained(&self, op: Infix) -> Result<(), ParseError> {
        let next = self.peek();
        match op.non_associative() {
            Some(class)
                if Infix::from_tok(&next.tok).and_then(Infix::non_associative) == Some(class) =>
            {
                Err(chained_error(class, next.start))
            }
            _ => Ok(()),
        }
    }

    /// Prefix operators, then an operand with its postfix operators.
    fn unary(&mut self) -> Result<Expr, ParseError> {
        let op = match self.peek().tok {
            Tok::Bang => Some(UnaryOp::Not),
            Tok::Tilde => Some(UnaryOp::BitNot),
            Tok::Minus => Some(UnaryOp::Negate),
            // Unary plus returns its operand unchanged, as in Icinga.
            Tok::Plus => None,
            Tok::Star | Tok::Amp => return Err(reference_error(self.peek())),
            _ => return self.postfix_expression(),
        };
        let start = self.bump().start;
        self.enter()?;
        let operand = self.unary()?;
        self.leave();
        match op {
            Some(op) => unary_node(op, operand, start),
            None => Ok(operand),
        }
    }

    /// An operand followed by member accesses, indexers and calls.
    fn postfix_expression(&mut self) -> Result<Expr, ParseError> {
        let mut expr = self.primary()?;
        loop {
            expr = match self.peek().tok {
                Tok::Dot => self.dot(expr)?,
                Tok::LBracket => self.bracket(expr)?,
                Tok::LParen => self.call(&expr)?,
                _ => return Ok(expr),
            };
        }
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        match self.peek().tok {
            Tok::LParen => self.parenthesized(),
            Tok::LBracket => self.array(),
            Tok::LBrace => self.dict(),
            _ => self.atom(),
        }
    }

    /// A literal or a variable name.
    fn atom(&mut self) -> Result<Expr, ParseError> {
        let token = self.take();
        let span = Span::new(token.start, token.end);
        let value = match token.tok {
            Tok::Number(number) => Value::Number(number),
            Tok::Str(text) => Value::from(text),
            Tok::True => Value::Bool(true),
            Tok::False => Value::Bool(false),
            Tok::Null => Value::Null,
            Tok::Ident(name) => {
                if self.at(&Tok::Arrow) {
                    return Err(lambda_error(self.peek().start));
                }
                return Ok(Expr {
                    kind: ExprKind::Path(vec![name.into()]),
                    span,
                    depth: 1,
                });
            }
            tok => return Err(unexpected(&Token { tok, ..token }, "an expression")),
        };
        Ok(Expr::literal(value, span))
    }

    fn parenthesized(&mut self) -> Result<Expr, ParseError> {
        let open = self.bump().start;
        if self.at(&Tok::RParen) {
            return Err(error("empty parentheses: expected an expression", open));
        }
        let inner = self.expression()?;
        if !self.at(&Tok::RParen) {
            return Err(unclosed(self.peek(), "')'", '(', open));
        }
        self.bump();
        if self.at(&Tok::Arrow) {
            return Err(lambda_error(self.peek().start));
        }
        Ok(inner)
    }

    fn array(&mut self) -> Result<Expr, ParseError> {
        let open = self.bump().start;
        self.skip_newlines();
        let mut items = Vec::new();
        let end = loop {
            if self.at(&Tok::RBracket) {
                break self.bump().end;
            }
            items.push(self.expression()?);
            match self.peek().tok {
                Tok::Comma => {
                    self.bump();
                    self.skip_newlines();
                }
                Tok::Newline => {
                    self.skip_newlines();
                    break self
                        .expect(&Tok::RBracket, "']' after a line break in an array")?
                        .end;
                }
                Tok::RBracket => break self.bump().end,
                _ => return Err(unexpected(self.peek(), "',' or ']' in the array")),
            }
        };
        node(ExprKind::Array(items), Span::new(open, end))
    }

    fn dict(&mut self) -> Result<Expr, ParseError> {
        let open = self.bump().start;
        self.skip_newlines();
        let mut entries = Vec::new();
        let end = loop {
            if self.at(&Tok::RBrace) {
                break self.bump().end;
            }
            let key = self.dict_key()?;
            entries.push((key, self.expression()?));
            match self.peek().tok {
                Tok::Comma | Tok::Semicolon => {
                    self.bump();
                    self.skip_newlines();
                }
                Tok::Newline => {
                    self.skip_newlines();
                }
                Tok::RBrace => break self.bump().end,
                _ => return Err(unexpected(self.peek(), "',' or '}' in the dictionary")),
            }
        };
        node(ExprKind::Dict(entries), Span::new(open, end))
    }

    /// `key =` in a dictionary literal.
    fn dict_key(&mut self) -> Result<Box<str>, ParseError> {
        let token = self.take();
        let key = match token.tok {
            Tok::Ident(name) | Tok::Str(name) => name,
            Tok::Keyword(_) | Tok::True | Tok::False | Tok::Null | Tok::In => {
                return Err(reserved_key_error(&token));
            }
            tok => {
                return Err(unexpected(
                    &Token { tok, ..token },
                    "a key (a name or a string) in the dictionary",
                ));
            }
        };
        let assign = self.peek();
        match assign.tok {
            Tok::Assign => {
                self.bump();
                Ok(key.into())
            }
            _ => Err(dict_assign_error(assign)),
        }
    }

    /// `.name` or `.name(args…)`.
    fn dot(&mut self, expr: Expr) -> Result<Expr, ParseError> {
        self.bump();
        let token = self.take();
        let name = match token.tok {
            Tok::Ident(name) => name,
            Tok::Keyword(_) | Tok::True | Tok::False | Tok::Null | Tok::In => {
                return Err(reserved_member_error(&token));
            }
            tok => return Err(unexpected(&Token { tok, ..token }, "a name after '.'")),
        };
        let span = expr.span.to(Span::new(token.start, token.end));
        if self.at(&Tok::LParen) {
            self.method_call(expr, name.into(), span)
        } else {
            member(expr, name.into(), span)
        }
    }

    /// `[index]`, or `["name"](args…)` (a method call).
    fn bracket(&mut self, expr: Expr) -> Result<Expr, ParseError> {
        self.bump();
        let index = self.expression()?;
        let close = self.expect(&Tok::RBracket, "']' to close the index")?;
        let span = expr.span.to(close);
        if let (Some(Value::String(name)), true) = (index.as_literal(), self.at(&Tok::LParen)) {
            let name = name.as_ref().into();
            return self.method_call(expr, name, span);
        }
        index_node(expr, index, span)
    }

    /// `name(args…)`.
    fn call(&mut self, callee: &Expr) -> Result<Expr, ParseError> {
        let function = match &callee.kind {
            ExprKind::Path(segments) if segments.len() == 1 => {
                Function::from_name(segments.first().map_or("", |segment| &**segment))
            }
            _ => return Err(not_callable(self.peek().start)),
        };
        let (args, end) = self.arguments()?;
        call_node(function, args, Span::new(callee.span.start, end))
    }

    fn method_call(
        &mut self,
        receiver: Expr,
        name: Box<str>,
        span: Span,
    ) -> Result<Expr, ParseError> {
        let (args, end) = self.arguments()?;
        node(
            ExprKind::Method {
                receiver: Box::new(receiver),
                name,
                args,
            },
            span.to(Span::new(end, end)),
        )
    }

    /// `(args…)`. Returns the arguments and the end of the closing `)`.
    fn arguments(&mut self) -> Result<(Vec<Expr>, usize), ParseError> {
        let open = self.expect(&Tok::LParen, "'('")?.start;
        let mut args = Vec::new();
        loop {
            if self.at(&Tok::RParen) {
                return Ok((args, self.bump().end));
            }
            args.push(self.expression()?);
            match self.peek().tok {
                Tok::Comma => {
                    self.bump();
                }
                Tok::RParen => return Ok((args, self.bump().end)),
                _ => return Err(unclosed(self.peek(), "',' or ')'", '(', open)),
            }
        }
    }
}

fn infix(op: Infix, lhs: Expr, rhs: Expr) -> Result<Expr, ParseError> {
    let span = lhs.span.to(rhs.span);
    let kind = match op {
        Infix::And | Infix::Or => return logical(op, lhs, rhs, span),
        Infix::In | Infix::NotIn => ExprKind::In {
            item: Box::new(lhs),
            collection: Box::new(rhs),
            negated: op == Infix::NotIn,
        },
        _ => match op.binary_op() {
            Some(op) => ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            None => return Err(error("internal error: unknown operator", span.start)),
        },
    };
    node(kind, span)
}

/// `&&` and `||` chains become one flat node, so long chains (generated
/// filters with hundreds of `||`) neither nest deeply nor take quadratic
/// time to build.
fn logical(op: Infix, lhs: Expr, rhs: Expr, span: Span) -> Result<Expr, ParseError> {
    let is_chain = |expr: &Expr| {
        matches!(
            (op, &expr.kind),
            (Infix::And, ExprKind::And(_)) | (Infix::Or, ExprKind::Or(_))
        )
    };
    let wrap = |items: Vec<Expr>| {
        if op == Infix::And {
            ExprKind::And(items)
        } else {
            ExprKind::Or(items)
        }
    };
    let (lhs_chain, rhs_chain) = (is_chain(&lhs), is_chain(&rhs));
    if !lhs_chain && !rhs_chain {
        return node(wrap(vec![lhs, rhs]), span);
    }
    // A chain node always holds a non-literal (two literals are folded),
    // so the result can't be folded: extend it in place.
    let depth = (lhs.depth + usize::from(!lhs_chain)).max(rhs.depth + usize::from(!rhs_chain));
    if depth > MAX_DEPTH {
        return Err(too_deep(span.start));
    }
    let mut items = match lhs.kind {
        ExprKind::And(items) | ExprKind::Or(items) if lhs_chain => items,
        kind => vec![Expr { kind, ..lhs }],
    };
    match rhs.kind {
        ExprKind::And(more) | ExprKind::Or(more) if rhs_chain => items.extend(more),
        kind => items.push(Expr { kind, ..rhs }),
    }
    Ok(Expr {
        kind: wrap(items),
        span,
        depth,
    })
}

fn unary_node(op: UnaryOp, operand: Expr, start: usize) -> Result<Expr, ParseError> {
    let span = Span::new(start, operand.span.end);
    node(
        ExprKind::Unary {
            op,
            operand: Box::new(operand),
        },
        span,
    )
}

fn conditional_node(condition: Expr, then: Expr, otherwise: Expr) -> Result<Expr, ParseError> {
    let span = condition.span.to(otherwise.span);
    node(
        ExprKind::Conditional {
            condition: Box::new(condition),
            then: Box::new(then),
            otherwise: Box::new(otherwise),
        },
        span,
    )
}

fn call_node(function: Function, args: Vec<Expr>, span: Span) -> Result<Expr, ParseError> {
    let function = precompile(function, &args)?;
    node(ExprKind::Call { function, args }, span)
}

/// `expr.name`: extends a path, or accesses a member of a computed value.
fn member(expr: Expr, name: Box<str>, span: Span) -> Result<Expr, ParseError> {
    match expr.kind {
        ExprKind::Path(mut segments) => {
            segments.push(name);
            Ok(Expr {
                kind: ExprKind::Path(segments),
                span,
                depth: 1,
            })
        }
        kind => node(
            ExprKind::Member {
                target: Box::new(Expr { kind, ..expr }),
                name,
            },
            span,
        ),
    }
}

/// `expr[index]`: a literal string or number extends a path (Icinga turns
/// the index into a string either way).
fn index_node(expr: Expr, index: Expr, span: Span) -> Result<Expr, ParseError> {
    let constant = match index.as_literal() {
        Some(Value::String(text)) => Some(text.to_string()),
        Some(Value::Number(number)) => Some(format_number(*number)),
        _ => None,
    };
    match (expr.kind, constant) {
        (ExprKind::Path(mut segments), Some(name)) => {
            segments.push(name.into());
            Ok(Expr {
                kind: ExprKind::Path(segments),
                span,
                depth: 1,
            })
        }
        (kind, _) => node(
            ExprKind::Index {
                target: Box::new(Expr { kind, ..expr }),
                index: Box::new(index),
            },
            span,
        ),
    }
}

/// Builds a node: checks the tree height and folds constants.
fn node(kind: ExprKind, span: Span) -> Result<Expr, ParseError> {
    let depth = 1 + children_depth(&kind);
    if depth > MAX_DEPTH {
        return Err(too_deep(span.start));
    }
    Ok(fold(Expr { kind, span, depth }))
}

#[cold]
fn too_deep(offset: usize) -> ParseError {
    error(
        format!("the filter is nested too deeply (more than {MAX_DEPTH} levels)"),
        offset,
    )
}

/// What follows a complete expression that isn't the end of the filter.
#[cold]
fn trailing_error(token: &Token, separated: bool, line_break: bool) -> ParseError {
    let follows_operand = Infix::from_tok(&token.tok).is_some()
        || matches!(
            token.tok,
            Tok::Question | Tok::Colon | Tok::Dot | Tok::LBracket | Tok::LParen
        );
    if line_break && !separated && follows_operand {
        return error(MULTI_LINE_HINT, token.start);
    }
    if (separated || line_break) && starts_expression(&token.tok) {
        let mut message =
            "only a single expression is supported in filters, not a list of statements".to_owned();
        if line_break && !separated {
            message.push_str("; wrap a multi-line filter in parentheses");
        }
        return error(message, token.start);
    }
    unexpected(token, "an operator or the end of the filter")
}

#[cold]
fn chained_error(class: NonAssociative, offset: usize) -> ParseError {
    let operators = match class {
        NonAssociative::Equality => "'==' and '!='",
        NonAssociative::Relational => "'<', '<=', '>' and '>='",
    };
    error(
        format!("{operators} can't be chained; use parentheses or combine the comparisons with &&"),
        offset,
    )
}

#[cold]
fn reference_error(token: &Token) -> ParseError {
    let symbol = if token.tok == Tok::Star { "*" } else { "&" };
    error(
        format!("references (prefix '{symbol}') are not supported in filters"),
        token.start,
    )
}

#[cold]
fn lambda_error(offset: usize) -> ParseError {
    error("functions (lambdas) are not supported in filters", offset)
}

#[cold]
fn not_callable(offset: usize) -> ParseError {
    error("only functions and methods can be called", offset)
}

/// A missing closing bracket.
#[cold]
fn unclosed(token: &Token, expected: &str, open: char, open_offset: usize) -> ParseError {
    unexpected(
        token,
        &format!("{expected} to close the '{open}' at byte {open_offset}"),
    )
}

/// The word of a keyword-like token (`default`, `true`, `in`).
fn reserved_word(token: &Token) -> String {
    match &token.tok {
        Tok::Keyword(keyword) => keyword.as_str().to_owned(),
        other => other.describe().trim_matches('\'').to_owned(),
    }
}

#[cold]
fn reserved_key_error(token: &Token) -> ParseError {
    let word = reserved_word(token);
    error(
        format!("'{word}' is a reserved keyword; write @{word} or \"{word}\" to use it as a key"),
        token.start,
    )
}

#[cold]
fn reserved_member_error(token: &Token) -> ParseError {
    let word = reserved_word(token);
    error(
        format!(
            "'{word}' is a reserved keyword; write .@{word} or [\"{word}\"] to access a key named '{word}'"
        ),
        token.start,
    )
}

/// Anything but `=` after a dictionary key.
#[cold]
fn dict_assign_error(token: &Token) -> ParseError {
    match &token.tok {
        Tok::CompoundAssign(op) => error(
            format!("'{op}' is not supported in dictionary literals; use '='"),
            token.start,
        ),
        Tok::Dot | Tok::LBracket => error(
            "nested keys (a.b = …) are not supported in dictionary literals; use a nested dictionary",
            token.start,
        ),
        _ => unexpected(token, "'=' after the dictionary key"),
    }
}

/// An error for an unexpected token, with advice for constructs from Icinga's
/// configuration language that filters don't support.
#[cold]
fn unexpected(token: &Token, expected: &str) -> ParseError {
    let message = match &token.tok {
        Tok::Assign => "assignments are not supported in filters; use == to compare".to_owned(),
        Tok::CompoundAssign(op) => format!("assignments ('{op}') are not supported in filters"),
        Tok::Arrow => "functions (lambdas) are not supported in filters".to_owned(),
        Tok::LambdaOpen => "'{{' starts a function, which filters don't support; \
                            write '{ {' for nested dictionaries"
            .to_owned(),
        Tok::LambdaClose => "'}}' ends a function in Icinga's syntax; \
                             write '} }' to close two dictionaries"
            .to_owned(),
        Tok::AngleString => "'<' followed by '>' without a space in between is an include \
                             path in Icinga's syntax; put spaces around '<' and '>'"
            .to_owned(),
        Tok::Keyword(keyword) => keyword_message(*keyword),
        Tok::NotIn if expected == "an expression" => {
            "'!in' is the 'not in' operator; to negate a name that starts with 'in', \
             write a space after '!'"
                .to_owned()
        }
        Tok::Newline => format!(
            "unexpected line break, expected {expected}; wrap a multi-line filter in parentheses"
        ),
        other => format!("unexpected {}, expected {expected}", other.describe()),
    };
    error(message, token.start)
}

fn keyword_message(keyword: Keyword) -> String {
    let word = keyword.as_str();
    match keyword {
        Keyword::Function => "functions are not supported in filters".to_owned(),
        Keyword::This
        | Keyword::Globals
        | Keyword::Locals
        | Keyword::CurrentFilename
        | Keyword::CurrentLine => format!("'{word}' is not supported in filters"),
        Keyword::Default | Keyword::To | Keyword::Where | Keyword::Use | Keyword::IgnoreOnError => {
            format!("'{word}' is a reserved keyword; write @{word} to use it as a name")
        }
        _ => format!("only expressions are supported in filters ('{word}' starts a statement)"),
    }
}

/// The height of the tallest child.
fn children_depth(kind: &ExprKind) -> usize {
    let max =
        |exprs: &mut dyn Iterator<Item = &Expr>| exprs.map(|expr| expr.depth).max().unwrap_or(0);
    match kind {
        ExprKind::Literal(_) | ExprKind::Path(_) => 0,
        ExprKind::Member { target, .. } => target.depth,
        ExprKind::Index { target, index } => target.depth.max(index.depth),
        ExprKind::Unary { operand, .. } => operand.depth,
        ExprKind::Binary { lhs, rhs, .. } => lhs.depth.max(rhs.depth),
        ExprKind::In {
            item, collection, ..
        } => item.depth.max(collection.depth),
        ExprKind::Conditional {
            condition,
            then,
            otherwise,
        } => condition.depth.max(then.depth).max(otherwise.depth),
        ExprKind::And(items) | ExprKind::Or(items) | ExprKind::Array(items) => {
            max(&mut items.iter())
        }
        ExprKind::Dict(entries) => max(&mut entries.iter().map(|(_, value)| value)),
        ExprKind::Call { args, .. } => max(&mut args.iter()),
        ExprKind::Method { receiver, args, .. } => receiver.depth.max(max(&mut args.iter())),
    }
}

/// Evaluates operators on literals while parsing, so `-5`, `5m * 2` and
/// `["a", "b"]` cost nothing per evaluation. Expressions that fail are kept
/// and fail when evaluated, like in Icinga.
fn fold(expr: Expr) -> Expr {
    let literal = |expr: &Expr| expr.as_literal().is_some();
    let foldable = match &expr.kind {
        ExprKind::Unary { operand, .. } => literal(operand),
        ExprKind::Binary { lhs, rhs, .. } => literal(lhs) && literal(rhs),
        ExprKind::In {
            item, collection, ..
        } => literal(item) && literal(collection),
        ExprKind::Index { target, index } => literal(target) && literal(index),
        ExprKind::Member { target, .. } => literal(target),
        ExprKind::And(items) | ExprKind::Or(items) | ExprKind::Array(items) => {
            items.iter().all(literal)
        }
        ExprKind::Dict(entries) => entries.iter().all(|(_, value)| literal(value)),
        ExprKind::Conditional { condition, .. } => literal(condition),
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::Call { .. }
        | ExprKind::Method { .. } => false,
    };
    if !foldable {
        return expr;
    }
    if let ExprKind::Conditional {
        condition,
        then,
        otherwise,
    } = expr.kind
    {
        let truthy = condition.as_literal().is_some_and(Value::is_truthy);
        return if truthy { *then } else { *otherwise };
    }
    let empty = Chain { scopes: &[] };
    match Evaluator::new(&empty, None, "").eval(&expr) {
        Ok(value) => Expr::literal(value, expr.span),
        Err(_) => expr,
    }
}

/// Compiles a literal pattern argument of `match`, `regex` or `cidr_match`.
fn precompile(function: Function, args: &[Expr]) -> Result<Function, ParseError> {
    let Some((pattern, span)) = args.first().and_then(|arg| {
        arg.as_literal()
            .map(|value| (value.to_icinga_string(), arg.span))
    }) else {
        return Ok(function);
    };
    let fail = |message: String| error(message, span.start);
    Ok(match function {
        Function::Match(_) => Function::Match(PatternArg::Compiled(Arc::new(
            Glob::new(&pattern).map_err(fail)?,
        ))),
        Function::Regex(_) => Function::Regex(PatternArg::Compiled(Arc::new(
            IcingaRegex::new(&pattern).map_err(fail)?,
        ))),
        Function::CidrMatch(_) => Function::CidrMatch(PatternArg::Compiled(Arc::new(
            Cidr::new(&pattern).map_err(fail)?,
        ))),
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders the tree in a compact prefix form for precedence tests.
    fn show(expr: &Expr) -> String {
        match &expr.kind {
            ExprKind::Literal(value) => match value {
                Value::String(text) => format!("{text:?}"),
                other => other.to_icinga_string(),
            },
            ExprKind::Path(segments) => segments.join("."),
            ExprKind::Member { target, name } => format!("(. {} {name})", show(target)),
            ExprKind::Index { target, index } => format!("([] {} {})", show(target), show(index)),
            ExprKind::Unary { op, operand } => format!("({op:?} {})", show(operand)),
            ExprKind::Binary { op, lhs, rhs } => format!("({op:?} {} {})", show(lhs), show(rhs)),
            ExprKind::And(items) => format!(
                "(And {})",
                items.iter().map(show).collect::<Vec<_>>().join(" ")
            ),
            ExprKind::Or(items) => format!(
                "(Or {})",
                items.iter().map(show).collect::<Vec<_>>().join(" ")
            ),
            ExprKind::In {
                item,
                collection,
                negated,
            } => format!(
                "({} {} {})",
                if *negated { "NotIn" } else { "In" },
                show(item),
                show(collection)
            ),
            ExprKind::Conditional {
                condition,
                then,
                otherwise,
            } => format!("(? {} {} {})", show(condition), show(then), show(otherwise)),
            ExprKind::Array(items) => {
                format!("[{}]", items.iter().map(show).collect::<Vec<_>>().join(" "))
            }
            ExprKind::Dict(entries) => format!(
                "{{{}}}",
                entries
                    .iter()
                    .map(|(key, value)| format!("{key}={}", show(value)))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            ExprKind::Call { function, args } => format!(
                "({}() {})",
                function.name(),
                args.iter().map(show).collect::<Vec<_>>().join(" ")
            ),
            ExprKind::Method {
                receiver,
                name,
                args,
            } => format!(
                "(.{name}() {} {})",
                show(receiver),
                args.iter().map(show).collect::<Vec<_>>().join(" ")
            ),
        }
    }

    fn tree(source: &str) -> String {
        show(&parse(source).unwrap().unwrap())
    }

    fn parse_error(source: &str) -> ParseError {
        parse(source).unwrap_err()
    }

    #[test]
    fn precedence_follows_icinga() {
        let cases = [
            ("a || b && c", "(Or a (And b c))"),
            ("a && b || c", "(Or (And a b) c)"),
            ("a || b || c", "(Or a b c)"),
            ("(a || b) || c", "(Or a b c)"),
            ("a && (b && c)", "(And a b c)"),
            ("a | b && c", "(And (BitOr a b) c)"),
            ("a | b ^ c", "(BitOr a (BitXor b c))"),
            ("a ^ b & c", "(BitXor a (BitAnd b c))"),
            ("a & b == c", "(BitAnd a (Equal b c))"),
            ("a == b in c", "(Equal a (In b c))"),
            ("a in b == c", "(Equal (In a b) c)"),
            ("a in b < c", "(In a (Less b c))"),
            ("a in b in c", "(In (In a b) c)"),
            ("a !in b", "(NotIn a b)"),
            ("a < b << c", "(Less a (ShiftLeft b c))"),
            ("a << b + c", "(ShiftLeft a (Add b c))"),
            ("a + b * c", "(Add a (Multiply b c))"),
            ("a - b - c", "(Subtract (Subtract a b) c)"),
            ("a / b % c", "(Modulo (Divide a b) c)"),
            ("-a * b", "(Multiply (Negate a) b)"),
            ("!a == b", "(Equal (Not a) b)"),
            ("!a.b", "(Not a.b)"),
            ("!a && b", "(And (Not a) b)"),
            ("-a.b(c)[d]", "(Negate ([] (.b() a c) d))"),
            ("~a + b", "(Add (BitNot a) b)"),
            ("a * -b", "(Multiply a (Negate b))"),
            ("!-a", "(Not (Negate a))"),
            ("+a", "a"),
            ("a ? b : c", "(? a b c)"),
            ("a || b ? c : d", "(? (Or a b) c d)"),
            ("a ? b : c ? d : e", "(? a b (? c d e))"),
            ("a ? b ? c : d : e", "(? a (? b c d) e)"),
            ("a ? b : c || d", "(? a b (Or c d))"),
            ("x + a ? b : c", "(? (Add x a) b c)"),
            ("(a)", "a"),
            ("a.b.c", "a.b.c"),
            ("a[\"b\"].c", "a.b.c"),
            ("a[0]", "a.0"),
            ("a[1.5]", "a.1.500000"),
            ("a[-1]", "a.-1"),
            ("a[b]", "([] a b)"),
            ("a[b].c", "(. ([] a b) c)"),
            ("a.@default", "a.default"),
            ("f(a, b)", "(f() a b)"),
            ("f()", "(f() )"),
            ("f(a,)", "(f() a)"),
            ("a.len()", "(.len() a )"),
            ("a[\"len\"]()", "(.len() a )"),
            ("a.b.contains(\"x\")", "(.contains() a.b \"x\")"),
            ("\"a,b\".split(\",\")[0]", "([] (.split() \"a,b\" \",\") 0)"),
        ];
        for (source, expected) in cases {
            assert_eq!(tree(source), expected, "{source}");
        }
    }

    #[test]
    fn icinga_operator_test_suite_parses() {
        // Expressions from Icinga's test/config-ops.cpp.
        let cases = [
            ("2 + 3 * 4", "14"),
            ("(2 + 3) * 4", "20"),
            ("2 * - 3", "-6"),
            ("-(2 + 3)", "-5"),
            ("- 2 * 2 - 2 * 3 - 4 * - 5", "10"),
            ("!0 == true", "true"),
            ("~0", "-1"),
            ("4 << 8", "1024"),
            ("1024 >> 4", "64"),
            ("2 << 3 << 4", "256"),
            ("256 >> 4 >> 3", "2"),
            ("5m * 10", "3000"),
            ("5m / 5", "60"),
            ("7 & 3", "3"),
            ("2 | 3", "3"),
            ("(7 | 8) == 15", "true"),
            ("(7 ^ 8) == 15", "true"),
            ("(7 & 15) == 7", "true"),
            ("7 in [7] == true", "true"),
            ("7 !in [7] == false", "true"),
            ("(7 | 8) > 14", "true"),
            ("1 + 0 ? 2 : 3 + 4", "2"),
            ("0 + 0 ? 2 : 3 + 4", "7"),
            ("1 ? 2 : 3 ? 4 : 5 ? 6 : 7", "2"),
            ("0 ? 2 : 3 ? 4 : 5 ? 6 : 7", "4"),
            ("0 ? 2 : 0 ? 4 : 5 ? 6 : 7", "6"),
            ("0 ? 2 : 0 ? 4 : 0 ? 6 : 7", "7"),
            ("{ a = 3 }.a", "3"),
            ("[ 2, 3 ][1]", "3"),
        ];
        for (source, expected) in cases {
            assert_eq!(tree(source), expected, "{source} is folded to a constant");
        }
    }

    #[test]
    fn constants_are_folded() {
        assert_eq!(tree("-5"), "-5");
        assert_eq!(tree("[1, \"a\", [true]]"), "[ 1.000000, \"a\", [ true ] ]");
        assert_eq!(
            tree("{ a = 1, \"b c\" = 2 }"),
            "{\n\ta = 1.000000\n\t\"b c\" = 2.000000\n}"
        );
        assert_eq!(tree("true ? x : y"), "x");
        assert_eq!(tree("\"\" ? x : y"), "y");
        assert_eq!(tree("x + 1 * 2"), "(Add x 2)");
        // Errors are kept for evaluation time (and short-circuiting).
        assert_eq!(tree("1 / 0"), "(Divide 1 0)");
        assert_eq!(tree("false && 1 / 0"), "(And false (Divide 1 0))");
        assert_eq!(tree("\"a\" in \"b\""), "(In \"a\" \"b\")");
        // Calls are never folded (get_time() is not constant).
        assert_eq!(tree("len(\"abc\")"), "(len() \"abc\")");
    }

    #[test]
    fn line_breaks_and_separators() {
        assert_eq!(tree("\n\n a == 1 \n\n"), "(Equal a 1)");
        assert_eq!(tree("a == 1;"), "(Equal a 1)");
        assert_eq!(tree("a == 1,\n"), "(Equal a 1)");
        assert_eq!(
            tree("(a == 1 &&\n b == 2)"),
            "(And (Equal a 1) (Equal b 2))"
        );
        assert_eq!(tree("f(a,\n b)"), "(f() a b)");
        assert_eq!(tree("x in [\n 1,\n 2\n]"), "(In x [ 1.000000, 2.000000 ])");
        assert_eq!(tree("x in [ 1, 2,\n]"), "(In x [ 1.000000, 2.000000 ])");
        assert_eq!(tree("{\n a = 1\n b = 2\n}.b"), "2");
        assert_eq!(tree("{ a = 1; b = 2; }.a"), "1");
    }

    #[test]
    fn statements_are_rejected() {
        let cases = [
            ("host.name = \"x\"", 10, "assignments are not supported"),
            ("x += 1", 2, "assignments ('+=')"),
            ("var x = 1", 0, "'var' starts a statement"),
            ("const X = 1", 0, "'const' starts a statement"),
            ("if (x) { 1 }", 0, "'if' starts a statement"),
            ("for (x in y) { }", 0, "'for' starts a statement"),
            ("while (true) { }", 0, "'while' starts a statement"),
            ("return 1", 0, "'return' starts a statement"),
            ("break", 0, "'break' starts a statement"),
            ("throw \"x\"", 0, "'throw' starts a statement"),
            ("try { } except { }", 0, "'try' starts a statement"),
            ("object Host \"x\" { }", 0, "'object' starts a statement"),
            ("apply Service \"x\" { }", 0, "'apply' starts a statement"),
            ("include \"x\"", 0, "'include' starts a statement"),
            ("import \"x\"", 0, "'import' starts a statement"),
            ("assign where x", 0, "'assign' starts a statement"),
            ("namespace X { }", 0, "'namespace' starts a statement"),
            ("using X", 0, "'using' starts a statement"),
            ("function f() { }", 0, "functions are not supported"),
            ("function() { 1 }", 0, "functions are not supported"),
            ("x => x * 2", 2, "lambdas"),
            ("(x) => x", 4, "lambdas"),
            ("{{ 3 }}", 0, "'{{' starts a function"),
            ("this.x", 0, "'this' is not supported"),
            ("globals.x", 0, "'globals' is not supported"),
            ("locals", 0, "'locals' is not supported"),
            ("current_line", 0, "'current_line' is not supported"),
            ("&x", 0, "references"),
            ("*x", 0, "references"),
            ("a; b", 3, "only a single expression"),
            ("a, b", 3, "only a single expression"),
            ("a\nb", 2, "only a single expression"),
            ("a;;", 2, "unexpected ';'"),
        ];
        for (source, offset, message) in cases {
            let error = parse_error(source);
            assert!(
                error.message.contains(message),
                "{source}: expected {message:?}, got {error:?}"
            );
            assert_eq!(error.offset, offset, "{source}: {error:?}");
        }
    }

    #[test]
    fn syntax_errors_have_offsets_and_advice() {
        let cases = [
            (
                "host.name ==",
                12,
                "unexpected end of filter, expected an expression",
            ),
            ("== 3", 0, "unexpected '==', expected an expression"),
            ("(a", 2, "expected ')' to close the '(' at byte 0"),
            ("a)", 1, "unexpected ')'"),
            ("()", 0, "empty parentheses"),
            ("f(a b)", 4, "expected ',' or ')'"),
            ("[1 2]", 3, "expected ',' or ']'"),
            ("[1\n, 2]", 3, "expected ']' after a line break"),
            ("a.", 2, "expected a name after '.'"),
            ("a.5", 2, "expected a name after '.'"),
            ("a[1", 3, "expected ']'"),
            ("a ? b", 5, "expected ':'"),
            ("a == b == c", 7, "can't be chained"),
            ("a != b == c", 7, "can't be chained"),
            ("a < b < c", 6, "can't be chained"),
            ("a <= b > c", 7, "can't be chained"),
            ("a == b < c == d", 11, "can't be chained"),
            ("a ==\n b", 4, "line break"),
            ("a\n&& b", 2, "wrap a multi-line filter in parentheses"),
            ("a &&\n b", 4, "unexpected line break"),
            ("!inactive", 0, "'!in' is the 'not in' operator"),
            ("a<b>c", 1, "include path"),
            ("service.state<2&&host.state>0", 13, "include path"),
            ("host.vars.default", 10, "'default' is a reserved keyword"),
            ("host.vars.in", 10, "'in' is a reserved keyword"),
            ("{ default = 1 }", 2, "reserved keyword"),
            ("{ a.b = 1 }", 3, "nested keys"),
            (
                "{ a += 1 }",
                4,
                "'+=' is not supported in dictionary literals",
            ),
            ("{ a 1 }", 4, "expected '=' after the dictionary key"),
            ("{ a = 1 b = 2 }", 8, "expected ',' or '}'"),
            ("{ 1 = 2 }", 2, "expected a key"),
            ("{ a = { b = 1 }}", 14, "'}}' ends a function"),
            ("x.y(1)(2)", 6, "only functions and methods can be called"),
            ("(1)(2)", 3, "only functions and methods can be called"),
            ("a.b(1", 5, "expected ',' or ')'"),
            ("default", 0, "reserved keyword"),
            (
                "a b",
                2,
                "unexpected identifier 'b', expected an operator or the end of the filter",
            ),
            ("1 2", 2, "unexpected number"),
            ("x in", 4, "unexpected end of filter"),
        ];
        for (source, offset, message) in cases {
            let error = parse_error(source);
            assert!(
                error.message.contains(message),
                "{source}: expected {message:?}, got {error:?}"
            );
            assert_eq!(error.offset, offset, "{source}: {error:?}");
        }
    }

    #[test]
    fn literal_patterns_are_compiled_while_parsing() {
        let compiled = |source: &str| {
            let expr = parse(source).unwrap().unwrap();
            match expr.kind {
                ExprKind::Call { function, .. } => matches!(
                    function,
                    Function::Match(PatternArg::Compiled(_))
                        | Function::Regex(PatternArg::Compiled(_))
                        | Function::CidrMatch(PatternArg::Compiled(_))
                ),
                _ => false,
            }
        };
        assert!(compiled("match(\"pg_*\", service.name)"));
        assert!(compiled("regex(\"^db\", host.name)"));
        assert!(compiled("cidr_match(\"10.0.0.0/8\", host.address)"));
        assert!(compiled("match(5, x)"), "non-string literals are converted");
        assert!(!compiled("match(host.vars.pattern, service.name)"));
        assert!(!compiled("len(x)"));

        let error = parse_error("x && regex(\"(\", host.name)");
        assert_eq!(error.offset, 11);
        assert!(
            error.message.contains("invalid regular expression"),
            "{error:?}"
        );
        let error = parse_error("cidr_match(\"10.0.0.1/8\", host.address)");
        assert_eq!(error.offset, 11);
        assert!(error.message.contains("masked-off bits"), "{error:?}");
    }

    #[test]
    fn nesting_is_bounded() {
        let deep_parens = format!("{}x{}", "(".repeat(10_000), ")".repeat(10_000));
        assert!(
            parse_error(&deep_parens)
                .message
                .contains("nested too deeply")
        );
        let deep_not = format!("{}x", "!".repeat(10_000));
        assert!(parse_error(&deep_not).message.contains("nested too deeply"));
        let deep_minus = format!("{}x", "- ".repeat(10_000));
        assert!(
            parse_error(&deep_minus)
                .message
                .contains("nested too deeply")
        );
        let long_sum = vec!["x"; 10_000].join(" + ");
        assert!(parse_error(&long_sum).message.contains("nested too deeply"));
        let deep_array = format!("{}1{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(
            parse_error(&deep_array)
                .message
                .contains("nested too deeply")
        );
        let deep_calls = format!("{}x{}", "len(".repeat(10_000), ")".repeat(10_000));
        assert!(
            parse_error(&deep_calls)
                .message
                .contains("nested too deeply")
        );
        let deep_ternary = format!("{}x", "a ? b : ".repeat(10_000));
        assert!(
            parse_error(&deep_ternary)
                .message
                .contains("nested too deeply")
        );
        // Long chains of || and && are flat and fine.
        let long_or = vec!["host.name == \"x\""; 10_000].join(" || ");
        let expr = parse(&long_or).unwrap().unwrap();
        assert!(expr.depth <= 3, "depth {}", expr.depth);
        let ok = format!("{}x{}", "(".repeat(50), ")".repeat(50));
        assert!(parse(&ok).is_ok());
    }
}
