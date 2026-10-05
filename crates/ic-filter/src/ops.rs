//! Icinga's operators on values, following `lib/base/value-operators.cpp`.
//!
//! The rules are Icinga's, quirks included: `null` and `""` both count as
//! "empty" and act as `0` in arithmetic, `1 == true`, `"1" != 1`, `+`
//! concatenates as soon as one side is a string, and the bitwise operators
//! work on 32-bit integers. Errors are plain messages; the evaluator adds
//! where in the filter they happened.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::sync::Arc;

use crate::budget::Budget;
use crate::value::{Value, format_number, preview_str};

/// Result of an operator: a value or an error message.
pub(crate) type OpResult<T> = Result<T, String>;

fn type_error(op: &str, lhs: &Value, rhs: &Value) -> String {
    format!(
        "operator {op} cannot be applied to values of type '{}' and '{}'",
        lhs.type_name(),
        rhs.type_name()
    )
}

/// `null` or a number (but not `""`): the operands `+` and `-` add up.
fn is_null_or_number(value: &Value) -> bool {
    matches!(value, Value::Null | Value::Number(_))
}

/// A number or an empty value (`null` or `""`).
fn is_empty_or_number(value: &Value) -> bool {
    matches!(value, Value::Number(_)) || value.is_empty_value()
}

/// The number for a value known to be a number, boolean or empty.
fn numeric(value: &Value) -> f64 {
    match value {
        Value::Number(number) => *number,
        Value::Bool(true) => 1.0,
        _ => 0.0,
    }
}

/// Icinga's conversion to a number (`Value::operator double`): booleans are
/// 0 or 1, empty values 0, strings are parsed strictly.
pub(crate) fn to_number(value: &Value) -> OpResult<f64> {
    match value {
        Value::Number(number) => Ok(*number),
        Value::Bool(flag) => Ok(if *flag { 1.0 } else { 0.0 }),
        Value::Null => Ok(0.0),
        Value::String(text) if text.is_empty() => Ok(0.0),
        Value::String(text) => text.parse::<f64>().map_err(|_| {
            format!(
                "can't convert '{}' to a floating point number",
                preview_str(text)
            )
        }),
        Value::Array(_) | Value::Dict(_) => Err(format!(
            "can't convert '{}' to a floating point number",
            value.icinga_preview()
        )),
    }
}

/// C++ `static_cast<int>(double)` as compiled for x86-64: truncation, with
/// NaN and out-of-range values becoming `i32::MIN`.
pub(crate) fn to_i32(number: f64) -> i32 {
    if number > -2_147_483_649.0 && number < 2_147_483_648.0 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "range checked above; truncation toward zero is the intent"
        )]
        let truncated = number as i32;
        truncated
    } else {
        i32::MIN
    }
}

/// C++ `static_cast<long>(double)` as compiled for x86-64 (64-bit `long`).
pub(crate) fn to_i64(number: f64) -> i64 {
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&number) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "range checked above; truncation toward zero is the intent"
        )]
        let truncated = number as i64;
        truncated
    } else {
        i64::MIN
    }
}

/// `boost::lexical_cast<long>`: an optional sign and decimal digits, nothing
/// else (no spaces, no fraction). Used for array indexes and CIDR prefixes.
pub(crate) fn parse_integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.strip_prefix('+').unwrap_or(text).parse().ok()
}

/// `==`.
#[expect(clippy::float_cmp, reason = "Icinga compares numbers exactly")]
pub(crate) fn equals(lhs: &Value, rhs: &Value) -> bool {
    match (lhs, rhs) {
        (Value::Number(a), Value::Number(b)) => return a == b,
        (Value::Number(_) | Value::Bool(_), Value::Number(_) | Value::Bool(_)) => {
            return numeric(lhs) == numeric(rhs);
        }
        (Value::String(a), Value::String(b)) => return a == b,
        _ => {}
    }
    let (lhs_empty, rhs_empty) = (lhs.is_empty_value(), rhs.is_empty_value());
    if lhs_empty || rhs_empty {
        // A non-empty string never equals an empty value; two empty values
        // (`null`, `""`) are equal.
        return lhs_empty && rhs_empty;
    }
    match (lhs, rhs) {
        (Value::Array(a), Value::Array(b)) => {
            Arc::ptr_eq(a, b)
                || (a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| equals(x, y)))
        }
        // Icinga compares dictionaries by identity; values here are
        // immutable, so comparing contents is the faithful equivalent of
        // "the same dictionary" and the useful one for literals.
        (Value::Dict(a), Value::Dict(b)) => {
            Arc::ptr_eq(a, b)
                || (a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((ka, va), (kb, vb))| ka == kb && equals(va, vb)))
        }
        _ => false,
    }
}

/// `x in array`: membership by `==`.
pub(crate) fn contains(items: &[Value], value: &Value) -> bool {
    items.iter().any(|item| equals(item, value))
}

/// `null`, a number or a string: the operands `+` concatenates as text
/// (when one of them is a string).
pub(crate) fn is_text_operand(value: &Value) -> bool {
    matches!(value, Value::Null | Value::Number(_) | Value::String(_))
}

/// The text of `null`, a number or a string.
pub(crate) fn operand_text(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        Value::Number(number) => Cow::Owned(format_number(*number)),
        _ => Cow::Borrowed(""),
    }
}

/// `+`: numbers add, strings concatenate (with numbers and `null` converted),
/// arrays concatenate, dictionaries merge (the right side wins). What it
/// creates is charged to `budget`.
pub(crate) fn add(lhs: &Value, rhs: &Value, budget: &Budget) -> OpResult<Value> {
    let both_null = lhs.is_null() && rhs.is_null();
    if is_null_or_number(lhs) && is_null_or_number(rhs) && !both_null {
        return Ok(Value::Number(numeric(lhs) + numeric(rhs)));
    }
    if is_text_operand(lhs)
        && is_text_operand(rhs)
        && (!both_null || matches!(lhs, Value::String(_)) || matches!(rhs, Value::String(_)))
    {
        let (left, right) = (operand_text(lhs), operand_text(rhs));
        let length = left.len().saturating_add(right.len());
        budget.text(length)?;
        let mut text = String::with_capacity(length);
        text.push_str(&left);
        text.push_str(&right);
        return Ok(Value::from(text));
    }
    if !(lhs.is_empty_value() && rhs.is_empty_value()) {
        if let (Some(left), Some(right)) = (array_or_empty(lhs), array_or_empty(rhs)) {
            budget.items(left.len().saturating_add(right.len()))?;
            let mut items = Vec::with_capacity(left.len() + right.len());
            items.extend_from_slice(left);
            items.extend_from_slice(right);
            return Ok(Value::array(items));
        }
        if let (Some(left), Some(right)) = (dict_or_empty(lhs), dict_or_empty(rhs)) {
            budget.items(left.len().saturating_add(right.len()))?;
            let mut entries: BTreeMap<String, Value> = left.clone();
            for (key, value) in right {
                entries.insert(key.clone(), value.clone());
            }
            return Ok(Value::Dict(Arc::new(entries)));
        }
    }
    Err(type_error("+", lhs, rhs))
}

static EMPTY_ITEMS: [Value; 0] = [];

/// The items of an array, or no items for an empty value (`null`, `""`).
fn array_or_empty(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Array(items) => Some(items),
        other if other.is_empty_value() => Some(&EMPTY_ITEMS),
        _ => None,
    }
}

/// The entries of a dictionary, or no entries for an empty value.
fn dict_or_empty(value: &Value) -> Option<&BTreeMap<String, Value>> {
    static EMPTY: BTreeMap<String, Value> = BTreeMap::new();
    match value {
        Value::Dict(entries) => Some(entries),
        other if other.is_empty_value() => Some(&EMPTY),
        _ => None,
    }
}

/// `-`: numbers subtract; for arrays, the items of the left side that are not
/// in the right side. What it creates is charged to `budget`.
pub(crate) fn subtract(lhs: &Value, rhs: &Value, budget: &Budget) -> OpResult<Value> {
    if is_null_or_number(lhs) && is_null_or_number(rhs) && !(lhs.is_null() && rhs.is_null()) {
        return Ok(Value::Number(numeric(lhs) - numeric(rhs)));
    }
    if !(lhs.is_empty_value() && rhs.is_empty_value())
        && let (Some(left), Some(right)) = (array_or_empty(lhs), array_or_empty(rhs))
    {
        // Icinga dereferences a null right side here and crashes; an empty
        // right side removes nothing.
        budget.items(left.len())?;
        return Ok(Value::array(difference(left, right)));
    }
    Err(type_error("-", lhs, rhs))
}

/// The items of `left` that are not `==` to any item of `right`. Large right
/// sides are hashed, so this takes linear rather than quadratic time.
fn difference(left: &[Value], right: &[Value]) -> Vec<Value> {
    const SCAN_UP_TO: usize = 16;
    if right.len() <= SCAN_UP_TO {
        return left
            .iter()
            .filter(|item| !contains(right, item))
            .cloned()
            .collect();
    }
    let hasher = RandomState::new();
    let hash = |value: &Value| {
        let mut state = hasher.build_hasher();
        hash_for_equality(value, &mut state);
        state.finish()
    };
    let mut buckets: HashMap<u64, Vec<&Value>> = HashMap::with_capacity(right.len());
    for item in right {
        buckets.entry(hash(item)).or_default().push(item);
    }
    left.iter()
        .filter(|item| {
            !buckets
                .get(&hash(item))
                .is_some_and(|candidates| candidates.iter().any(|other| equals(other, item)))
        })
        .cloned()
        .collect()
}

/// Feeds `value` to `state` so that values that are `==` hash alike:
/// numbers and booleans by their numeric value, `null` like `""`, strings,
/// arrays and dictionaries by their contents.
fn hash_for_equality(value: &Value, state: &mut impl Hasher) {
    match value {
        Value::Null => state.write_u8(0),
        Value::String(text) if text.is_empty() => state.write_u8(0),
        Value::String(text) => {
            state.write_u8(1);
            state.write(text.as_bytes());
            state.write_u8(0xff);
        }
        Value::Number(_) | Value::Bool(_) => {
            state.write_u8(2);
            // `+ 0.0` turns -0 into 0, which compares equal to it.
            state.write_u64((numeric(value) + 0.0).to_bits());
        }
        Value::Array(items) => {
            state.write_u8(3);
            state.write_usize(items.len());
            for item in items.iter() {
                hash_for_equality(item, state);
            }
        }
        Value::Dict(entries) => {
            state.write_u8(4);
            state.write_usize(entries.len());
            for (key, item) in entries.iter() {
                state.write(key.as_bytes());
                state.write_u8(0xff);
                hash_for_equality(item, state);
            }
        }
    }
}

/// `*`: numbers (empty values count as 0).
pub(crate) fn multiply(lhs: &Value, rhs: &Value) -> OpResult<Value> {
    if is_empty_or_number(lhs)
        && is_empty_or_number(rhs)
        && !(lhs.is_empty_value() && rhs.is_empty_value())
    {
        return Ok(Value::Number(numeric(lhs) * numeric(rhs)));
    }
    Err(type_error("*", lhs, rhs))
}

/// `/`: numbers; dividing by zero or by an empty value is an error.
pub(crate) fn divide(lhs: &Value, rhs: &Value) -> OpResult<Value> {
    if rhs.is_empty_value() {
        return Err("right-hand side argument for operator / is Empty".to_owned());
    }
    if is_empty_or_number(lhs)
        && let Value::Number(divisor) = rhs
    {
        if *divisor == 0.0 {
            return Err("right-hand side argument for operator / is 0".to_owned());
        }
        return Ok(Value::Number(numeric(lhs) / divisor));
    }
    Err(type_error("/", lhs, rhs))
}

/// `%`: remainder of the operands truncated to 32-bit integers. The left side
/// is converted like `number()`.
pub(crate) fn modulo(lhs: &Value, rhs: &Value) -> OpResult<Value> {
    if rhs.is_empty_value() {
        return Err("right-hand side argument for operator % is Empty".to_owned());
    }
    let Value::Number(divisor) = rhs else {
        return Err(type_error("%", lhs, rhs));
    };
    let divisor = to_i32(*divisor);
    if divisor == 0 {
        // Also covers divisors like 0.5, which Icinga truncates to 0 and then
        // divides by (undefined behaviour in C++).
        return Err("right-hand side argument for operator % is 0".to_owned());
    }
    let dividend = to_i32(to_number(lhs)?);
    Ok(Value::Number(f64::from(dividend.wrapping_rem(divisor))))
}

fn integer_operands(op: &str, lhs: &Value, rhs: &Value) -> OpResult<(i32, i32)> {
    if is_empty_or_number(lhs)
        && is_empty_or_number(rhs)
        && !(lhs.is_empty_value() && rhs.is_empty_value())
    {
        Ok((to_i32(numeric(lhs)), to_i32(numeric(rhs))))
    } else {
        Err(type_error(op, lhs, rhs))
    }
}

/// `&` on 32-bit integers.
pub(crate) fn bit_and(lhs: &Value, rhs: &Value) -> OpResult<Value> {
    let (a, b) = integer_operands("&", lhs, rhs)?;
    Ok(Value::Number(f64::from(a & b)))
}

/// `|` on 32-bit integers.
pub(crate) fn bit_or(lhs: &Value, rhs: &Value) -> OpResult<Value> {
    let (a, b) = integer_operands("|", lhs, rhs)?;
    Ok(Value::Number(f64::from(a | b)))
}

/// `^` on 32-bit integers.
pub(crate) fn bit_xor(lhs: &Value, rhs: &Value) -> OpResult<Value> {
    let (a, b) = integer_operands("^", lhs, rhs)?;
    Ok(Value::Number(f64::from(a ^ b)))
}

/// `<<` on 32-bit integers; the shift count wraps at 32 like x86's.
pub(crate) fn shift_left(lhs: &Value, rhs: &Value) -> OpResult<Value> {
    let (a, b) = integer_operands("<<", lhs, rhs)?;
    Ok(Value::Number(f64::from(a.wrapping_shl(b.cast_unsigned()))))
}

/// `>>` (arithmetic) on 32-bit integers.
pub(crate) fn shift_right(lhs: &Value, rhs: &Value) -> OpResult<Value> {
    let (a, b) = integer_operands(">>", lhs, rhs)?;
    Ok(Value::Number(f64::from(a.wrapping_shr(b.cast_unsigned()))))
}

/// Unary `-`, which Icinga implements as `0 - x`.
pub(crate) fn negate(value: &Value, budget: &Budget) -> OpResult<Value> {
    subtract(&Value::Number(0.0), value, budget)
}

/// `~`: bitwise negation of the operand converted to a 64-bit integer.
pub(crate) fn bit_not(value: &Value) -> OpResult<Value> {
    let number = to_i64(to_number(value)?);
    #[expect(
        clippy::cast_precision_loss,
        reason = "Icinga stores the result in a double as well"
    )]
    let result = (!number) as f64;
    Ok(Value::Number(result))
}

/// The ordering operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Comparison {
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
}

impl Comparison {
    fn symbol(self) -> &'static str {
        match self {
            Comparison::Less => "<",
            Comparison::Greater => ">",
            Comparison::LessEqual => "<=",
            Comparison::GreaterEqual => ">=",
        }
    }
}

/// `<`, `>`, `<=`, `>=`: strings compare byte-wise, numbers (with empty
/// values as 0) numerically; `<` and `>` also compare arrays element-wise.
pub(crate) fn compare(op: Comparison, lhs: &Value, rhs: &Value) -> OpResult<bool> {
    if let (Value::String(a), Value::String(b)) = (lhs, rhs) {
        return Ok(match op {
            Comparison::Less => a < b,
            Comparison::Greater => a > b,
            Comparison::LessEqual => a <= b,
            Comparison::GreaterEqual => a >= b,
        });
    }
    if is_empty_or_number(lhs)
        && is_empty_or_number(rhs)
        && !(lhs.is_empty_value() && rhs.is_empty_value())
    {
        let (a, b) = (numeric(lhs), numeric(rhs));
        return Ok(match op {
            Comparison::Less => a < b,
            Comparison::Greater => a > b,
            Comparison::LessEqual => a <= b,
            Comparison::GreaterEqual => a >= b,
        });
    }
    if let (Value::Array(a), Value::Array(b)) = (lhs, rhs)
        && matches!(op, Comparison::Less | Comparison::Greater)
    {
        return compare_arrays(op, a, b);
    }
    Err(type_error(op.symbol(), lhs, rhs))
}

/// Lexicographic `<` / `>` on arrays; a missing item counts as `null`.
fn compare_arrays(op: Comparison, lhs: &[Value], rhs: &[Value]) -> OpResult<bool> {
    let (first, second) = match op {
        Comparison::Greater => (Comparison::Greater, Comparison::Less),
        _ => (Comparison::Less, Comparison::Greater),
    };
    for index in 0..lhs.len().max(rhs.len()) {
        let left = lhs.get(index).unwrap_or(&Value::Null);
        let right = rhs.get(index).unwrap_or(&Value::Null);
        if compare(first, left, right)? {
            return Ok(true);
        }
        if compare(second, left, right)? {
            return Ok(false);
        }
    }
    Ok(false)
}

/// Icinga's `<` used for sorting (`union`, `intersection`).
pub(crate) fn less(lhs: &Value, rhs: &Value) -> OpResult<bool> {
    compare(Comparison::Less, lhs, rhs)
}

/// Stable merge sort with Icinga's fallible `<`. Unlike `slice::sort_by`
/// it can't panic when the order is inconsistent (mixed types).
pub(crate) fn sort_values(values: Vec<Value>) -> OpResult<Vec<Value>> {
    if values.len() <= 1 {
        return Ok(values);
    }
    let mut values = values;
    let right = values.split_off(values.len() / 2);
    let left = sort_values(values)?;
    let right = sort_values(right)?;
    let mut merged = Vec::with_capacity(left.len() + right.len());
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
        if less(b, a)? {
            merged.extend(right.next());
        } else {
            merged.extend(left.next());
        }
    }
    merged.extend(left);
    merged.extend(right);
    Ok(merged)
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

    fn d(entries: &[(&str, Value)]) -> Value {
        Value::from(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    const NULL: Value = Value::Null;
    const T: Value = Value::Bool(true);
    const F: Value = Value::Bool(false);

    #[test]
    fn equality_table() {
        let cases = [
            (n(1.0), n(1.0), true),
            (n(1.0), n(2.0), false),
            (n(f64::NAN), n(f64::NAN), false),
            (n(1.0), T, true),
            (n(0.0), F, true),
            (n(2.0), T, false),
            (T, T, true),
            (T, F, false),
            (s("a"), s("a"), true),
            (s("a"), s("A"), false),
            (s("1"), n(1.0), false),
            (s("true"), T, false),
            (NULL, NULL, true),
            (NULL, s(""), true),
            (s(""), NULL, true),
            (s(""), s(""), true),
            (NULL, n(0.0), false),
            (n(0.0), NULL, false),
            (NULL, F, false),
            (s(""), n(0.0), false),
            (s("a"), NULL, false),
            (NULL, a(&[]), false),
            (a(&[]), a(&[]), true),
            (a(&[n(1.0), s("x")]), a(&[n(1.0), s("x")]), true),
            (a(&[n(1.0)]), a(&[T]), true),
            (a(&[n(1.0)]), a(&[n(1.0), n(2.0)]), false),
            (d(&[("a", n(1.0))]), d(&[("a", n(1.0))]), true),
            (d(&[("a", n(1.0))]), d(&[("a", n(2.0))]), false),
            (d(&[("a", n(1.0))]), d(&[("b", n(1.0))]), false),
            (d(&[]), a(&[]), false),
        ];
        for (lhs, rhs, expected) in cases {
            assert_eq!(equals(&lhs, &rhs), expected, "{lhs:?} == {rhs:?}");
        }
    }

    #[test]
    fn addition_table() {
        let ok = [
            (n(1.0), n(2.0), n(3.0)),
            (NULL, n(3.0), n(3.0)),
            (n(3.0), NULL, n(3.0)),
            (s("test"), n(3.0), s("test3")),
            (n(3.0), s("x"), s("3x")),
            (s("a"), NULL, s("a")),
            (NULL, s("a"), s("a")),
            (s(""), NULL, s("")),
            (s(""), n(2.5), s("2.500000")),
            (s("a"), s("b"), s("ab")),
            (a(&[n(1.0)]), a(&[n(2.0)]), a(&[n(1.0), n(2.0)])),
            (a(&[n(1.0)]), NULL, a(&[n(1.0)])),
            (NULL, a(&[n(1.0)]), a(&[n(1.0)])),
            (s(""), a(&[n(1.0)]), a(&[n(1.0)])),
            (
                d(&[("a", n(1.0)), ("b", n(1.0))]),
                d(&[("b", n(2.0))]),
                d(&[("a", n(1.0)), ("b", n(2.0))]),
            ),
            (NULL, d(&[("a", n(1.0))]), d(&[("a", n(1.0))])),
        ];
        for (lhs, rhs, expected) in ok {
            assert_eq!(
                add(&lhs, &rhs, &Budget::new()).unwrap(),
                expected,
                "{lhs:?} + {rhs:?}"
            );
        }
        for (lhs, rhs) in [
            (NULL, NULL),
            (T, n(1.0)),
            (s("a"), T),
            (a(&[]), s("a")),
            (a(&[]), d(&[])),
            (d(&[]), n(1.0)),
        ] {
            let error = add(&lhs, &rhs, &Budget::new()).unwrap_err();
            assert!(error.starts_with("operator + cannot be applied"), "{error}");
        }
        assert_eq!(
            add(&T, &n(1.0), &Budget::new()).unwrap_err(),
            "operator + cannot be applied to values of type 'Boolean' and 'Number'"
        );
    }

    #[test]
    fn subtraction_and_arithmetic() {
        assert_eq!(subtract(&n(3.0), &n(1.0), &Budget::new()).unwrap(), n(2.0));
        assert_eq!(subtract(&NULL, &n(5.0), &Budget::new()).unwrap(), n(-5.0));
        assert_eq!(subtract(&n(5.0), &NULL, &Budget::new()).unwrap(), n(5.0));
        assert!(subtract(&NULL, &NULL, &Budget::new()).is_err());
        assert!(subtract(&s("5"), &n(1.0), &Budget::new()).is_err());
        assert!(
            subtract(&n(5.0), &s(""), &Budget::new()).is_err(),
            "\"\" is a string, not null, for -"
        );
        assert_eq!(
            subtract(
                &a(&[n(1.0), n(2.0), n(3.0), n(2.0)]),
                &a(&[n(2.0)]),
                &Budget::new()
            )
            .unwrap(),
            a(&[n(1.0), n(3.0)])
        );
        assert_eq!(
            subtract(&a(&[n(1.0)]), &NULL, &Budget::new()).unwrap(),
            a(&[n(1.0)])
        );
        assert_eq!(
            subtract(&NULL, &a(&[n(1.0)]), &Budget::new()).unwrap(),
            a(&[])
        );
        assert_eq!(negate(&n(2.0), &Budget::new()).unwrap(), n(-2.0));
        assert_eq!(negate(&NULL, &Budget::new()).unwrap(), n(0.0));
        assert!(negate(&s("1"), &Budget::new()).is_err());
        assert!(negate(&T, &Budget::new()).is_err());

        assert_eq!(multiply(&n(300.0), &n(10.0)).unwrap(), n(3000.0));
        assert_eq!(multiply(&s(""), &n(10.0)).unwrap(), n(0.0));
        assert_eq!(multiply(&NULL, &n(10.0)).unwrap(), n(0.0));
        assert!(multiply(&NULL, &s("")).is_err());
        assert!(multiply(&T, &n(1.0)).is_err());
        assert!(multiply(&s("2"), &n(1.0)).is_err());

        assert_eq!(divide(&n(300.0), &n(5.0)).unwrap(), n(60.0));
        assert_eq!(divide(&NULL, &n(5.0)).unwrap(), n(0.0));
        assert_eq!(
            divide(&n(1.0), &n(0.0)).unwrap_err(),
            "right-hand side argument for operator / is 0"
        );
        assert_eq!(
            divide(&n(1.0), &NULL).unwrap_err(),
            "right-hand side argument for operator / is Empty"
        );
        assert!(divide(&n(1.0), &T).is_err());
        assert!(divide(&T, &n(1.0)).is_err());

        assert_eq!(modulo(&n(17.0), &n(12.0)).unwrap(), n(5.0));
        assert_eq!(modulo(&n(-7.0), &n(3.0)).unwrap(), n(-1.0));
        assert_eq!(modulo(&n(7.5), &n(2.0)).unwrap(), n(1.0));
        assert_eq!(
            modulo(&s("7"), &n(4.0)).unwrap(),
            n(3.0),
            "left side is converted"
        );
        assert_eq!(modulo(&T, &n(4.0)).unwrap(), n(1.0));
        assert_eq!(modulo(&NULL, &n(4.0)).unwrap(), n(0.0));
        assert!(modulo(&s("x"), &n(4.0)).is_err());
        assert!(modulo(&n(1.0), &n(0.0)).is_err());
        assert!(modulo(&n(1.0), &n(0.5)).is_err(), "truncates to 0");
        assert!(modulo(&n(1.0), &NULL).is_err());
        assert!(modulo(&n(1.0), &s("2")).is_err());
        assert_eq!(
            modulo(&n(-2_147_483_648.0), &n(-1.0)).unwrap(),
            n(0.0),
            "no overflow trap"
        );
    }

    #[test]
    fn bitwise_and_shifts() {
        assert_eq!(bit_and(&n(7.0), &n(3.0)).unwrap(), n(3.0));
        assert_eq!(bit_or(&n(2.0), &n(3.0)).unwrap(), n(3.0));
        assert_eq!(bit_xor(&n(17.0), &n(12.0)).unwrap(), n(29.0));
        assert_eq!(bit_or(&NULL, &n(3.0)).unwrap(), n(3.0));
        assert_eq!(bit_and(&n(7.9), &n(3.0)).unwrap(), n(3.0), "truncates");
        assert!(bit_and(&T, &n(1.0)).is_err());
        assert!(bit_or(&NULL, &NULL).is_err());
        assert!(bit_xor(&s("1"), &n(1.0)).is_err());
        assert_eq!(shift_left(&n(4.0), &n(8.0)).unwrap(), n(1024.0));
        assert_eq!(shift_right(&n(1024.0), &n(4.0)).unwrap(), n(64.0));
        assert_eq!(shift_right(&n(-16.0), &n(2.0)).unwrap(), n(-4.0));
        assert_eq!(
            shift_left(&n(1.0), &n(32.0)).unwrap(),
            n(1.0),
            "count wraps"
        );
        assert_eq!(shift_left(&n(1.0), &n(31.0)).unwrap(), n(-2_147_483_648.0));
        assert_eq!(bit_not(&n(0.0)).unwrap(), n(-1.0));
        assert_eq!(bit_not(&T).unwrap(), n(-2.0));
        assert_eq!(bit_not(&NULL).unwrap(), n(-1.0));
        assert_eq!(bit_not(&s("12")).unwrap(), n(-13.0));
        assert!(bit_not(&s("x")).is_err());
        assert!(bit_not(&a(&[])).is_err());
    }

    #[test]
    fn integer_conversion_matches_x86() {
        assert_eq!(to_i32(1.9), 1);
        assert_eq!(to_i32(-1.9), -1);
        assert_eq!(to_i32(2_147_483_647.5), i32::MAX);
        assert_eq!(to_i32(2_147_483_648.0), i32::MIN);
        assert_eq!(to_i32(-2_147_483_648.9), i32::MIN);
        assert_eq!(to_i32(-2_147_483_649.0), i32::MIN);
        assert_eq!(to_i32(f64::NAN), i32::MIN);
        assert_eq!(to_i64(1e300), i64::MIN);
        assert_eq!(to_i64(-3.5), -3);
        assert_eq!(to_i64(f64::NAN), i64::MIN);
    }

    #[test]
    fn comparison_table() {
        use Comparison::{Greater, GreaterEqual, Less, LessEqual};
        let ok = [
            (Less, n(3.0), n(5.0), true),
            (Greater, n(3.0), n(5.0), false),
            (LessEqual, n(3.0), n(3.0), true),
            (GreaterEqual, n(3.0), n(3.0), true),
            (Less, s("a"), s("b"), true),
            (Less, s("B"), s("a"), true),
            (Less, s(""), s("a"), true),
            (Less, NULL, n(1.0), true),
            (Less, s(""), n(1.0), true),
            (Greater, n(-1.0), NULL, false),
            (Less, a(&[n(1.0)]), a(&[n(1.0), n(2.0)]), true),
            (Greater, a(&[n(2.0)]), a(&[n(1.0), n(9.0)]), true),
            (Less, a(&[n(1.0), n(2.0)]), a(&[n(1.0), n(2.0)]), false),
        ];
        for (op, lhs, rhs, expected) in ok {
            assert_eq!(
                compare(op, &lhs, &rhs).unwrap(),
                expected,
                "{lhs:?} {op:?} {rhs:?}"
            );
        }
        for (op, lhs, rhs) in [
            (Less, NULL, NULL),
            (Less, s("a"), n(1.0)),
            (Less, T, n(1.0)),
            (Less, s("a"), NULL),
            (LessEqual, a(&[]), a(&[])),
            (Less, a(&[s("a")]), a(&[s("a"), s("b")])),
            (Less, d(&[]), d(&[])),
        ] {
            assert!(compare(op, &lhs, &rhs).is_err(), "{lhs:?} {op:?} {rhs:?}");
        }
    }

    #[test]
    fn sorting_is_fallible_and_stable() {
        let sorted = sort_values(vec![n(3.0), n(1.0), n(2.0), n(1.0)]).unwrap();
        assert_eq!(sorted, vec![n(1.0), n(1.0), n(2.0), n(3.0)]);
        let sorted = sort_values(vec![s("b"), s("a"), s("")]).unwrap();
        assert_eq!(sorted, vec![s(""), s("a"), s("b")]);
        assert!(sort_values(vec![n(1.0), s("a")]).is_err());
        assert!(sort_values(vec![NULL, NULL]).is_err());
        assert_eq!(sort_values(vec![]).unwrap(), vec![]);
    }

    #[test]
    fn large_differences_use_icinga_equality() {
        // More than 16 items on the right side: the hashed path.
        let right: Vec<Value> = (0..40)
            .map(|index| match index % 5 {
                0 => n(f64::from(index)),
                1 => s(&format!("s{index}")),
                2 => a(&[n(f64::from(index)), s("x")]),
                3 => d(&[("k", n(f64::from(index)))]),
                _ => NULL,
            })
            .collect();
        let left = vec![
            n(0.0),
            n(-0.0),
            T,
            n(1.0),
            n(5.0),
            F,
            s("s1"),
            s("s2"),
            s(""),
            NULL,
            a(&[n(2.0), s("x")]),
            a(&[T, s("x")]),
            a(&[n(2.0)]),
            d(&[("k", n(3.0))]),
            d(&[("k", n(4.0))]),
        ];
        let budget = Budget::new();
        let hashed = subtract(&a(&left), &a(&right), &budget).unwrap();
        let scanned: Vec<Value> = left
            .iter()
            .filter(|item| !contains(&right, item))
            .cloned()
            .collect();
        assert_eq!(hashed, a(&scanned));
        // 0 and -0 and false, the empty string and null are equal.
        assert_eq!(
            hashed,
            a(&[
                T,
                n(1.0),
                s("s2"),
                a(&[T, s("x")]),
                a(&[n(2.0)]),
                d(&[("k", n(4.0))]),
            ])
        );
        // NaN equals nothing, so it is never removed.
        let nan = subtract(&a(&[n(f64::NAN)]), &a(&right), &budget).unwrap();
        assert!(matches!(nan.as_array(), Some([Value::Number(x)]) if x.is_nan()));
        // The same array object is equal to itself even with NaN inside.
        let with_nan = a(&[n(f64::NAN)]);
        let mut right_with_nan = right.clone();
        right_with_nan.push(with_nan.clone());
        assert_eq!(
            subtract(&a(&[with_nan]), &a(&right_with_nan), &budget).unwrap(),
            a(&[])
        );
    }

    #[test]
    fn large_differences_take_linear_time() {
        let items: Vec<Value> = (0..50_000).map(|index| n(f64::from(index))).collect();
        let start = std::time::Instant::now();
        let rest = subtract(&a(&items), &a(&items[1..]), &Budget::new()).unwrap();
        assert_eq!(rest, a(&[n(0.0)]));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn operators_charge_what_they_create() {
        let budget = Budget::new();
        budget.text(crate::budget::MAX_TEXT - 4).unwrap();
        assert_eq!(add(&s("ab"), &n(1.0), &budget).unwrap(), s("ab1"));
        let error = add(&s("ab"), &s("cd"), &budget).unwrap_err();
        assert!(error.starts_with("evaluation limit reached"), "{error}");
        let budget = Budget::new();
        budget.items(crate::budget::MAX_ITEMS - 3).unwrap();
        assert!(add(&a(&[n(1.0)]), &a(&[n(2.0), n(3.0)]), &budget).is_ok());
        assert!(add(&a(&[n(1.0)]), &a(&[]), &budget).is_err());
        let budget = Budget::new();
        budget.items(crate::budget::MAX_ITEMS - 1).unwrap();
        assert!(subtract(&a(&[n(1.0), n(2.0)]), &NULL, &budget).is_err());
        assert!(add(&d(&[("a", n(1.0))]), &d(&[("b", n(1.0))]), &budget).is_err());
    }

    #[test]
    fn number_conversion() {
        assert_eq!(to_number(&s("78")), Ok(78.0));
        assert_eq!(to_number(&s("-1.5e3")), Ok(-1500.0));
        assert_eq!(to_number(&s("")), Ok(0.0));
        assert_eq!(to_number(&F), Ok(0.0));
        assert_eq!(to_number(&T), Ok(1.0));
        assert_eq!(to_number(&NULL), Ok(0.0));
        assert!(to_number(&s(" 1")).is_err());
        assert!(to_number(&s("1x")).is_err());
        assert_eq!(
            to_number(&a(&[n(1.0)])).unwrap_err(),
            "can't convert '[ 1.000000 ]' to a floating point number"
        );
        // Messages show only the start of long values.
        let long = to_number(&s(&"9x".repeat(100_000))).unwrap_err();
        assert!(long.len() < 200, "{}", long.len());
        let big: Value = (0..100_000).map(|index| n(f64::from(index))).collect();
        let message = to_number(&big).unwrap_err();
        assert!(message.len() < 200, "{}", message.len());
        assert!(message.contains("[ 0.000000, 1.000000,"), "{message}");
    }
}
