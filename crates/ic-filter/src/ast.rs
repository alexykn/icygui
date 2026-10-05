//! Syntax tree of a parsed filter.

use std::sync::Arc;

use crate::pattern::{Cidr, Glob, IcingaRegex, PatternCache};
use crate::value::Value;

/// A byte range in the filter source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl Span {
    pub(crate) fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }

    /// The smallest span covering both.
    pub(crate) fn to(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

/// An expression node.
#[derive(Clone, Debug)]
pub(crate) struct Expr {
    pub(crate) kind: ExprKind,
    pub(crate) span: Span,
    /// Height of the tree under this node (a leaf is 1). The parser bounds
    /// it so that evaluating and dropping the tree can't overflow the stack.
    pub(crate) depth: usize,
}

impl Expr {
    pub(crate) fn literal(value: Value, span: Span) -> Expr {
        Expr {
            kind: ExprKind::Literal(value),
            span,
            depth: 1,
        }
    }

    pub(crate) fn as_literal(&self) -> Option<&Value> {
        match &self.kind {
            ExprKind::Literal(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ExprKind {
    Literal(Value),
    /// A variable followed by constant member names, `host.vars["os"]` is
    /// `["host", "vars", "os"]`. Resolved through the scope in one lookup.
    Path(Vec<Box<str>>),
    /// `target.name` (or `target["name"]`) on something that isn't a path.
    Member {
        target: Box<Expr>,
        name: Box<str>,
    },
    /// `target[index]` with a computed index.
    Index {
        target: Box<Expr>,
        index: Box<Expr>,
    },
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// `a && b && …`: the first falsy operand, or the last one.
    And(Vec<Expr>),
    /// `a || b || …`: the first truthy operand, or the last one.
    Or(Vec<Expr>),
    /// `item in collection` or `item !in collection`.
    In {
        item: Box<Expr>,
        collection: Box<Expr>,
        negated: bool,
    },
    /// `condition ? then : otherwise`.
    Conditional {
        condition: Box<Expr>,
        then: Box<Expr>,
        otherwise: Box<Expr>,
    },
    Array(Vec<Expr>),
    Dict(Vec<(Box<str>, Expr)>),
    Call {
        function: Function,
        args: Vec<Expr>,
    },
    Method {
        receiver: Box<Expr>,
        name: Box<str>,
        args: Vec<Expr>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    /// `!`
    Not,
    /// `~`
    BitNot,
    /// `-`
    Negate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    ShiftLeft,
    ShiftRight,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
    Equal,
    NotEqual,
    BitAnd,
    BitXor,
    BitOr,
}

/// A global function, resolved by name while parsing.
#[derive(Clone, Debug)]
pub(crate) enum Function {
    Match(PatternArg<Glob>),
    Regex(PatternArg<IcingaRegex>),
    CidrMatch(PatternArg<Cidr>),
    Len,
    TypeOf,
    String,
    Number,
    Bool,
    Intersection,
    Union,
    Range,
    Keys,
    GetTime,
    /// Calling a type: `String(x)`, `Number(x)`, `Boolean(x)`.
    Construct(Primitive),
    /// An Icinga function filters don't support (`get_host`, `log`, …).
    Unsupported(Box<str>),
    /// Not a function at all. Calling it is an evaluation error.
    Unknown(Box<str>),
}

/// The types that can be called to convert a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Primitive {
    String,
    Number,
    Boolean,
}

impl Primitive {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Primitive::String => "String",
            Primitive::Number => "Number",
            Primitive::Boolean => "Boolean",
        }
    }
}

/// Icinga functions that exist but aren't available in filters here.
const UNSUPPORTED: [&str; 33] = [
    "assert",
    "basename",
    "dirname",
    "escape_create_process_arg",
    "escape_shell_arg",
    "escape_shell_cmd",
    "exit",
    "get_check_command",
    "get_event_command",
    "get_host",
    "get_host_group",
    "get_notification_command",
    "get_object",
    "get_objects",
    "get_service",
    "get_service_group",
    "get_services",
    "get_template",
    "get_templates",
    "get_time_period",
    "get_user",
    "get_user_group",
    "getenv",
    "glob",
    "glob_recursive",
    "log",
    "macro",
    "msi_get_component_path",
    "parse_performance_data",
    "path_exists",
    "ptr",
    "random",
    "sleep",
];

impl Function {
    pub(crate) fn from_name(name: &str) -> Function {
        match name {
            "match" => Function::Match(PatternArg::dynamic()),
            "regex" => Function::Regex(PatternArg::dynamic()),
            "cidr_match" => Function::CidrMatch(PatternArg::dynamic()),
            "len" => Function::Len,
            "typeof" => Function::TypeOf,
            "string" => Function::String,
            "number" => Function::Number,
            "bool" => Function::Bool,
            "intersection" => Function::Intersection,
            "union" => Function::Union,
            "range" => Function::Range,
            "keys" => Function::Keys,
            "get_time" => Function::GetTime,
            "String" => Function::Construct(Primitive::String),
            "Number" => Function::Construct(Primitive::Number),
            "Boolean" => Function::Construct(Primitive::Boolean),
            _ if UNSUPPORTED.contains(&name) => Function::Unsupported(name.into()),
            _ => Function::Unknown(name.into()),
        }
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Function::Match(_) => "match",
            Function::Regex(_) => "regex",
            Function::CidrMatch(_) => "cidr_match",
            Function::Len => "len",
            Function::TypeOf => "typeof",
            Function::String => "string",
            Function::Number => "number",
            Function::Bool => "bool",
            Function::Intersection => "intersection",
            Function::Union => "union",
            Function::Range => "range",
            Function::Keys => "keys",
            Function::GetTime => "get_time",
            Function::Construct(primitive) => primitive.name(),
            Function::Unsupported(name) | Function::Unknown(name) => name,
        }
    }
}

/// The pattern argument of `match`, `regex` and `cidr_match`.
#[derive(Debug)]
pub(crate) enum PatternArg<T> {
    /// A literal pattern, compiled while parsing.
    Compiled(Arc<T>),
    /// A computed pattern, compiled when evaluated; the last one is cached.
    Dynamic(PatternCache<T>),
}

/// Compiled patterns are shared; a cache starts empty.
impl<T> Clone for PatternArg<T> {
    fn clone(&self) -> Self {
        match self {
            PatternArg::Compiled(compiled) => PatternArg::Compiled(Arc::clone(compiled)),
            PatternArg::Dynamic(cache) => PatternArg::Dynamic(cache.clone()),
        }
    }
}

impl<T> PatternArg<T> {
    fn dynamic() -> Self {
        PatternArg::Dynamic(PatternCache::default())
    }
}
