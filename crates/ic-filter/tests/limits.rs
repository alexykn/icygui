//! Hostile and extreme input: deeply nested filters are rejected cleanly,
//! the deepest accepted ones parse and evaluate on a small stack, filters
//! that would create huge amounts of data fail instead of exhausting memory,
//! pathological input takes linear (or n log n) time, and error messages
//! stay short.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ic_filter::{Chain, Filter, HostScope, Scope, Value, VarsScope};
use ic_model::Host;

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

fn evaluate(source: &str, vars: &BTreeMap<String, Value>) -> Result<Value, String> {
    Filter::parse(source)
        .map_err(|error| error.to_string())?
        .evaluate(&VarsScope { vars })
        .map_err(|error| error.message)
}

/// Generous for unoptimised builds on a busy machine; the operations below
/// take milliseconds when they are linear and minutes when quadratic.
const SLOW: Duration = Duration::from_secs(10);

#[test]
fn amplification_hits_the_evaluation_limit() {
    let vars = BTreeMap::new();
    let tenfold = ".replace(\"a\", \"aaaaaaaaaa\")";
    // 10^7 bytes fit into the limit…
    let seven = format!("len(\"a\"{})", tenfold.repeat(7));
    assert_eq!(evaluate(&seven, &vars), Ok(Value::Number(1e7)));
    let start = Instant::now();
    for steps in [8, 11, 30] {
        // …10^8 bytes don't, and 10^11 or 10^30 are refused just as fast,
        // before anything that large is allocated.
        let source = format!("len(\"a\"{}) > 0", tenfold.repeat(steps));
        let error = evaluate(&source, &vars).unwrap_err();
        assert!(error.starts_with("evaluation limit reached"), "{error}");
        assert!(
            !Filter::parse(&source)
                .unwrap()
                .matches(&VarsScope { vars: &vars })
        );
    }
    let doubling = format!("len(\"ab\"{})", " + \"ab\"".repeat(40));
    assert_eq!(evaluate(&doubling, &vars), Ok(Value::Number(82.0)));
    let mut growing = "\"x\"".to_owned();
    for _ in 0..40 {
        growing = format!("({growing}).replace(\"x\", \"xx\")");
    }
    assert!(
        evaluate(&format!("len({growing})"), &vars)
            .unwrap_err()
            .starts_with("evaluation limit reached")
    );
    assert!(start.elapsed() < SLOW, "{:?}", start.elapsed());
}

#[test]
fn many_ranges_hit_the_evaluation_limit() {
    let vars = BTreeMap::new();
    assert!(
        evaluate("len(range(10001))", &vars)
            .unwrap_err()
            .contains("range() would produce more than 10000 numbers")
    );
    let ten = format!("len([{}])", ["range(10000)"; 10].join(", "));
    assert_eq!(evaluate(&ten, &vars), Ok(Value::Number(10.0)));
    let start = Instant::now();
    for source in [
        format!("len([{}])", vec!["range(10000)"; 1_000].join(", ")),
        format!("len(union({}))", vec!["range(10000)"; 1_000].join(", ")),
        format!("len({})", vec!["range(10000)"; 50].join(" + ")),
    ] {
        let error = evaluate(&source, &vars).unwrap_err();
        assert!(error.starts_with("evaluation limit reached"), "{error}");
    }
    assert!(start.elapsed() < SLOW, "{:?}", start.elapsed());
}

#[test]
fn shared_values_are_not_written_out_beyond_the_limit() {
    // A value from the scope referenced many times stays shared until it is
    // turned into text, which is then cut off at the limit.
    let big = Value::from("x".repeat(1_000_000));
    let vars = BTreeMap::from([("big".to_owned(), big)]);
    let list = format!("[{}]", vec!["big"; 100].join(", "));
    assert_eq!(
        evaluate(&format!("len({list})"), &vars),
        Ok(Value::Number(100.0))
    );
    let start = Instant::now();
    for source in [
        format!("len(string({list}))"),
        format!("len({list}.join(\"\"))"),
        format!("len({list}.to_string())"),
        format!("match(\"*\", [{list}])"),
        format!("\"x\".contains({list})"),
        format!("{{}}[{list}]"),
    ] {
        let error = evaluate(&source, &vars).unwrap_err();
        assert!(
            error.starts_with("evaluation limit reached"),
            "{source}: {error}"
        );
    }
    assert!(start.elapsed() < SLOW, "{:?}", start.elapsed());
}

#[test]
fn pathological_input_takes_linear_time() {
    let vars = BTreeMap::new();
    let start = Instant::now();
    // `join` appends in place instead of copying the result for every item.
    assert_eq!(
        evaluate(
            "len(range(10000).join(\",\") + range(10000).join(\",\")) > 0",
            &vars
        ),
        Ok(Value::Bool(true))
    );
    // `union` sorts once instead of inserting into a sorted list.
    assert_eq!(
        evaluate(
            "len(union(range(10000, 0, -1), range(0, 10000))) == 10001",
            &vars
        ),
        Ok(Value::Bool(true))
    );
    // Array `-` hashes the right side.
    assert_eq!(
        evaluate("len(range(10000) - range(1, 10000))", &vars),
        Ok(Value::Number(1.0))
    );
    // `find` doesn't compare the needle at every position, and `split`
    // doesn't compare every byte with every delimiter.
    let text = format!("{}b", "a".repeat(1_000_000));
    let needle = format!("{}b", "a".repeat(10_000));
    let vars = BTreeMap::from([
        ("text".to_owned(), Value::from(text.as_str())),
        ("needle".to_owned(), Value::from(needle)),
        ("other".to_owned(), Value::from("xyz".repeat(300_000))),
    ]);
    assert_eq!(
        evaluate("text.find(needle)", &vars),
        Ok(Value::Number(990_000.0))
    );
    assert_eq!(
        evaluate("len(other.split(text))", &vars),
        Ok(Value::Number(1.0))
    );
    assert!(
        evaluate("len(text.split(text))", &vars)
            .unwrap_err()
            .starts_with("evaluation limit reached"),
        "a million parts are over the limit"
    );
    // `match()` never backtracks: one star before a long literal tail took
    // seconds per evaluation when it did (review of 025864c). The pattern
    // and text are built from literals inside the filter, as a shared
    // dashboard could.
    let tenfold = ".replace(\"a\", \"aaaaaaaaaa\")";
    let pattern = format!("\"*\" + \"a\"{} + \"b\"", tenfold.repeat(4));
    let text = format!("\"a\"{}", tenfold.repeat(5));
    for filter in [
        format!("match({pattern}, {text})"),
        format!("match({pattern} + \"*\", {text})"),
        format!("match(\"*\" + {pattern} + \"*\", {text})"),
    ] {
        assert_eq!(evaluate(&filter, &vars), Ok(Value::Bool(false)), "{filter}");
    }
    // A run of `?` longer than 64 bytes is the one case that isn't linear;
    // its cost is charged to the evaluation, which fails instead of taking
    // seconds.
    let wild_tenfold = ".replace(\"?\", \"??????????\")";
    let wild = format!("\"*\" + \"?\"{} + \"b*\"", wild_tenfold.repeat(4));
    assert!(
        evaluate(&format!("match({wild}, {text})"), &vars)
            .unwrap_err()
            .starts_with("evaluation limit reached"),
    );
    assert_eq!(
        evaluate(&format!("match(\"*{}b*\", {text})", "?".repeat(200)), &vars),
        Ok(Value::Bool(false)),
        "a modest run of `?` on a long text is within the budget",
    );
    // The lexer checks for include paths (`<…>`) in one pass.
    for source in [
        "<".repeat(200_000),
        "x<1||".repeat(40_000),
        "a<".repeat(100_000),
    ] {
        let _ = Filter::parse(&source);
    }
    assert!(start.elapsed() < SLOW, "{:?}", start.elapsed());
}

#[test]
fn error_messages_stay_short() {
    let output = "x".repeat(1_000_000);
    let vars = BTreeMap::from([
        ("output".to_owned(), Value::from(output.as_str())),
        (
            "list".to_owned(),
            (0..100_000)
                .map(|index| Value::Number(f64::from(index)))
                .collect(),
        ),
        (
            "dict".to_owned(),
            Value::from(BTreeMap::from([(
                "k".to_owned(),
                Value::from(output.as_str()),
            )])),
        ),
    ]);
    let long_expression = format!("(\"{}\" + 1) < 2 && x", "y".repeat(70_000));
    for source in [
        "\"a\" in output",
        "\"a\" !in dict",
        "number(output) > 5",
        "number(list) > 5",
        "output.substr(2000000)",
        "output[output]",
        "list[output]",
        "cidr_match(output, \"10.0.0.1\")",
        "cidr_match(\"10.0.0.0/\" + output, \"10.0.0.1\")",
        "output.nope()",
        long_expression.as_str(),
    ] {
        let start = Instant::now();
        let error = evaluate(source, &vars).unwrap_err();
        assert!(error.len() < 400, "{} bytes for {source:.60}", error.len());
        assert!(start.elapsed() < Duration::from_secs(2), "{source:.60}");
    }
    // Failing on a long expression is cheap, also when repeated per object.
    let filter = Filter::parse(&long_expression).unwrap();
    let scope = VarsScope { vars: &vars };
    let start = Instant::now();
    for _ in 0..1_000 {
        assert!(!filter.matches(&scope));
    }
    assert!(start.elapsed() < SLOW, "{:?}", start.elapsed());
    // Parse errors don't repeat huge identifiers either.
    let error = Filter::parse(&format!("a {}", "b".repeat(100_000))).unwrap_err();
    assert!(error.message.len() < 400, "{}", error.message.len());
    let error = Filter::parse(&format!("\"\\0{}\"", "1".repeat(100_000))).unwrap_err();
    assert!(error.message.len() < 400, "{}", error.message.len());
}

#[test]
fn one_filter_is_shared_between_threads() {
    let mut host = Host::new("web-01");
    host.vars = serde_json::json!({ "pattern": "web-*", "regex": "^web" })
        .as_object()
        .cloned()
        .unwrap_or_default();
    let mut other = Host::new("db-01");
    other.vars = serde_json::json!({ "pattern": "db-*", "regex": "^db" })
        .as_object()
        .cloned()
        .unwrap_or_default();
    let filter = Filter::parse(
        "match(host.vars.pattern, host.name) && regex(host.vars.regex, host.name) \
         && !match(host.vars.regex, host.name)",
    )
    .unwrap();
    let hosts = [&host, &other];
    std::thread::scope(|scope| {
        for thread in 0..4 {
            let filter = &filter;
            let clone = filter.clone();
            scope.spawn(move || {
                for round in 0..2_000 {
                    // Alternate hosts, so the dynamic patterns change between
                    // evaluations on every thread.
                    let host = hosts[(round + thread) % 2];
                    let host_scope = HostScope { host };
                    let scopes: [&dyn Scope; 1] = [&host_scope];
                    let chain = Chain { scopes: &scopes };
                    assert!(filter.matches(&chain), "shared, round {round}");
                    assert!(clone.matches(&host_scope), "cloned, round {round}");
                }
            });
        }
    });
}
