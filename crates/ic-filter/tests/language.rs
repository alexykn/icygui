//! Table-driven tests of the filter language: literals, every operator,
//! precedence, short-circuiting, Icinga's comparison rules, functions,
//! methods, member access and errors.

use std::collections::BTreeMap;

use ic_filter::{Chain, Filter, Value, VarsScope};
use ic_model::Timestamp;
use serde_json::{Value as Json, json};

fn vars() -> BTreeMap<String, Value> {
    let json = json!({
        "n": 5,
        "zero": 0,
        "half": 0.5,
        "s": "abc",
        "empty": "",
        "digits": "42",
        "t": true,
        "f": false,
        "nil": null,
        "arr": [1, "two", true, null],
        "nums": [3, 1, 2],
        "strs": ["web-01", "web-02", "db-01"],
        "dict": { "a": 1, "nested": { "b": "x" }, "list": [10, 20], "len": "shadow" },
        "ips": ["10.0.0.1", "192.168.1.5"],
        "output": "OK - fine\nline two\nline three",
        "pattern": "web-*",
    });
    Value::from_json(&json)
        .as_dict()
        .cloned()
        .unwrap_or_default()
}

fn eval(source: &str) -> Result<Value, String> {
    let filter = Filter::parse(source).map_err(|error| format!("parse error: {error}"))?;
    let vars = vars();
    filter
        .evaluate(&VarsScope { vars: &vars })
        .map_err(|error| error.message)
}

/// Evaluates each case and compares with the expected JSON value.
fn check(cases: &[(&str, Json)]) {
    let mut failures = Vec::new();
    for (source, expected) in cases {
        let expected = Value::from_json(expected);
        match eval(source) {
            Ok(value) if value == expected => {}
            other => failures.push(format!(
                "{source}\n    expected {expected:?}\n    got      {other:?}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Evaluates each case and checks that it fails with a message containing
/// the given text.
fn check_errors(cases: &[(&str, &str)]) {
    let mut failures = Vec::new();
    for (source, message) in cases {
        match eval(source) {
            Err(error) if error.contains(message) => {}
            other => failures.push(format!(
                "{source}\n    expected error containing {message:?}\n    got {other:?}"
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn literals() {
    check(&[
        ("42", json!(42)),
        ("27.3", json!(27.3)),
        ("500ms", json!(0.5)),
        ("30s", json!(30)),
        ("5m", json!(300)),
        ("2.5m", json!(150)),
        ("1h", json!(3600)),
        ("1.5h", json!(5400)),
        ("2d", json!(172_800)),
        (r#""hello""#, json!("hello")),
        (
            r#""tab\tnew\nquote\"back\\""#,
            json!("tab\tnew\nquote\"back\\"),
        ),
        (r#""\r\b\f""#, json!("\r\u{8}\u{c}")),
        (r#""\101\102""#, json!("AB")),
        (r#""caf\303\251""#, json!("café")),
        ("\"ünïcödé ✓\"", json!("ünïcödé ✓")),
        (
            "{{{multi\nline \"raw\" \\n}}}",
            json!("multi\nline \"raw\" \\n"),
        ),
        ("{{{}}}", json!("")),
        ("true", json!(true)),
        ("false", json!(false)),
        ("null", json!(null)),
        ("[]", json!([])),
        ("[1, \"a\", [true], null]", json!([1, "a", [true], null])),
        ("[1, 2,]", json!([1, 2])),
        ("[\n  1,\n  2\n]", json!([1, 2])),
        ("{}", json!({})),
        ("{ a = 1, \"b c\" = [2] }", json!({ "a": 1, "b c": [2] })),
        ("{ a = 1; a = 2 }", json!({ "a": 2 })),
        ("{\n  x = n\n  y = s\n}", json!({ "x": 5, "y": "abc" })),
        ("{ @default = 1 }", json!({ "default": 1 })),
    ]);
}

#[test]
fn arithmetic() {
    check(&[
        ("1 + 2", json!(3)),
        ("n + 1", json!(6)),
        ("nil + 3", json!(3)),
        ("3 + nil", json!(3)),
        (r#""test" + 3"#, json!("test3")),
        ("s + n", json!("abc5")),
        ("n + s", json!("5abc")),
        ("half + \"\"", json!("0.500000")),
        ("s + nil", json!("abc")),
        ("empty + nil", json!("")),
        ("[1] + [2]", json!([1, 2])),
        ("arr + nil", json!([1, "two", true, null])),
        ("{ a = 1 } + { b = 2 }", json!({ "a": 1, "b": 2 })),
        ("{ a = 1 } + { a = 2 }", json!({ "a": 2 })),
        ("3 - 1", json!(2)),
        ("nil - 5", json!(-5)),
        ("nums - [1]", json!([3, 2])),
        ("nil - [1]", json!([])),
        ("5m * 10", json!(3000)),
        ("\"\" * 3", json!(0)),
        ("nil * 3", json!(0)),
        ("5m / 5", json!(60)),
        ("7 / 2", json!(3.5)),
        ("nil / 2", json!(0)),
        ("17 % 12", json!(5)),
        ("-7 % 3", json!(-1)),
        ("7.9 % 2", json!(1)),
        ("digits % 5", json!(2)),
        ("t % 2", json!(1)),
        ("-n", json!(-5)),
        ("- -n", json!(5)),
        ("-nil", json!(0)),
        ("+s", json!("abc")),
    ]);
}

#[test]
fn precedence_and_associativity() {
    // Expressions from Icinga's own operator tests and the precedence table.
    check(&[
        ("2 + 3 * 4", json!(14)),
        ("(2 + 3) * 4", json!(20)),
        ("2 * - 3", json!(-6)),
        ("-(2 + 3)", json!(-5)),
        ("- 2 * 2 - 2 * 3 - 4 * - 5", json!(10)),
        ("10 - 2 - 3", json!(5)),
        ("2 * 3 % 4", json!(2)),
        ("100 / 10 / 5", json!(2)),
        ("2 << 3 << 4", json!(256)),
        ("256 >> 4 >> 3", json!(2)),
        ("1 << 2 + 1", json!(8)),
        ("1 + 1 < 3", json!(true)),
        ("1 < 2 == true", json!(true)),
        ("7 in [7] == true", json!(true)),
        ("7 !in [7] == false", json!(true)),
        ("3 < 5 in [true]", json!(true)),
        ("(7 | 8) == 15", json!(true)),
        ("(7 ^ 8) == 15", json!(true)),
        ("(7 & 15) == 7", json!(true)),
        ("(7 | 8) > 14", json!(true)),
        ("6 & 3 ^ 1", json!(3)),
        ("6 ^ 3 | 1", json!(5)),
        ("1 | 2 && 0", json!(0)),
        ("0 && 1 || 2", json!(2)),
        ("1 || 0 && 0", json!(1)),
        ("!0 == true", json!(true)),
        ("!false && false", json!(false)),
        ("!(false && false)", json!(true)),
        ("-dict.a", json!(-1)),
        ("!dict.missing", json!(true)),
        ("1 + 0 ? 2 : 3 + 4", json!(2)),
        ("0 + 0 ? 2 : 3 + 4", json!(7)),
        ("1 ? 2 : 3 ? 4 : 5 ? 6 : 7", json!(2)),
        ("0 ? 2 : 3 ? 4 : 5 ? 6 : 7", json!(4)),
        ("0 ? 2 : 0 ? 4 : 5 ? 6 : 7", json!(6)),
        ("0 ? 2 : 0 ? 4 : 0 ? 6 : 7", json!(7)),
        ("1 ? 0 ? 3 : 4 : 5", json!(4)),
        ("n > 3 ? \"big\" : \"small\"", json!("big")),
    ]);
    // `&` binds looser than `==`, so this compares first and then fails.
    check_errors(&[(
        "7 & 15 == 7",
        "operator & cannot be applied to values of type 'Number' and 'Boolean'",
    )]);
}

#[test]
fn comparisons_follow_icinga() {
    check(&[
        ("3 < 5", json!(true)),
        ("3 > 5", json!(false)),
        ("3 <= 3", json!(true)),
        ("3 >= 4", json!(false)),
        ("\"a\" < \"b\"", json!(true)),
        ("\"B\" < \"a\"", json!(true)),
        ("\"abc\" < \"abd\"", json!(true)),
        ("\"\" < \"a\"", json!(true)),
        ("\"b\" >= \"b\"", json!(true)),
        ("nil < 1", json!(true)),
        ("empty < 1", json!(true)),
        ("-1 < nil", json!(true)),
        ("[1] < [1, 2]", json!(true)),
        ("[2] > [1, 9]", json!(true)),
        ("[1, 2] < [1, 2]", json!(false)),
    ]);
    check_errors(&[
        (
            "t < 1",
            "operator < cannot be applied to values of type 'Boolean' and 'Number'",
        ),
        ("s > 1", "operator > cannot be applied"),
        ("nil < nil", "type 'Empty' and 'Empty'"),
        ("s < nil", "type 'String' and 'Empty'"),
        ("[1] <= [1]", "operator <= cannot be applied"),
        ("[\"a\"] < [\"a\", \"b\"]", "operator <"),
        ("{} >= {}", "operator >="),
    ]);
}

#[test]
fn equality_follows_icinga() {
    check(&[
        ("1 == 1", json!(true)),
        ("1 == 2", json!(false)),
        ("1 == true", json!(true)),
        ("0 == false", json!(true)),
        ("2 == true", json!(false)),
        ("true == true", json!(true)),
        ("\"a\" == \"a\"", json!(true)),
        ("\"a\" == \"A\"", json!(false)),
        ("\"1\" == 1", json!(false)),
        ("\"true\" == true", json!(false)),
        ("nil == null", json!(true)),
        ("nil == \"\"", json!(true)),
        ("empty == null", json!(true)),
        ("nil == 0", json!(false)),
        ("nil == false", json!(false)),
        ("empty == 0", json!(false)),
        ("undefined == null", json!(true)),
        ("dict.missing == \"\"", json!(true)),
        ("[1, \"a\"] == [1, \"a\"]", json!(true)),
        ("[1] == [true]", json!(true)),
        ("[1] == [1, 2]", json!(false)),
        ("[] == null", json!(false)),
        ("{ a = 1 } == { a = 1 }", json!(true)),
        ("{ a = 1 } != { a = 2 }", json!(true)),
        ("1 != 2", json!(true)),
        ("s != \"abc\"", json!(false)),
    ]);
}

#[test]
fn logical_operators_return_operands_and_short_circuit() {
    check(&[
        ("true && false", json!(false)),
        ("true || false", json!(true)),
        ("3 && 7", json!(7)),
        ("0 && 7", json!(0)),
        ("0 || 7", json!(7)),
        ("\"\" || \"default\"", json!("default")),
        ("s || \"default\"", json!("abc")),
        ("nil && missing_function()", json!(null)),
        ("nil || nil", json!(null)),
        ("1 && 2 && 3", json!(3)),
        ("1 && 0 && 3", json!(0)),
        ("0 || \"\" || [] || \"x\"", json!("x")),
        ("false && 1 / 0", json!(false)),
        ("true || 1 / 0", json!(true)),
        ("false && missing_function()", json!(false)),
        ("true || s.nope()", json!(true)),
        ("!true", json!(false)),
        ("!0", json!(true)),
        ("!\"\"", json!(true)),
        ("!\"x\"", json!(false)),
        ("![]", json!(true)),
        ("!{}", json!(true)),
        ("![0]", json!(false)),
        ("!nil", json!(true)),
        ("!!s", json!(true)),
    ]);
    check_errors(&[(
        "true && 1 / 0",
        "right-hand side argument for operator / is 0",
    )]);
}

#[test]
fn bitwise_operators() {
    check(&[
        ("7 & 3", json!(3)),
        ("2 | 3", json!(3)),
        ("17 ^ 12", json!(29)),
        ("~0", json!(-1)),
        ("~5", json!(-6)),
        ("~t", json!(-2)),
        ("~digits", json!(-43)),
        ("4 << 8", json!(1024)),
        ("1024 >> 4", json!(64)),
        ("-16 >> 2", json!(-4)),
        ("nil | 4", json!(4)),
        ("7.9 & 3", json!(3)),
    ]);
    check_errors(&[
        ("t & 1", "operator & cannot be applied"),
        ("nil ^ nil", "operator ^ cannot be applied"),
        ("s | 1", "operator | cannot be applied"),
        ("1 << s", "operator << cannot be applied"),
        ("~s", "can't convert 'abc' to a floating point number"),
    ]);
}

#[test]
fn membership() {
    check(&[
        ("\"foo\" in [\"foo\", \"bar\"]", json!(true)),
        ("\"foo\" in [\"bar\", \"baz\"]", json!(false)),
        ("\"foo\" !in [\"bar\", \"baz\"]", json!(true)),
        ("\"foo\" !in [\"foo\", \"bar\"]", json!(false)),
        ("\"foo\" in null", json!(false)),
        ("\"foo\" !in null", json!(true)),
        ("\"foo\" in empty", json!(false)),
        ("\"foo\" in undefined", json!(false)),
        ("\"foo\" !in dict.missing", json!(true)),
        ("1 in [true]", json!(true)),
        ("null in [\"\"]", json!(true)),
        ("\"two\" in arr", json!(true)),
        ("\"x\" !in arr", json!(true)),
        ("\"db-01\" in strs", json!(true)),
        ("[1] in [[1], [2]]", json!(true)),
        ("missing_function() in nil", json!(false)),
    ]);
    check_errors(&[
        (
            "\"a\" in \"b\"",
            "invalid right side argument for 'in' operator: \"b\"",
        ),
        (
            "\"a\" !in 5",
            "invalid right side argument for '!in' operator: 5",
        ),
        (
            "\"a\" in dict",
            "invalid right side argument for 'in' operator: {",
        ),
    ]);
}

#[test]
fn member_access() {
    check(&[
        ("dict.a", json!(1)),
        ("dict[\"a\"]", json!(1)),
        ("dict.nested.b", json!("x")),
        ("dict.nested[\"b\"]", json!("x")),
        ("dict.list[1]", json!(20)),
        ("dict.list[\"0\"]", json!(10)),
        ("dict.list[zero]", json!(10)),
        ("strs[1 + 1]", json!("db-01")),
        ("dict.len", json!("shadow")),
        ("dict.@len", json!("shadow")),
        ("[2, 3][1]", json!(3)),
        ("{ a = 3 }.a", json!(3)),
        ("{ abc = 7 }[s]", json!(7)),
        ("(dict).a", json!(1)),
        ("[dict][0].nested.b", json!("x")),
        // Missing keys, indexes out of range and members of null are null.
        ("dict.missing", json!(null)),
        ("dict.missing.deeper.still", json!(null)),
        ("dict[s]", json!(null)),
        ("dict.list[5]", json!(null)),
        ("dict.list[-1]", json!(null)),
        ("nil.x", json!(null)),
        ("nil[0]", json!(null)),
        ("undefined", json!(null)),
        ("undefined.x.y", json!(null)),
        ("undefined[0][\"a\"]", json!(null)),
    ]);
    check_errors(&[
        (
            "s.foo",
            "invalid field access (for value of type 'String'): 'foo'",
        ),
        (
            "s[0]",
            "invalid field access (for value of type 'String'): '0'",
        ),
        (
            "n.x",
            "invalid field access (for value of type 'Number'): 'x'",
        ),
        (
            "t.x",
            "invalid field access (for value of type 'Boolean'): 'x'",
        ),
        (
            "strs.x",
            "invalid field access (for value of type 'Array'): 'x'",
        ),
        (
            "strs[1.5]",
            "invalid field access (for value of type 'Array'): '1.500000'",
        ),
        (
            "dict.a.b",
            "invalid field access (for value of type 'Number'): 'b'",
        ),
        ("s.len", "'len' is a method; call it as len()"),
    ]);
}

#[test]
fn pattern_functions() {
    check(&[
        ("match(\"web-*\", \"web-01\")", json!(true)),
        ("match(\"WEB-*\", \"web-01\")", json!(true)),
        ("match(\"web-?\", \"web-1\")", json!(true)),
        ("match(\"web-?\", \"web-10\")", json!(false)),
        ("match(\"*prod-sfo*\", \"db-prod-sfo-657\")", json!(true)),
        ("match(\"*-dev-*\", \"db-prod-sfo-657\")", json!(false)),
        ("match(\"\\\\*\", \"*\")", json!(true)),
        ("match(\"\\\\*\", \"a\")", json!(false)),
        ("match(\"web-*\", strs)", json!(false)),
        ("match(\"web-*\", strs, MatchAll)", json!(false)),
        ("match(\"web-*\", strs, MatchAny)", json!(true)),
        ("match(\"*-0?\", strs, MatchAll)", json!(true)),
        ("match(\"x\", [], MatchAny)", json!(false)),
        ("match(\"*\", [])", json!(false)),
        ("match(\"x*\", strs, 2)", json!(false)),
        ("match(pattern, \"web-07\")", json!(true)),
        ("match(pattern, \"db-07\")", json!(false)),
        ("match(\"1*\", 12)", json!(true)),
        ("match(\"\", nil)", json!(true)),
        ("match(\"*\", nil)", json!(true)),
        ("match(\"*ü*\", \"Grüße\")", json!(true)),
        ("regex(\"^Linux\", \"Linux/Unix\")", json!(true)),
        ("regex(\"^Linux$\", \"Linux/Unix\")", json!(false)),
        (
            "regex(\"^db-prod\\\\d+\", [\"db-prod1\", \"db-prod2\", \"db-dev\"], MatchAny)",
            json!(true),
        ),
        (
            "regex(\"^db-prod\\\\d+\", [\"db-prod1\", \"db-prod2\", \"db-dev\"], MatchAll)",
            json!(false),
        ),
        ("regex(\"^line two$\", output)", json!(true)),
        ("regex(\"(?i)^ok\", output)", json!(true)),
        ("regex(\"fine.line\", output)", json!(true)),
        ("regex(\"^$\", nil)", json!(true)),
        ("regex(s, \"xabcx\")", json!(true)),
        (
            "cidr_match(\"192.168.56.0/24\", \"192.168.56.101\")",
            json!(true),
        ),
        (
            "cidr_match(\"192.168.56.0/26\", \"192.168.56.101\")",
            json!(false),
        ),
        ("cidr_match(\"10.0.0.0/8\", ips)", json!(false)),
        ("cidr_match(\"10.0.0.0/8\", ips, MatchAny)", json!(true)),
        ("cidr_match(\"10.0.0.0/8\", \"not-an-ip\")", json!(false)),
        ("cidr_match(\"fd00::/8\", \"fd00::3\")", json!(true)),
    ]);
    check_errors(&[
        (
            "match(\"x\")",
            "match() needs a pattern and a value (1 argument given)",
        ),
        (
            "regex()",
            "regex() needs a pattern and a value (0 arguments given)",
        ),
        (
            "match(\"x\", dict)",
            "dictionaries are not supported by match()",
        ),
        (
            "cidr_match(\"::/0\", dict)",
            "dictionaries are not supported by cidr_match()",
        ),
        ("regex(pattern + \"(\", s)", "invalid regular expression"),
        ("regex(\"a\" + \"(?=b)\", s)", "look-around"),
        ("cidr_match(s, \"10.0.0.1\")", "invalid IP address 'abc'"),
        (
            "cidr_match(\"10.0.0.1/\" + s, \"10.0.0.1\")",
            "can't convert 'abc' to an integer",
        ),
        (
            "match(\"x\", s, \"bad\")",
            "can't convert 'bad' to a floating point number",
        ),
    ]);
}

#[test]
fn other_functions() {
    check(&[
        ("len(\"abc\")", json!(3)),
        ("len(\"äb\")", json!(3)),
        ("len(arr)", json!(4)),
        ("len(dict)", json!(4)),
        ("len(nil)", json!(0)),
        ("len(5)", json!(0)),
        ("typeof(3)", json!({ "name": "Number" })),
        ("typeof(3).name", json!("Number")),
        ("typeof(s).name == \"String\"", json!(true)),
        ("typeof(3) == \"Number\"", json!(false)),
        ("typeof(3) != String", json!(true)),
        ("typeof(dict) in [Array, Dictionary]", json!(true)),
        ("typeof(3) == Number", json!(true)),
        ("typeof(\"x\") == String", json!(true)),
        ("typeof(true) == Boolean", json!(true)),
        ("typeof([1]) == Array", json!(true)),
        ("typeof({}) == Dictionary", json!(true)),
        ("typeof(nil) == Object", json!(true)),
        ("string(5)", json!("5")),
        ("string(2.5)", json!("2.500000")),
        ("string(true)", json!("true")),
        ("string(nil)", json!("")),
        ("string([1, \"a\"])", json!("[ 1.000000, \"a\" ]")),
        ("number(\"78\")", json!(78)),
        ("number(\"1e3\")", json!(1000)),
        ("number(false)", json!(0)),
        ("number(nil)", json!(0)),
        ("bool(1)", json!(true)),
        ("bool(0)", json!(false)),
        ("bool(\"\")", json!(false)),
        ("bool([0])", json!(true)),
        ("keys(dict)", json!(["a", "len", "list", "nested"])),
        ("keys(nil)", json!([])),
        (
            "union([\"devs\", \"slack\"], [\"slack\", \"noc\"])",
            json!(["devs", "noc", "slack"]),
        ),
        ("union(nums, [2, 5])", json!([1, 2, 3, 5])),
        (
            "intersection([\"devs\", \"slack\"], [\"slack\", \"noc\"])",
            json!(["slack"]),
        ),
        ("intersection(nums, [9, 3, 1])", json!([1, 3])),
        ("range(5)", json!([0, 1, 2, 3, 4])),
        ("range(2, 4)", json!([2, 3])),
        ("range(2, 10, 2)", json!([2, 4, 6, 8])),
        ("len(range(3)) == 3", json!(true)),
        ("len(\"abc\", )", json!(3)),
        ("String(5)", json!("5")),
        ("String(arr)", json!("[ 1.000000, \"two\", true, null ]")),
        ("String()", json!("")),
        ("Number(\"5\")", json!(5)),
        ("Number(t)", json!(1)),
        ("Number()", json!(0)),
        ("Boolean(1)", json!(true)),
        ("Boolean(empty)", json!(false)),
        ("Boolean()", json!(0)),
    ]);
    check_errors(&[
        ("nope()", "unknown function 'nope()'"),
        (
            "get_host(\"x\")",
            "the Icinga function 'get_host()' is not available in filters",
        ),
        (
            "Number(1, 2)",
            "Number() takes at most 1 argument (2 given)",
        ),
        (
            "Number(s)",
            "can't convert 'abc' to a floating point number",
        ),
        ("len()", "len() takes exactly 1 argument (0 given)"),
        ("len(1, 2)", "len() takes exactly 1 argument (2 given)"),
        ("typeof()", "typeof() takes exactly 1 argument"),
        (
            "number(\"abc\")",
            "can't convert 'abc' to a floating point number",
        ),
        (
            "keys(s)",
            "keys() expects a dictionary, got a value of type 'String'",
        ),
        ("union(s)", "union() expects arrays"),
        ("intersection(nums, dict)", "intersection() expects arrays"),
        (
            "range(1, 2, 3, 4)",
            "range() takes 1 to 3 arguments (4 given)",
        ),
        ("range(10000000)", "range() would produce more than"),
        ("len(1 / 0)", "right-hand side argument for operator / is 0"),
    ]);
}

#[test]
fn literal_patterns_are_checked_while_parsing() {
    for (source, offset, message) in [
        ("regex(\"(\", s)", 6, "invalid regular expression"),
        ("x && regex(\"a(?=b)\", s)", 11, "look-around"),
        (
            "cidr_match(\"10.0.0.1/8\", ip)",
            11,
            "masked-off bits must all be zero",
        ),
        ("cidr_match(\"10.0.0.0/33\", ip)", 11, "between 0 and 32"),
        ("cidr_match(\"host\", ip)", 11, "invalid IP address 'host'"),
    ] {
        let error = Filter::parse(source).unwrap_err();
        assert_eq!(error.offset, offset, "{source}: {error:?}");
        assert!(error.message.contains(message), "{source}: {error:?}");
    }
}

#[test]
fn methods() {
    check(&[
        ("s.len()", json!(3)),
        ("\"Linux/Unix\".len()", json!(10)),
        ("s.upper()", json!("ABC")),
        ("\"MiXeD\".lower()", json!("mixed")),
        ("\"  pad \\n\".trim()", json!("pad")),
        ("s.contains(\"bc\")", json!(true)),
        ("s.contains(\"x\")", json!(false)),
        ("s.find(\"c\")", json!(2)),
        ("s.find(\"z\")", json!(-1)),
        ("\"abcabc\".find(\"b\", 2)", json!(4)),
        ("\"a,b,,c\".split(\",\")", json!(["a", "b", "", "c"])),
        ("\"hello\".substr(1, 3)", json!("ell")),
        ("\"hello\".substr(1)", json!("ello")),
        ("\"pg_main\".starts_with(\"pg_\")", json!(true)),
        ("\"x.example.com\".ends_with(\".com\")", json!(true)),
        ("\"a-b-c\".replace(\"-\", \"+\")", json!("a+b+c")),
        ("s.to_string()", json!("abc")),
        ("strs.len()", json!(3)),
        ("strs.contains(\"db-01\")", json!(true)),
        ("strs.join(\", \")", json!("web-01, web-02, db-01")),
        ("[].join(\",\")", json!(null)),
        (
            "strs.to_string()",
            json!("[ \"web-01\", \"web-02\", \"db-01\" ]"),
        ),
        ("dict.contains(\"a\")", json!(true)),
        ("dict.contains(\"zzz\")", json!(false)),
        ("dict.get(\"nested\").b", json!("x")),
        ("dict.get(\"zzz\")", json!(null)),
        ("{ a = 1, b = 2 }.keys()", json!(["a", "b"])),
        ("{ a = 1, b = 2 }.values()", json!([1, 2])),
        ("{ a = 1 }.len()", json!(1)),
        (
            "{ \"/\" = {}, \"/var\" = {} }.to_string()",
            json!("{\n\t\"/\" = {\n\t}\n\t\"/var\" = {\n\t}\n}"),
        ),
        ("5.to_string()", json!("5")),
        ("half.to_string()", json!("0.500000")),
        ("false.to_string()", json!("false")),
        ("output.split(\"\\n\")[1]", json!("line two")),
        ("s.upper().lower()", json!("abc")),
        ("strs[0].starts_with(\"web\")", json!(true)),
        ("s[\"len\"]()", json!(3)),
        ("(s).len()", json!(3)),
        // Methods on null (missing variables and keys) treat it as empty.
        ("nil.len()", json!(0)),
        ("undefined.contains(\"x\")", json!(false)),
        ("!undefined.contains(\"x\")", json!(true)),
        ("dict.missing.contains(\"x\") || s == \"abc\"", json!(true)),
        ("undefined.find(\"x\") >= 0", json!(false)),
        ("undefined.lower() == \"\"", json!(true)),
        ("undefined.split(\",\")", json!([])),
        ("undefined.keys()", json!([])),
        ("undefined.get(\"a\")", json!(null)),
        ("undefined.join(\",\")", json!(null)),
    ]);
    check_errors(&[
        ("nil.nope()", "unknown method 'nope' (called on null"),
        (
            "nil.contains()",
            "contains() takes exactly 1 argument (0 given)",
        ),
        (
            "dict.len()",
            "'len' is a key of the dictionary, not a method",
        ),
        ("s.nope()", "unknown method 'nope' for type 'String'"),
        ("strs.lower()", "unknown method 'lower' for type 'Array'"),
        ("n.len()", "unknown method 'len' for type 'Number'"),
        (
            "s.contains()",
            "contains() takes exactly 1 argument (0 given)",
        ),
        (
            "strs.join(nil, nil)",
            "join() takes exactly 1 argument (2 given)",
        ),
        ("s.substr(5)", "string index is out of range"),
        ("s.find(\"a\", -1)", "string index is out of range"),
        ("[nil].join(\",\")", "operator + cannot be applied"),
        // The method is looked up before the arguments are evaluated.
        ("s.nope(1 / 0)", "unknown method 'nope'"),
    ]);
}

#[test]
fn builtin_constants() {
    check(&[
        ("MatchAll", json!(0)),
        ("MatchAny", json!(1)),
        ("ServiceOK", json!(0)),
        ("ServiceWarning", json!(1)),
        ("ServiceCritical", json!(2)),
        ("ServiceUnknown", json!(3)),
        ("HostUp", json!(0)),
        ("HostDown", json!(1)),
        ("Dictionary", json!({ "name": "Dictionary" })),
        ("Dictionary.name", json!("Dictionary")),
    ]);
}

#[test]
fn errors_name_the_failing_part() {
    let error = eval("n > 1 && (true\n  + 1)").unwrap_err();
    assert_eq!(
        error,
        "operator + cannot be applied to values of type 'Boolean' and 'Number' (in `true + 1`)"
    );
    let error = eval("s.substr(10)").unwrap_err();
    assert_eq!(error, "string index is out of range (in `s.substr(10)`)");
}

#[test]
fn get_time_uses_the_given_instant() {
    let filter = Filter::parse("get_time()").unwrap();
    let scope = Chain { scopes: &[] };
    let now = Timestamp::from_unix_seconds(1_700_000_000.5);
    assert_eq!(
        filter.evaluate_at(&scope, now).unwrap(),
        Value::Number(1_700_000_000.5)
    );
    let filter = Filter::parse("get_time() - 1h > 1600000000").unwrap();
    assert!(filter.matches_at(&scope, now));
    // Without a fixed instant it reads the clock.
    let Value::Number(clock) = Filter::parse("get_time()")
        .unwrap()
        .evaluate(&scope)
        .unwrap()
    else {
        panic!("get_time() returns a number");
    };
    assert!(clock > 1_600_000_000.0);
    assert_eq!(
        Filter::parse("get_time(1, 2)")
            .unwrap()
            .evaluate_at(&scope, now)
            .unwrap(),
        Value::Number(1_700_000_000.5),
        "extra arguments are ignored, as in Icinga"
    );
}

#[test]
fn matches_uses_truthiness_and_treats_errors_as_no_match() {
    let vars = vars();
    let scope = VarsScope { vars: &vars };
    for (source, expected) in [
        ("n", true),
        ("zero", false),
        ("s", true),
        ("empty", false),
        ("arr", true),
        ("undefined", false),
        ("dict.list", true),
        ("s.len() > 2 && \"two\" in arr", true),
        ("true + 1", false),
        ("nope()", false),
    ] {
        assert_eq!(
            Filter::parse(source).unwrap().matches(&scope),
            expected,
            "{source}"
        );
    }
}

#[test]
fn parse_errors_have_byte_offsets() {
    for (source, offset, message) in [
        (
            "host.name = \"x\"",
            10,
            "assignments are not supported in filters",
        ),
        (
            "var x = 1",
            0,
            "only expressions are supported in filters ('var' starts a statement)",
        ),
        ("if (x) { 1 }", 0, "'if' starts a statement"),
        ("for (x in y) { }", 0, "'for' starts a statement"),
        ("while (true) { }", 0, "'while' starts a statement"),
        ("function f() { }", 0, "functions are not supported"),
        ("x => x * 2", 2, "functions (lambdas) are not supported"),
        ("return 1", 0, "'return' starts a statement"),
        ("a == 1; b == 2", 8, "only a single expression is supported"),
        ("a == 1\nb == 2", 7, "only a single expression is supported"),
        (
            "host.name ==",
            12,
            "unexpected end of filter, expected an expression",
        ),
        ("(a", 2, "expected ')'"),
        ("a == b == c", 7, "can't be chained"),
        ("x == \"unterminated", 5, "unterminated string"),
        ("x == \"bad \\q\"", 10, "bad escape sequence '\\q'"),
        ("x == {{{never closed", 5, "unterminated multi-line string"),
        ("x /* comment", 2, "unterminated comment"),
        ("!inactive", 0, "'!in' is the 'not in' operator"),
        ("a<b>c", 1, "include path"),
        (
            "host.vars.default == 1",
            10,
            "'default' is a reserved keyword",
        ),
        ("a == ä", 5, "unexpected character 'ä'"),
        ("host.name == 'x'", 13, "strings use double quotes"),
        ("a ==\nb", 4, "wrap a multi-line filter in parentheses"),
        ("a == b\n&& c", 7, "wrap a multi-line filter in parentheses"),
    ] {
        let error = Filter::parse(source).unwrap_err();
        assert_eq!(error.offset, offset, "{source}: {error:?}");
        assert!(error.message.contains(message), "{source}: {error:?}");
    }
}

#[test]
fn earliest_error_wins() {
    // A syntax error comes before a later lexical error, and vice versa.
    let error = Filter::parse("a == == \"unterminated").unwrap_err();
    assert_eq!(error.offset, 5);
    let error = Filter::parse("\"unterminated == == a").unwrap_err();
    assert_eq!(error.offset, 0);
}

#[test]
fn multi_line_filters_need_parentheses() {
    let filter = Filter::parse("(\n  host.vars.role == \"postgres\" &&\n  service.state != 0\n)\n");
    assert!(filter.is_ok(), "{filter:?}");
    let filter = Filter::parse("x in [\n  \"a\",\n  \"b\"\n] # trailing comment\n");
    assert!(filter.is_ok(), "{filter:?}");
}
