//! Tree-walking evaluator.
//!
//! Variables are looked up through the [`Scope`] with as much of their
//! member path as is known while parsing (`host.vars.role` is one lookup),
//! longest first. Whatever the scope doesn't resolve is indexed here with
//! Icinga's rules: members of `null` are `null`, missing dictionary keys and
//! array indexes are `null`, and members of strings, numbers and booleans are
//! errors.

use std::collections::BTreeMap;
use std::sync::Arc;

use ic_model::Timestamp;

use crate::ast::{BinaryOp, Expr, ExprKind, Span, UnaryOp};
use crate::ops::{self, Comparison};
use crate::scope::Scope;
use crate::value::Value;
use crate::{EvalError, functions, methods};

/// Paths up to this many segments are resolved without allocating.
const INLINE_PATH: usize = 16;

/// Evaluates expressions against a scope.
pub(crate) struct Evaluator<'a> {
    scope: &'a dyn Scope,
    now: Option<f64>,
    source: &'a str,
}

impl<'a> Evaluator<'a> {
    /// `now` fixes what `get_time()` returns (Unix seconds); `None` reads
    /// the clock. `source` is used to quote the failing part in errors.
    pub(crate) fn new(scope: &'a dyn Scope, now: Option<f64>, source: &'a str) -> Self {
        Evaluator { scope, now, source }
    }

    /// The current time for `get_time()`.
    pub(crate) fn now(&self) -> f64 {
        self.now
            .unwrap_or_else(|| Timestamp::now().as_unix_seconds())
    }

    /// An evaluation error, quoting the part of the filter that failed.
    #[cold]
    pub(crate) fn fail(&self, span: Span, message: impl Into<String>) -> EvalError {
        let mut message = message.into();
        let snippet = self.source.get(span.start..span.end).unwrap_or_default();
        let snippet = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
        if !snippet.is_empty() {
            const MAX_CHARS: usize = 60;
            message.push_str(" (in `");
            if snippet.chars().count() > MAX_CHARS {
                message.extend(snippet.chars().take(MAX_CHARS));
                message.push('…');
            } else {
                message.push_str(&snippet);
            }
            message.push_str("`)");
        }
        EvalError::new(message)
    }

    /// Evaluates an expression. Each kind of node has its own function, so
    /// the frames on the recursive path stay small in unoptimised builds.
    pub(crate) fn eval(&self, expr: &Expr) -> Result<Value, EvalError> {
        match &expr.kind {
            ExprKind::Literal(value) => Ok(value.clone()),
            ExprKind::Path(segments) => self.path(segments, expr.span),
            ExprKind::Member { target, name } => self.member(target, name, expr.span),
            ExprKind::Index { target, index } => self.index(target, index, expr.span),
            ExprKind::Unary { op, operand } => self.unary(*op, operand, expr.span),
            ExprKind::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, expr.span),
            ExprKind::And(items) => self.chain(items, false),
            ExprKind::Or(items) => self.chain(items, true),
            ExprKind::In {
                item,
                collection,
                negated,
            } => self.membership(item, collection, *negated, expr.span),
            ExprKind::Conditional {
                condition,
                then,
                otherwise,
            } => self.conditional(condition, then, otherwise),
            ExprKind::Array(items) => self.array(items),
            ExprKind::Dict(entries) => self.dict(entries),
            ExprKind::Call { function, args } => functions::call(self, function, args, expr.span),
            ExprKind::Method {
                receiver,
                name,
                args,
            } => self.method(receiver, name, args, expr.span),
        }
    }

    fn member(&self, target: &Expr, name: &str, span: Span) -> Result<Value, EvalError> {
        let target = self.eval(target)?;
        self.field(&target, name, span)
    }

    fn index(&self, target: &Expr, index: &Expr, span: Span) -> Result<Value, EvalError> {
        let target = self.eval(target)?;
        let index = self.eval(index)?;
        self.field(&target, &index.to_icinga_string(), span)
    }

    fn unary(&self, op: UnaryOp, operand: &Expr, span: Span) -> Result<Value, EvalError> {
        let operand = self.eval(operand)?;
        match op {
            UnaryOp::Not => Ok(Value::Bool(!operand.is_truthy())),
            UnaryOp::BitNot => ops::bit_not(&operand).map_err(|message| self.fail(span, message)),
            UnaryOp::Negate => ops::negate(&operand).map_err(|message| self.fail(span, message)),
        }
    }

    fn binary(&self, op: BinaryOp, lhs: &Expr, rhs: &Expr, span: Span) -> Result<Value, EvalError> {
        let lhs = self.eval(lhs)?;
        let rhs = self.eval(rhs)?;
        binary(op, &lhs, &rhs).map_err(|message| self.fail(span, message))
    }

    fn conditional(
        &self,
        condition: &Expr,
        then: &Expr,
        otherwise: &Expr,
    ) -> Result<Value, EvalError> {
        if self.eval(condition)?.is_truthy() {
            self.eval(then)
        } else {
            self.eval(otherwise)
        }
    }

    fn array(&self, items: &[Expr]) -> Result<Value, EvalError> {
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(self.eval(item)?);
        }
        Ok(Value::array(values))
    }

    fn dict(&self, entries: &[(Box<str>, Expr)]) -> Result<Value, EvalError> {
        let mut map = BTreeMap::new();
        for (key, value) in entries {
            map.insert(key.to_string(), self.eval(value)?);
        }
        Ok(Value::Dict(Arc::new(map)))
    }

    /// `&&` (`stop_when` false) and `||` (true): the first operand whose
    /// truthiness is `stop_when`, or the last one. Like Icinga, the result
    /// is an operand, not necessarily a boolean.
    fn chain(&self, items: &[Expr], stop_when: bool) -> Result<Value, EvalError> {
        let mut last = Value::Null;
        for item in items {
            last = self.eval(item)?;
            if last.is_truthy() == stop_when {
                break;
            }
        }
        Ok(last)
    }

    /// `item in collection` / `item !in collection`.
    fn membership(
        &self,
        item: &Expr,
        collection: &Expr,
        negated: bool,
        span: Span,
    ) -> Result<Value, EvalError> {
        // Icinga evaluates the collection first; an empty one (null or "")
        // contains nothing, and the item isn't evaluated.
        let collection = self.eval(collection)?;
        if collection.is_empty_value() {
            return Ok(Value::Bool(negated));
        }
        let Value::Array(items) = &collection else {
            return Err(self.fail(
                span,
                format!(
                    "invalid right side argument for '{}' operator: {} (expected an array)",
                    if negated { "!in" } else { "in" },
                    collection.to_json_text()
                ),
            ));
        };
        let item = self.eval(item)?;
        Ok(Value::Bool(ops::contains(items, &item) != negated))
    }

    fn method(
        &self,
        receiver: &Expr,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Value, EvalError> {
        let receiver = self.eval(receiver)?;
        // Like Icinga, find the method before evaluating the arguments.
        let method =
            methods::resolve(&receiver, name).map_err(|message| self.fail(span, message))?;
        let args = self.eval_all(args)?;
        methods::invoke(method, &receiver, &args).map_err(|message| self.fail(span, message))
    }

    /// Evaluates arguments in order.
    pub(crate) fn eval_all(&self, args: &[Expr]) -> Result<Vec<Value>, EvalError> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            values.push(self.eval(arg)?);
        }
        Ok(values)
    }

    /// Resolves a variable path, longest known prefix first.
    fn path(&self, segments: &[Box<str>], span: Span) -> Result<Value, EvalError> {
        let mut inline = [""; INLINE_PATH];
        let heap: Vec<&str>;
        let names: &[&str] = if segments.len() <= INLINE_PATH {
            for (slot, segment) in inline.iter_mut().zip(segments) {
                *slot = segment;
            }
            &inline[..segments.len()]
        } else {
            heap = segments.iter().map(AsRef::as_ref).collect();
            &heap
        };
        for known in (1..=names.len()).rev() {
            if let Some(value) = self.scope.lookup(&names[..known]) {
                return self.index_path(value, &names[known..], span);
            }
        }
        match names.split_first() {
            Some((root, rest)) => match builtin(root) {
                Some(value) => self.index_path(value, rest, span),
                None => Ok(Value::Null),
            },
            None => Ok(Value::Null),
        }
    }

    fn index_path(&self, mut value: Value, rest: &[&str], span: Span) -> Result<Value, EvalError> {
        for name in rest {
            value = self.field(&value, name, span)?;
        }
        Ok(value)
    }

    /// `target.name` / `target[name]`.
    fn field(&self, target: &Value, name: &str, span: Span) -> Result<Value, EvalError> {
        match lookup_field(target, name) {
            Field::Found(value) => Ok(value),
            Field::Missing => Ok(Value::Null),
            Field::Invalid => Err(self.fail(span, field_error(target, name))),
        }
    }
}

/// The result of looking up a member.
pub(crate) enum Field {
    Found(Value),
    /// A missing dictionary key or array index, or a member of `null`.
    Missing,
    /// Strings, numbers and booleans have no members; arrays only indexes.
    Invalid,
}

/// Looks up `name` in a value with Icinga's rules (see the module docs).
pub(crate) fn lookup_field(target: &Value, name: &str) -> Field {
    match target {
        Value::Null => Field::Missing,
        Value::Dict(entries) => entries
            .get(name)
            .map_or(Field::Missing, |value| Field::Found(value.clone())),
        Value::Array(items) => match ops::parse_integer(name) {
            Some(index) => usize::try_from(index)
                .ok()
                .and_then(|index| items.get(index))
                .map_or(Field::Missing, |value| Field::Found(value.clone())),
            None => Field::Invalid,
        },
        Value::Bool(_) | Value::Number(_) | Value::String(_) => Field::Invalid,
    }
}

fn field_error(target: &Value, name: &str) -> String {
    if methods::resolve(target, name).is_ok() {
        format!("'{name}' is a method; call it as {name}()")
    } else {
        format!(
            "invalid field access (for value of type '{}'): '{name}'",
            target.type_name()
        )
    }
}

fn binary(op: BinaryOp, lhs: &Value, rhs: &Value) -> ops::OpResult<Value> {
    let compare = |comparison| ops::compare(comparison, lhs, rhs).map(Value::Bool);
    match op {
        BinaryOp::Add => ops::add(lhs, rhs),
        BinaryOp::Subtract => ops::subtract(lhs, rhs),
        BinaryOp::Multiply => ops::multiply(lhs, rhs),
        BinaryOp::Divide => ops::divide(lhs, rhs),
        BinaryOp::Modulo => ops::modulo(lhs, rhs),
        BinaryOp::ShiftLeft => ops::shift_left(lhs, rhs),
        BinaryOp::ShiftRight => ops::shift_right(lhs, rhs),
        BinaryOp::BitAnd => ops::bit_and(lhs, rhs),
        BinaryOp::BitXor => ops::bit_xor(lhs, rhs),
        BinaryOp::BitOr => ops::bit_or(lhs, rhs),
        BinaryOp::Less => compare(Comparison::Less),
        BinaryOp::Greater => compare(Comparison::Greater),
        BinaryOp::LessEqual => compare(Comparison::LessEqual),
        BinaryOp::GreaterEqual => compare(Comparison::GreaterEqual),
        BinaryOp::Equal => Ok(Value::Bool(ops::equals(lhs, rhs))),
        BinaryOp::NotEqual => Ok(Value::Bool(!ops::equals(lhs, rhs))),
    }
}

/// Icinga's global constants that are useful in filters. Scopes come first,
/// so a filter variable of the same name wins (as in Icinga).
fn builtin(name: &str) -> Option<Value> {
    Some(match name {
        "MatchAll" | "ServiceOK" | "HostUp" => Value::Number(0.0),
        "MatchAny" | "ServiceWarning" | "HostDown" => Value::Number(1.0),
        "ServiceCritical" => Value::Number(2.0),
        "ServiceUnknown" => Value::Number(3.0),
        // Type objects, as returned by typeof(); see `functions::type_name`.
        "Object" | "Boolean" | "Number" | "String" | "Array" | "Dictionary" => Value::str(name),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Filter;
    use crate::scope::{Chain, VarsScope};

    fn eval(source: &str) -> Result<Value, EvalError> {
        Filter::parse(source)
            .unwrap()
            .evaluate(&Chain { scopes: &[] })
    }

    #[test]
    fn errors_quote_the_failing_expression() {
        let error = eval("1 + 2 > 0 && true + 1").unwrap_err();
        assert_eq!(
            error.message,
            "operator + cannot be applied to values of type 'Boolean' and 'Number' (in `true + 1`)"
        );
        let long = format!("\"{}\" in \"x\"", "a".repeat(100));
        let error = eval(&long).unwrap_err();
        assert!(error.message.ends_with("…`)"), "{}", error.message);
        let error = eval("(true\n  +\n  1)").unwrap_err();
        assert!(
            error.message.ends_with("(in `true + 1`)"),
            "{}",
            error.message
        );
    }

    #[test]
    fn unknown_variables_are_null_and_builtins_resolve() {
        assert_eq!(eval("nothing").unwrap(), Value::Null);
        assert_eq!(eval("nothing.deeper[0].still").unwrap(), Value::Null);
        assert_eq!(eval("MatchAny").unwrap(), Value::Number(1.0));
        assert_eq!(eval("ServiceCritical").unwrap(), Value::Number(2.0));
        assert_eq!(eval("HostDown").unwrap(), Value::Number(1.0));
        assert_eq!(eval("String").unwrap(), Value::from("String"));
        assert!(eval("MatchAny.x").is_err(), "numbers have no members");

        let vars = BTreeMap::from([("MatchAny".to_owned(), Value::from("shadowed"))]);
        let scope = VarsScope { vars: &vars };
        let value = Filter::parse("MatchAny").unwrap().evaluate(&scope).unwrap();
        assert_eq!(value, Value::from("shadowed"), "scopes come first");
    }

    #[test]
    fn field_lookup_rules() {
        let dict = Value::from(BTreeMap::from([("a".to_owned(), Value::Number(1.0))]));
        assert!(matches!(
            lookup_field(&dict, "a"),
            Field::Found(Value::Number(_))
        ));
        assert!(matches!(lookup_field(&dict, "b"), Field::Missing));
        assert!(matches!(lookup_field(&Value::Null, "b"), Field::Missing));
        let array = Value::from(vec![Value::from("x")]);
        assert!(matches!(lookup_field(&array, "0"), Field::Found(_)));
        assert!(matches!(lookup_field(&array, "+0"), Field::Found(_)));
        assert!(matches!(lookup_field(&array, "1"), Field::Missing));
        assert!(matches!(lookup_field(&array, "-1"), Field::Missing));
        assert!(matches!(lookup_field(&array, "x"), Field::Invalid));
        assert!(matches!(lookup_field(&array, "0.5"), Field::Invalid));
        assert!(matches!(
            lookup_field(&Value::from("s"), "0"),
            Field::Invalid
        ));
        assert!(matches!(
            lookup_field(&Value::Number(1.0), "x"),
            Field::Invalid
        ));
        assert!(matches!(
            lookup_field(&Value::Bool(true), "x"),
            Field::Invalid
        ));

        assert_eq!(
            eval("\"abc\".foo").unwrap_err().message,
            "invalid field access (for value of type 'String'): 'foo' (in `\"abc\".foo`)"
        );
        assert_eq!(
            eval("\"abc\".len").unwrap_err().message,
            "'len' is a method; call it as len() (in `\"abc\".len`)"
        );
        assert_eq!(
            eval("{ a = 1 }.len").unwrap(),
            Value::Null,
            "dictionary keys first"
        );
    }

    #[test]
    fn long_paths_are_resolved() {
        let mut nested = Value::from("leaf");
        for _ in 0..20 {
            nested = Value::from(BTreeMap::from([("k".to_owned(), nested)]));
        }
        let vars = BTreeMap::from([("root".to_owned(), nested)]);
        let source = format!("root{}", ".k".repeat(20));
        let value = Filter::parse(&source)
            .unwrap()
            .evaluate(&VarsScope { vars: &vars })
            .unwrap();
        assert_eq!(value, Value::from("leaf"));
    }
}
