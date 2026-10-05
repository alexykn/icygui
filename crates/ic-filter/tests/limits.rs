//! Hostile and extreme input: deeply nested filters are rejected cleanly,
//! and the deepest accepted ones parse and evaluate on a small stack.

use std::collections::BTreeMap;

use ic_filter::{Filter, Value, VarsScope};

/// Thread stacks are usually 2 MiB (std, tokio's blocking pool). The deepest
/// accepted filter must fit in half of that even in unoptimised builds
/// (about 450 KiB were measured there; release builds need a third).
const SMALL_STACK: usize = 1024 * 1024;

/// A name and a function building a filter nested `n` levels deep.
type Generator = (&'static str, fn(usize) -> String);

fn generators() -> Vec<Generator> {
    vec![
        ("parentheses", |n| {
            format!("{}x{}", "(".repeat(n), ")".repeat(n))
        }),
        ("negations", |n| format!("{}x", "!".repeat(n))),
        ("minus signs", |n| format!("{}x", "-".repeat(n))),
        ("sums", |n| vec!["x"; n].join(" + ")),
        ("arrays", |n| format!("{}x{}", "[".repeat(n), "]".repeat(n))),
        ("calls", |n| {
            format!("{}x{}", "len(".repeat(n), ")".repeat(n))
        }),
        ("methods", |n| format!("s{}", ".lower()".repeat(n))),
        ("indexes", |n| {
            format!("{}0{}", "list[".repeat(n), "]".repeat(n))
        }),
        ("conditionals", |n| format!("{}x", "x ? x : ".repeat(n))),
        ("dictionaries", |n| {
            format!("{}x{}", "{ a = ".repeat(n), " }".repeat(n))
        }),
        ("mixed", |n| {
            let mut source = String::from("x");
            for level in 0..n {
                source = match level % 4 {
                    0 => format!("!({source})"),
                    1 => format!("[{source}, 1]"),
                    2 => format!("({source} || x) && y"),
                    _ => format!("len({source}) + 1"),
                };
            }
            source
        }),
    ]
}

/// The largest nesting that still parses.
fn deepest_accepted(generate: fn(usize) -> String) -> usize {
    let (mut low, mut high) = (1, 1_000);
    assert!(Filter::parse(&generate(low)).is_ok());
    assert!(Filter::parse(&generate(high)).is_err());
    while high - low > 1 {
        let middle = usize::midpoint(low, high);
        if Filter::parse(&generate(middle)).is_ok() {
            low = middle;
        } else {
            high = middle;
        }
    }
    low
}

#[test]
fn excessive_nesting_is_a_parse_error() {
    for (name, generate) in generators() {
        let error = Filter::parse(&generate(100_000)).unwrap_err();
        assert!(
            error.message.contains("nested too deeply"),
            "{name}: {error:?}"
        );
    }
}

#[test]
fn deepest_accepted_filters_evaluate_on_a_small_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(|| {
            let vars = BTreeMap::from([
                ("x".to_owned(), Value::Number(1.0)),
                ("y".to_owned(), Value::Bool(true)),
                ("s".to_owned(), Value::from("ABC")),
                ("list".to_owned(), Value::from(vec![Value::Number(0.0)])),
            ]);
            let scope = VarsScope { vars: &vars };
            for (name, generate) in generators() {
                let depth = deepest_accepted(generate);
                assert!(depth >= 40, "{name}: only {depth} levels accepted");
                let filter = Filter::parse(&generate(depth)).unwrap();
                // Errors (such as adding to an array) are fine; crashing isn't.
                let _ = filter.evaluate(&scope);
                drop(filter.clone());
            }
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn long_flat_filters_are_fine() {
    let names: Vec<String> = (0..20_000)
        .map(|index| format!("host.name == \"h{index}\""))
        .collect();
    let filter = Filter::parse(&names.join(" || ")).unwrap();
    let vars = BTreeMap::new();
    assert_eq!(
        filter.evaluate(&VarsScope { vars: &vars }).unwrap(),
        Value::Bool(false)
    );
    let list: Vec<String> = (0..20_000).map(|index| format!("\"h{index}\"")).collect();
    let filter = Filter::parse(&format!("\"h19999\" in [{}]", list.join(", "))).unwrap();
    assert_eq!(
        filter.evaluate(&VarsScope { vars: &vars }).unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn huge_and_odd_input_does_not_panic() {
    let vars = BTreeMap::new();
    let scope = VarsScope { vars: &vars };
    let inputs = [
        "\0".to_owned(),
        "\"\0\"".to_owned(),
        "\u{feff}x".to_owned(),
        "x".repeat(100_000),
        format!("\"{}\"", "a".repeat(1_000_000)),
        format!("match(\"{}\", \"a\")", "*a".repeat(5_000)),
        "9".repeat(500),
        "1 / 0 % 0 << 99999999999 >> -1".to_owned(),
        "~1e308".replace("1e308", &"9".repeat(309)),
        "range(0, 1, 0.000000001)".to_owned(),
        "\"a\".substr(0, 99999999999999999999999)".to_owned(),
        "\"a\".find(\"a\", 99999999999999999999999)".to_owned(),
        "[1, 2] < [1, \"x\"]".to_owned(),
        "union([1, \"a\", null, [1], {}])".to_owned(),
        "intersection([{}, {}], [{}])".to_owned(),
        "cidr_match(\"::/0\", [\"::1\", 5, null, []], 99999999999)".to_owned(),
    ];
    for input in inputs {
        if let Ok(filter) = Filter::parse(&input) {
            let _ = filter.evaluate(&scope);
            let _ = filter.matches(&scope);
        }
    }
}
