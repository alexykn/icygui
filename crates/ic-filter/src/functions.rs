//! Global functions, following `lib/base/scriptutils.cpp`.
//!
//! As in Icinga, all arguments are evaluated before the call, functions with
//! fixed parameters require exactly that many arguments, and functions
//! without parameters (`get_time`) ignore extra ones.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::EvalError;
use crate::ast::{Expr, Function, PatternArg, Primitive, Span};
use crate::budget::Budget;
use crate::eval::Evaluator;
use crate::pattern::{Cidr, Glob, IcingaRegex};
use crate::value::Value;
use crate::{methods, ops};

/// `range()` refuses to build arrays larger than this. Icinga doesn't allow
/// `range()` in API filters at all (it isn't marked safe for its sandbox);
/// small ranges are harmless and occasionally handy in dashboards.
const RANGE_LIMIT: usize = 10_000;

/// `MatchAll`: every array item has to match.
const MATCH_ALL: i32 = 0;
/// `MatchAny`: one array item has to match.
const MATCH_ANY: i32 = 1;

/// Calls a global function.
///
/// Evaluating arguments recurses into the evaluator, so this and the
/// argument helpers only dispatch; the work happens in separate functions
/// once the arguments are values.
pub(crate) fn call(
    ev: &Evaluator<'_>,
    function: &Function,
    args: &[Expr],
    span: Span,
) -> Result<Value, EvalError> {
    match function {
        Function::Match(pattern) => pattern_match(ev, "match", pattern, Glob::new, args, span),
        Function::Regex(pattern) => {
            pattern_match(ev, "regex", pattern, IcingaRegex::new, args, span)
        }
        Function::CidrMatch(pattern) => {
            pattern_match(ev, "cidr_match", pattern, Cidr::new, args, span)
        }
        Function::Len
        | Function::TypeOf
        | Function::String
        | Function::Number
        | Function::Bool
        | Function::Keys => {
            let value = single_arg(ev, function, args, span)?;
            convert(function, value, ev.budget()).map_err(|message| ev.fail(span, message))
        }
        Function::Union | Function::Intersection | Function::Range | Function::GetTime => {
            let values = ev.eval_all(args)?;
            combine(ev, function, &values).map_err(|message| ev.fail(span, message))
        }
        Function::Construct(primitive) => {
            let values = ev.eval_all(args)?;
            construct(*primitive, &values, ev.budget()).map_err(|message| ev.fail(span, message))
        }
        Function::Unsupported(_) | Function::Unknown(_) => {
            Err(ev.fail(span, not_available(function)))
        }
    }
}

/// The one-parameter functions, on their evaluated argument.
fn convert(function: &Function, value: Value, budget: &Budget) -> Result<Value, String> {
    match function {
        Function::Len => {
            #[expect(
                clippy::cast_precision_loss,
                reason = "lengths far below 2^53 are exact"
            )]
            let length = match &value {
                Value::Dict(entries) => entries.len() as f64,
                Value::Array(items) => items.len() as f64,
                Value::String(text) => text.len() as f64,
                Value::Null | Value::Bool(_) | Value::Number(_) => 0.0,
            };
            Ok(Value::Number(length))
        }
        Function::TypeOf => Ok(type_value(type_name(&value))),
        Function::String => to_string(value, budget),
        Function::Number => ops::to_number(&value).map(Value::Number),
        Function::Bool => Ok(Value::Bool(value.is_truthy())),
        Function::Keys => match value {
            Value::Dict(entries) => methods::keys(&entries, budget),
            Value::Null | Value::Array(_) => Ok(Value::array(Vec::new())),
            other => Err(format!(
                "keys() expects a dictionary, got a value of type '{}'",
                other.type_name()
            )),
        },
        other => Err(format!("{}() is not a one-argument function", other.name())),
    }
}

/// `string(value)`: strings as they are, everything else converted (and
/// charged).
fn to_string(value: Value, budget: &Budget) -> Result<Value, String> {
    Ok(match value {
        Value::String(_) => value,
        other => Value::from(budget.text_of(&other)?.into_owned()),
    })
}

/// The functions taking any number of arguments, on their values.
fn combine(ev: &Evaluator<'_>, function: &Function, values: &[Value]) -> Result<Value, String> {
    let budget = ev.budget();
    match function {
        Function::Union => union(values, budget),
        Function::Intersection => intersection(values, budget),
        Function::Range => range(values, budget),
        // Takes no parameters; extra arguments are ignored, as in Icinga.
        Function::GetTime => Ok(Value::Number(ev.now())),
        other => Err(format!(
            "{}() doesn't take a list of arguments",
            other.name()
        )),
    }
}

/// Calling a type, Icinga's `ConstructorCall`: `String(x)`, `Number(x)` and
/// `Boolean(x)` convert like `string()`, `number()` and `bool()`; without an
/// argument they give `""`, `0` and (sic) `0`.
fn construct(primitive: Primitive, args: &[Value], budget: &Budget) -> Result<Value, String> {
    let value = match args {
        [] => {
            return Ok(match primitive {
                Primitive::String => Value::str(""),
                Primitive::Number | Primitive::Boolean => Value::Number(0.0),
            });
        }
        [value] => value,
        _ => {
            return Err(format!(
                "{}() takes at most 1 argument ({} given)",
                primitive.name(),
                args.len()
            ));
        }
    };
    match primitive {
        Primitive::String => to_string(value.clone(), budget),
        Primitive::Number => ops::to_number(value).map(Value::Number),
        Primitive::Boolean => Ok(Value::Bool(value.is_truthy())),
    }
}

#[cold]
fn not_available(function: &Function) -> String {
    match function {
        Function::Unsupported(name) => {
            format!("the Icinga function '{name}()' is not available in filters")
        }
        other => format!("unknown function '{}()'", other.name()),
    }
}

/// The name of the type `typeof()` returns (`null` is an `Object`).
pub(crate) fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "Object",
        other => other.type_name(),
    }
}

/// A type, as `typeof()` returns it and the globals `String`, `Number`, …
/// hold it: a dictionary with the type's `name`. Types compare equal by
/// name, so `typeof(x) == Number` and `typeof(x).name == "Number"` work as
/// in Icinga, and a type is not equal to the string of its name.
pub(crate) fn type_value(name: &str) -> Value {
    Value::from(BTreeMap::from([("name".to_owned(), Value::str(name))]))
}

/// Evaluates the only argument of a one-parameter function.
fn single_arg(
    ev: &Evaluator<'_>,
    function: &Function,
    args: &[Expr],
    span: Span,
) -> Result<Value, EvalError> {
    match args {
        [arg] => ev.eval(arg),
        _ => Err(ev.fail(span, arity_error(function, args.len()))),
    }
}

#[cold]
fn arity_error(function: &Function, given: usize) -> String {
    format!(
        "{}() takes exactly 1 argument ({given} given)",
        function.name()
    )
}

/// Something a pattern function tests text against.
trait Matcher {
    fn test(&self, text: &str) -> bool;
}

impl Matcher for Glob {
    fn test(&self, text: &str) -> bool {
        self.is_match(text)
    }
}

impl Matcher for IcingaRegex {
    fn test(&self, text: &str) -> bool {
        self.is_match(text)
    }
}

impl Matcher for Cidr {
    fn test(&self, text: &str) -> bool {
        self.contains(text)
    }
}

/// The evaluated arguments of a pattern function.
struct PatternInputs {
    /// The pattern, unless it was compiled while parsing.
    pattern: Option<Value>,
    value: Value,
    mode: Option<Value>,
}

/// `match`, `regex` and `cidr_match`: `(pattern, value[, mode])`, where the
/// value is a string or an array tested with `MatchAll` (default) or
/// `MatchAny`.
fn pattern_match<T: Matcher>(
    ev: &Evaluator<'_>,
    name: &str,
    pattern: &PatternArg<T>,
    compile: fn(&str) -> Result<T, String>,
    args: &[Expr],
    span: Span,
) -> Result<Value, EvalError> {
    let [pattern_arg, value_arg, rest @ ..] = args else {
        return Err(ev.fail(span, pattern_arity_error(name, args.len())));
    };
    let inputs = PatternInputs {
        pattern: match pattern {
            PatternArg::Compiled(_) => None,
            PatternArg::Dynamic(_) => Some(ev.eval(pattern_arg)?),
        },
        value: ev.eval(value_arg)?,
        mode: match rest.split_first() {
            Some((mode, extra)) => {
                let mode = ev.eval(mode)?;
                ev.eval_all(extra)?;
                Some(mode)
            }
            None => None,
        },
    };
    apply_pattern(name, pattern, compile, &inputs, ev.budget())
        .map_err(|message| ev.fail(span, message))
}

#[cold]
fn pattern_arity_error(name: &str, given: usize) -> String {
    format!(
        "{name}() needs a pattern and a value ({given} argument{} given)",
        if given == 1 { "" } else { "s" }
    )
}

fn apply_pattern<T: Matcher>(
    name: &str,
    pattern: &PatternArg<T>,
    compile: fn(&str) -> Result<T, String>,
    inputs: &PatternInputs,
    budget: &Budget,
) -> Result<Value, String> {
    if let Value::Dict(_) = inputs.value {
        return Err(format!("dictionaries are not supported by {name}()"));
    }
    let mode = match &inputs.mode {
        Some(mode) => ops::to_i32(ops::to_number(mode)?),
        None => MATCH_ALL,
    };
    // Compiled last, after the other arguments are checked, as in Icinga.
    let matcher: Arc<T> = match pattern {
        PatternArg::Compiled(compiled) => Arc::clone(compiled),
        PatternArg::Dynamic(cache) => {
            let text = match &inputs.pattern {
                Some(pattern) => budget.text_of(pattern)?,
                None => Cow::Borrowed(""),
            };
            cache.get_or_compile(&text, compile)?
        }
    };
    let Value::Array(items) = &inputs.value else {
        return Ok(Value::Bool(matcher.test(&budget.text_of(&inputs.value)?)));
    };
    if items.is_empty() {
        return Ok(Value::Bool(false));
    }
    for item in items.iter() {
        let hit = matcher.test(&budget.text_of(item)?);
        if mode == MATCH_ANY && hit {
            return Ok(Value::Bool(true));
        }
        if mode == MATCH_ALL && !hit {
            return Ok(Value::Bool(false));
        }
    }
    // MatchAll: everything matched. MatchAny: nothing did. Any other mode
    // is false, as in Icinga.
    Ok(Value::Bool(mode == MATCH_ALL))
}

/// The items of an array argument; `null` is `None`.
fn array_arg<'v>(function: &str, value: &'v Value) -> Result<Option<&'v [Value]>, String> {
    match value {
        Value::Array(items) => Ok(Some(items)),
        Value::Null => Ok(None),
        other => Err(format!(
            "{function}() expects arrays, got a value of type '{}'",
            other.type_name()
        )),
    }
}

/// `union(arrays…)`: the distinct items of all arrays, sorted with `<`.
///
/// Icinga inserts every item into a `std::set`, which keeps the first of
/// items that are neither less nor greater than each other. A stable sort
/// followed by dropping such duplicates gives the same set in O(n log n).
fn union(args: &[Value], budget: &Budget) -> Result<Value, String> {
    let mut all: Vec<Value> = Vec::new();
    for arg in args {
        let Some(items) = array_arg("union", arg)? else {
            continue;
        };
        budget.items(items.len())?;
        all.extend_from_slice(items);
    }
    let mut set: Vec<Value> = Vec::with_capacity(all.len());
    for item in ops::sort_values(all)? {
        let duplicate = match set.last() {
            Some(last) => !ops::less(last, &item)? && !ops::less(&item, last)?,
            None => false,
        };
        if !duplicate {
            set.push(item);
        }
    }
    Ok(Value::array(set))
}

/// `intersection(arrays…)`: the items common to all arrays (sorted; an item
/// repeated in every array is kept as often as its smallest count).
///
/// Like Icinga, a single array gives an empty result, and a `null` argument
/// ends the computation with the result so far (empty if it's the first or
/// second argument).
fn intersection(args: &[Value], budget: &Budget) -> Result<Value, String> {
    let Some((first, rest)) = args.split_first() else {
        return Ok(Value::array(Vec::new()));
    };
    let Some(first) = array_arg("intersection", first)? else {
        return Ok(Value::array(Vec::new()));
    };
    if rest.is_empty() {
        return Ok(Value::array(Vec::new()));
    }
    budget.items(first.len())?;
    let mut current = ops::sort_values(first.to_vec())?;
    for (index, arg) in rest.iter().enumerate() {
        let Some(other) = array_arg("intersection", arg)? else {
            return Ok(Value::array(if index == 0 { Vec::new() } else { current }));
        };
        budget.items(other.len())?;
        current = intersect_sorted(&current, &ops::sort_values(other.to_vec())?)?;
    }
    Ok(Value::array(current))
}

/// `std::set_intersection` on sorted arrays.
fn intersect_sorted(left: &[Value], right: &[Value]) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    let (mut i, mut j) = (0, 0);
    while let (Some(a), Some(b)) = (left.get(i), right.get(j)) {
        if ops::less(a, b)? {
            i += 1;
        } else if ops::less(b, a)? {
            j += 1;
        } else {
            result.push(a.clone());
            i += 1;
            j += 1;
        }
    }
    Ok(result)
}

/// `range(end)`, `range(start, end)`, `range(start, end, increment)`.
fn range(args: &[Value], budget: &Budget) -> Result<Value, String> {
    let numbers = args
        .iter()
        .map(ops::to_number)
        .collect::<Result<Vec<_>, _>>()?;
    let (start, end, increment) = match numbers.as_slice() {
        [end] => (0.0, *end, 1.0),
        [start, end] => (*start, *end, 1.0),
        [start, end, increment] => (*start, *end, *increment),
        _ => {
            return Err(format!(
                "range() takes 1 to 3 arguments ({} given)",
                args.len()
            ));
        }
    };
    let mut items = Vec::new();
    if (start < end && increment <= 0.0) || (start > end && increment >= 0.0) {
        return Ok(Value::array(items));
    }
    let mut current = start;
    while if increment > 0.0 {
        current < end
    } else {
        current > end
    } {
        if items.len() == RANGE_LIMIT {
            return Err(format!(
                "range() would produce more than {RANGE_LIMIT} numbers"
            ));
        }
        items.push(Value::Number(current));
        current += increment;
    }
    budget.items(items.len())?;
    Ok(Value::array(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(value: f64) -> Value {
        Value::Number(value)
    }

    fn s(text: &str) -> Value {
        Value::from(text)
    }

    fn a(items: &[Value]) -> Value {
        Value::from(items.to_vec())
    }

    fn union_(args: &[Value]) -> Result<Value, String> {
        union(args, &Budget::new())
    }

    fn intersection_(args: &[Value]) -> Result<Value, String> {
        intersection(args, &Budget::new())
    }

    fn range_(args: &[Value]) -> Result<Value, String> {
        range(args, &Budget::new())
    }

    #[test]
    fn union_sorts_and_dedupes() {
        assert_eq!(
            union_(&[a(&[s("devs"), s("slack")]), a(&[s("slack"), s("noc")])]).unwrap(),
            a(&[s("devs"), s("noc"), s("slack")])
        );
        assert_eq!(
            union_(&[a(&[n(3.0), n(1.0)]), Value::Null, a(&[n(1.0)])]).unwrap(),
            a(&[n(1.0), n(3.0)])
        );
        assert_eq!(union_(&[]).unwrap(), a(&[]));
        assert!(
            union_(&[a(&[n(1.0), s("a")])]).is_err(),
            "mixed types can't be ordered"
        );
        assert!(union_(&[s("a")]).is_err());
    }

    #[test]
    fn intersection_follows_icinga() {
        assert_eq!(
            intersection_(&[a(&[s("devs"), s("slack")]), a(&[s("slack"), s("noc")])]).unwrap(),
            a(&[s("slack")])
        );
        assert_eq!(
            intersection_(&[a(&[n(1.0), n(1.0), n(2.0)]), a(&[n(1.0), n(1.0), n(3.0)])]).unwrap(),
            a(&[n(1.0), n(1.0)])
        );
        assert_eq!(
            intersection_(&[
                a(&[s("a"), s("b"), s("c")]),
                a(&[s("c"), s("b")]),
                a(&[s("b"), s("x"), s("y")])
            ])
            .unwrap(),
            a(&[s("b")])
        );
        assert_eq!(
            intersection_(&[a(&[n(1.0)])]).unwrap(),
            a(&[]),
            "one array: empty"
        );
        assert_eq!(intersection_(&[]).unwrap(), a(&[]));
        assert_eq!(intersection_(&[Value::Null, a(&[n(1.0)])]).unwrap(), a(&[]));
        assert_eq!(intersection_(&[a(&[n(1.0)]), Value::Null]).unwrap(), a(&[]));
        assert_eq!(
            intersection_(&[a(&[n(1.0), n(2.0)]), a(&[n(2.0)]), Value::Null]).unwrap(),
            a(&[n(2.0)]),
            "null ends with the result so far"
        );
        assert!(intersection_(&[a(&[]), s("x")]).is_err());
    }

    #[test]
    fn range_follows_icinga() {
        assert_eq!(
            range_(&[n(5.0)]).unwrap(),
            a(&[n(0.0), n(1.0), n(2.0), n(3.0), n(4.0)])
        );
        assert_eq!(range_(&[n(2.0), n(4.0)]).unwrap(), a(&[n(2.0), n(3.0)]));
        assert_eq!(
            range_(&[n(2.0), n(10.0), n(2.0)]).unwrap(),
            a(&[n(2.0), n(4.0), n(6.0), n(8.0)])
        );
        assert_eq!(
            range_(&[n(3.0), n(0.0), n(-1.0)]).unwrap(),
            a(&[n(3.0), n(2.0), n(1.0)])
        );
        assert_eq!(range_(&[n(0.0), n(3.0), n(-1.0)]).unwrap(), a(&[]));
        assert_eq!(range_(&[n(3.0), n(0.0)]).unwrap(), a(&[]));
        assert_eq!(range_(&[n(1.0), n(1.0)]).unwrap(), a(&[]));
        assert_eq!(range_(&[n(1.0), n(2.0), n(0.0)]).unwrap(), a(&[]));
        assert_eq!(
            range_(&[s("2")]).unwrap(),
            a(&[n(0.0), n(1.0)]),
            "arguments are converted"
        );
        assert_eq!(range_(&[n(f64::NAN)]).unwrap(), a(&[]));
        assert!(range_(&[]).is_err());
        assert!(range_(&[n(1.0), n(2.0), n(3.0), n(4.0)]).is_err());
        assert!(range_(&[s("x")]).is_err());
        assert!(range_(&[n(1e12)]).unwrap_err().contains("more than 10000"));
        assert_eq!(
            range_(&[n(10_000.0)])
                .unwrap()
                .as_array()
                .map(<[Value]>::len),
            Some(10_000)
        );
        assert!(range_(&[n(10_001.0)]).is_err());
        assert!(
            range_(&[n(1e16), n(1e16 + 10.0), n(0.1)]).is_err(),
            "an increment too small to change the value doesn't loop forever"
        );
    }

    #[test]
    fn type_names() {
        assert_eq!(type_name(&Value::Null), "Object");
        assert_eq!(type_name(&n(1.0)), "Number");
        assert_eq!(type_name(&Value::Bool(true)), "Boolean");
        assert_eq!(type_name(&s("")), "String");
        assert_eq!(type_name(&a(&[])), "Array");
    }

    #[test]
    fn union_keeps_the_first_of_equivalent_items() {
        // 0 and "" are neither less nor greater than each other.
        assert_eq!(
            union_(&[a(&[n(0.0), s(""), n(1.0)])]).unwrap(),
            a(&[n(0.0), n(1.0)])
        );
        assert_eq!(union_(&[a(&[s(""), n(0.0)])]).unwrap(), a(&[s("")]));
        assert_eq!(union_(&[a(&[n(1.0)])]).unwrap(), a(&[n(1.0)]));
    }

    #[test]
    fn union_takes_n_log_n_time() {
        let descending: Vec<Value> = (0..100_000)
            .rev()
            .map(|index| n(f64::from(index)))
            .collect();
        let start = std::time::Instant::now();
        let set = union_(&[a(&descending), a(&descending)]).unwrap_err();
        assert!(
            set.contains("evaluation limit reached"),
            "200 000 items: {set}"
        );
        let set = union_(&[a(&descending)]).unwrap();
        assert_eq!(set.as_array().map(<[Value]>::len), Some(100_000));
        assert_eq!(set.as_array().and_then(<[Value]>::first), Some(&n(0.0)));
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn functions_charge_what_they_create() {
        let budget = Budget::new();
        budget.items(crate::budget::MAX_ITEMS - 5).unwrap();
        assert!(range(&[n(5.0)], &budget).is_ok());
        assert!(
            range(&[n(1.0)], &budget)
                .unwrap_err()
                .contains("evaluation limit")
        );
        let budget = Budget::new();
        budget.items(crate::budget::MAX_ITEMS - 3).unwrap();
        assert!(intersection(&[a(&[n(1.0), n(2.0)]), a(&[n(2.0), n(3.0)])], &budget).is_err());
        let budget = Budget::new();
        budget.text(crate::budget::MAX_TEXT - 3).unwrap();
        assert!(convert(&Function::String, a(&[n(1.0)]), &budget).is_err());
        assert_eq!(
            convert(&Function::String, s("long text"), &budget).unwrap(),
            s("long text")
        );
    }

    #[test]
    fn constructors_follow_icinga() {
        let budget = Budget::new();
        let cases = [
            (Primitive::String, vec![], s("")),
            (Primitive::String, vec![n(3.0)], s("3")),
            (Primitive::String, vec![a(&[s("a")])], s(r#"[ "a" ]"#)),
            (Primitive::Number, vec![], n(0.0)),
            (Primitive::Number, vec![s("5")], n(5.0)),
            (Primitive::Number, vec![Value::Bool(true)], n(1.0)),
            (Primitive::Boolean, vec![], n(0.0)),
            (Primitive::Boolean, vec![n(1.0)], Value::Bool(true)),
            (Primitive::Boolean, vec![s("")], Value::Bool(false)),
        ];
        for (primitive, args, expected) in cases {
            assert_eq!(
                construct(primitive, &args, &budget).unwrap(),
                expected,
                "{primitive:?}({args:?})"
            );
        }
        assert_eq!(
            construct(Primitive::Number, &[n(1.0), n(2.0)], &budget).unwrap_err(),
            "Number() takes at most 1 argument (2 given)"
        );
        assert!(construct(Primitive::Number, &[s("x")], &budget).is_err());
    }

    #[test]
    fn types_are_dictionaries_with_a_name() {
        let number = type_value("Number");
        assert_eq!(
            number.as_dict().and_then(|entries| entries.get("name")),
            Some(&s("Number"))
        );
        assert_eq!(
            convert(&Function::TypeOf, n(1.0), &Budget::new()).unwrap(),
            number
        );
        assert_eq!(
            convert(&Function::TypeOf, Value::Null, &Budget::new()).unwrap(),
            type_value("Object")
        );
    }
}
