//! API filter expressions (`filter`, `filter_vars`): a thin adapter over the
//! `ic-filter` crate.
//!
//! Icinga compiles an API filter once (`ConfigCompiler::CompileText`) and
//! evaluates it per object in a sandboxed frame
//! (`FilterUtility::GetFilterTargets`, `EvaluateFilter`). Here
//! [`compile`] parses with [`Filter::parse`] and [`ApiFilter::matches`]
//! evaluates with a [`Scope`] that stands in for Icinga's frame:
//!
//! - the object under test as `obj` and as its type's variable (`host`,
//!   `service`, `comment`, …), its joined objects under their navigation
//!   names (`host`, `check_command`, `child_host`, …; `null` when there is
//!   none), `event` for event streams and `dictionary` for status entries,
//!   all provided by a [`Frame`];
//! - `filter_vars`, which those frame variables override (Icinga sets them
//!   after the variables);
//! - Icinga's global constants filters use: `ServiceOK` … `HostDown`,
//!   `MatchAll`/`MatchAny`, the state and notification type names (`OK`,
//!   `Problem`, …, strings in Icinga 2.15), the `Downtime*Children` child
//!   options, `NodeName` and `ZoneName`, and the type globals `Object`,
//!   `Boolean`, `Number`, `String`, `Array` and `Dictionary`.
//!
//! Object attributes have Icinga's names and JSON values. Where the scope
//! decides, the adapter keeps Icinga's errors even though `ic-filter`
//! itself is lenient: an undefined variable is `Tried to access undefined
//! script variable`, an attribute the type doesn't have is `Invalid field
//! access`, and a `no_user_view` attribute is the sandbox error. The scope
//! records the first such error while `ic-filter` evaluates, and
//! [`ApiFilter::matches`] reports it instead of the result, so the request
//! fails as in Icinga.
//!
//! # HTTP behaviour
//!
//! Verified against Icinga 2.15.6 (`tests/fidelity.rs` replays the
//! recorded queries; `tests/events.rs` checks the event streams):
//! - Object queries, actions and `/v1/status`: a filter that doesn't
//!   compile (syntax errors, statements, assignments) or fails for any
//!   object fails the whole request with 404 `No objects found.`, and the
//!   reason in `diagnostic_information` with `verbose`. Its text is
//!   `ic-filter`'s, not Icinga's.
//! - An empty filter (blank, or only comments) matches nothing.
//! - Event streams: `filter: ""` is no filter. A filter that doesn't
//!   compile or is blank still opens the stream (200), which then delivers
//!   nothing; events whose evaluation fails are skipped.
//! - Without the `filter-expression` permission any `filter` is 403
//!   `Missing permission: filter-expression` (checked by the callers).
//!
//! # Differences from Icinga
//!
//! `ic-filter` deliberately differs from Icinga in a few places (see its
//! crate documentation). For API requests to the mock that means:
//! - **Methods on `null` don't fail.** Icinga answers 404 (`Argument is not
//!   a callable object.`) as soon as *any* object evaluates a method on a
//!   missing value: `host.vars.tags.contains("x")` on a host without
//!   `tags`, or `service.last_check_result.output.contains("x")` with a
//!   pending service. The mock treats `null` as an empty value instead
//!   (`contains` is false, `len()` 0), so the same request answers 200 with
//!   the objects that match.
//! - **Out-of-range array indexes are `null`** (`host.groups[5]`), where
//!   Icinga fails the request.
//! - **More is allowed:** dictionary literals with entries (`{ a = 1 }`,
//!   an assignment in Icinga's sandbox), `range()`, the dictionary method
//!   `get()` and the string methods `starts_with()` and `ends_with()` work;
//!   Icinga refuses all of them in API filters.
//! - **Regular expressions** use the `regex` crate's syntax: look-around and
//!   back-references are compile errors (404) instead of matching.
//! - **Objects used as values are dictionaries** of their attributes, so
//!   `typeof(host)` is `Dictionary`, objects compare equal by content, and
//!   a dynamic index with an unknown attribute (`host[name]`) is `null`.
//!   Icinga's type globals for object types (`Host`, `CheckCommand`, …) and
//!   its other globals (`Icinga`, `PluginDir`, …) are undefined.
//! - Error messages are `ic-filter`'s.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;

use ic_filter::{Filter, ParseError, Scope};
use ic_model::Timestamp;
use serde_json::{Map, Value as Json};

pub(crate) use ic_filter::Value;

use crate::model::ObjRef;

/// A filter that doesn't compile: a syntax error, a statement or an
/// assignment (Icinga's `ConfigCompiler` and sandbox errors).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompileError {
    message: String,
    line: usize,
    /// 0-based, like Icinga's locations.
    column: usize,
}

impl CompileError {
    fn new(source: &str, error: &ParseError) -> Self {
        let (line, column) = error.line_column(source);
        Self {
            message: error.message.clone(),
            line,
            column: column.saturating_sub(1),
        }
    }
}

impl fmt::Display for CompileError {
    /// Icinga's `diagnostic_information` layout.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (line, column) = (self.line, self.column);
        write!(
            f,
            "Error: {}\nLocation: in <API query>: {line}:{column}-{line}:{column}",
            self.message
        )
    }
}

/// A filter that failed while being evaluated (Icinga's `ScriptError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EvalError(pub(crate) String);

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error: {}", self.0)
    }
}

/// The node's global constants (`NodeName`, `ZoneName`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Node {
    pub(crate) name: String,
    pub(crate) zone: String,
}

/// What a frame variable or an object field holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item<'a> {
    /// An object of the mock (a host, a joined check command, …).
    Object(ObjRef),
    /// A value as the API writes it.
    Json(Cow<'a, Json>),
}

impl Item<'_> {
    /// `null`: a joined object that doesn't exist.
    pub(crate) fn null() -> Self {
        Item::Json(Cow::Owned(Json::Null))
    }
}

/// Icinga's frame namespace: the variables a filter sees besides
/// `filter_vars` and the globals, and the fields of objects.
pub(crate) trait Frame {
    /// A variable of the frame (`obj`, `host`, a navigation name, `event`,
    /// …), or `None` if the frame doesn't have it.
    fn variable(&self, name: &str) -> Option<Item<'_>>;

    /// The field `name` of `object`.
    ///
    /// # Errors
    /// Icinga's message for a field the type doesn't have, or one hidden
    /// from API users.
    fn field(&self, object: &ObjRef, name: &str) -> Result<Item<'_>, String>;

    /// An object used as a value: a dictionary of its attributes.
    fn object_value(&self, object: &ObjRef) -> Value;

    /// The current time in Unix seconds, for `get_time()`.
    fn now(&self) -> f64;
}

/// A compiled filter with its `filter_vars`.
#[derive(Clone, Debug)]
pub(crate) struct ApiFilter {
    /// `None` matches nothing (an event filter that didn't compile).
    filter: Option<Filter>,
    vars: BTreeMap<String, Value>,
    node: Node,
}

/// Compiles a filter for the node `node` (its `NodeName` and `ZoneName`).
///
/// # Errors
/// [`CompileError`] when the source is not a single valid expression.
pub(crate) fn compile(source: &str, node: Node) -> Result<ApiFilter, CompileError> {
    let filter = Filter::parse(source).map_err(|error| CompileError::new(source, &error))?;
    Ok(ApiFilter {
        filter: Some(filter),
        vars: BTreeMap::new(),
        node,
    })
}

impl ApiFilter {
    /// Binds `filter_vars`.
    pub(crate) fn with_vars(mut self, filter_vars: Option<&Map<String, Json>>) -> Self {
        self.vars = filter_vars
            .into_iter()
            .flatten()
            .map(|(key, value)| (key.clone(), Value::from_json(value)))
            .collect();
        self
    }

    /// A filter that matches nothing.
    pub(crate) fn nothing() -> Self {
        Self {
            filter: None,
            vars: BTreeMap::new(),
            node: Node::default(),
        }
    }

    /// Evaluates the filter for one object (or event, or status entry) and
    /// takes the result's truthiness. An empty filter matches nothing.
    ///
    /// # Errors
    /// The first Icinga error the frame reported (undefined variable,
    /// invalid or hidden field), else `ic-filter`'s evaluation error.
    pub(crate) fn matches(&self, frame: &dyn Frame) -> Result<bool, EvalError> {
        let Some(filter) = self.filter.as_ref().filter(|filter| !filter.is_empty()) else {
            return Ok(false);
        };
        let scope = FrameScope {
            frame,
            vars: &self.vars,
            node: &self.node,
            error: RefCell::new(None),
        };
        let result = filter.evaluate_at(&scope, Timestamp::from_unix_seconds(frame.now()));
        if let Some(message) = scope.error.into_inner() {
            return Err(EvalError(message));
        }
        result
            .map(|value| value.is_truthy())
            .map_err(|error| EvalError(error.message))
    }
}

/// The globals `ic-filter` resolves itself; anything else unknown is an
/// undefined variable.
const IC_FILTER_GLOBALS: [&str; 14] = [
    "MatchAll",
    "MatchAny",
    "ServiceOK",
    "ServiceWarning",
    "ServiceCritical",
    "ServiceUnknown",
    "HostUp",
    "HostDown",
    "Object",
    "Boolean",
    "Number",
    "String",
    "Array",
    "Dictionary",
];

/// Icinga 2.15's global string constants that filters use: state and
/// notification type filter names, and downtime child options. Each holds
/// its own name.
const NAME_CONSTANTS: [&str; 18] = [
    "OK",
    "Warning",
    "Critical",
    "Unknown",
    "Up",
    "Down",
    "DowntimeStart",
    "DowntimeEnd",
    "DowntimeRemoved",
    "Custom",
    "Acknowledgement",
    "Problem",
    "Recovery",
    "FlappingStart",
    "FlappingEnd",
    "DowntimeNoChildren",
    "DowntimeTriggeredChildren",
    "DowntimeNonTriggeredChildren",
];

/// The `ic-filter` scope over a frame, `filter_vars` and the globals.
struct FrameScope<'a> {
    frame: &'a dyn Frame,
    vars: &'a BTreeMap<String, Value>,
    node: &'a Node,
    /// The first Icinga error met while resolving.
    error: RefCell<Option<String>>,
}

impl<'a> FrameScope<'a> {
    /// Records an error (the first one wins: Icinga stops there). The
    /// caller stands in `null` so the evaluation can finish.
    fn fail(&self, message: String) {
        self.error.borrow_mut().get_or_insert(message);
    }

    fn global(&self, name: &str) -> Option<Value> {
        match name {
            "NodeName" => Some(Value::from(self.node.name.as_str())),
            "ZoneName" => Some(Value::from(self.node.zone.as_str())),
            _ => NAME_CONSTANTS.contains(&name).then(|| Value::from(name)),
        }
    }

    /// Follows `rest` from a frame variable: object fields through the
    /// frame, values like [`follow_value`].
    fn follow(&self, mut item: Item<'a>, mut rest: &[&str]) -> Option<Value> {
        let frame: &'a dyn Frame = self.frame;
        loop {
            match item {
                Item::Object(object) => {
                    let Some((field, deeper)) = rest.split_first() else {
                        return Some(frame.object_value(&object));
                    };
                    match frame.field(&object, field) {
                        Ok(next) => {
                            item = next;
                            rest = deeper;
                        }
                        Err(message) => {
                            self.fail(message);
                            return Some(Value::Null);
                        }
                    }
                }
                Item::Json(json) => return follow_json(&json, rest),
            }
        }
    }
}

impl<'a> Scope for FrameScope<'a> {
    fn lookup(&self, path: &[&str]) -> Option<Value> {
        let (name, rest) = path.split_first()?;
        let frame: &'a dyn Frame = self.frame;
        if let Some(item) = frame.variable(name) {
            return self.follow(item, rest);
        }
        if let Some(value) = self.vars.get(*name) {
            return follow_value(value, rest);
        }
        if let Some(value) = self.global(name) {
            return follow_value(&value, rest);
        }
        if rest.is_empty() && !IC_FILTER_GLOBALS.contains(name) {
            self.fail(format!(
                "Tried to access undefined script variable '{name}'"
            ));
            return Some(Value::Null);
        }
        // A longer path: the evaluator retries with the variable alone.
        None
    }
}

/// Follows constant member names into JSON: dictionary keys (missing ones
/// and members of `null` are `null`, as in Icinga). `None` for anything
/// else, so the evaluator indexes it and reports Icinga's errors.
fn follow_json(json: &Json, rest: &[&str]) -> Option<Value> {
    let mut current = json;
    for name in rest {
        current = match current {
            Json::Object(map) => match map.get(*name) {
                Some(value) => value,
                None => return Some(Value::Null),
            },
            Json::Null => return Some(Value::Null),
            _ => return None,
        };
    }
    Some(Value::from_json(current))
}

/// [`follow_json`] for values.
fn follow_value(value: &Value, rest: &[&str]) -> Option<Value> {
    let mut current = value;
    for name in rest {
        current = match current {
            Value::Dict(entries) => match entries.get(*name) {
                Some(value) => value,
                None => return Some(Value::Null),
            },
            Value::Null => return Some(Value::Null),
            _ => return None,
        };
    }
    Some(current.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ObjKind;
    use serde_json::json;

    /// A frame with the host `web-01` and its service `web-01!http`.
    struct TestFrame {
        host: Map<String, Json>,
        service: Map<String, Json>,
    }

    fn host_ref() -> ObjRef {
        ObjRef {
            kind: ObjKind::Host,
            name: "web-01".into(),
        }
    }

    fn service_ref() -> ObjRef {
        ObjRef {
            kind: ObjKind::Service,
            name: "web-01!http".into(),
        }
    }

    impl Frame for TestFrame {
        fn variable(&self, name: &str) -> Option<Item<'_>> {
            match name {
                "host" => Some(Item::Object(host_ref())),
                "service" | "obj" => Some(Item::Object(service_ref())),
                "check_period" => Some(Item::null()),
                _ => None,
            }
        }

        fn field(&self, object: &ObjRef, name: &str) -> Result<Item<'_>, String> {
            let (attrs, type_name) = match object.kind {
                ObjKind::Host => (&self.host, "Host"),
                _ => (&self.service, "Service"),
            };
            if object.kind == ObjKind::Service && name == "host" {
                return Ok(Item::Object(host_ref()));
            }
            if name == "state_raw" {
                return Err(format!(
                    "Accessing the field '{name}' for type '{type_name}' is not allowed in sandbox mode."
                ));
            }
            attrs
                .get(name)
                .map(|value| Item::Json(Cow::Borrowed(value)))
                .ok_or_else(|| {
                    format!("Invalid field access (for value of type '{type_name}'): '{name}'")
                })
        }

        fn object_value(&self, object: &ObjRef) -> Value {
            let attrs = match object.kind {
                ObjKind::Host => &self.host,
                _ => &self.service,
            };
            Value::from_json(&Json::Object(attrs.clone()))
        }

        fn now(&self) -> f64 {
            1_000.0
        }
    }

    fn frame() -> TestFrame {
        let host = json!({
            "name": "web-01",
            "state": 1,
            "address": "",
            "vars": { "role": "web", "disks": { "/var": { "warn": 80 } }, "tags": ["a", "b"] },
            "groups": ["linux-servers", "web"],
        });
        let service = json!({
            "name": "http",
            "__name": "web-01!http",
            "state": 2,
            "vars": null,
            "last_check_result": null,
        });
        let (Json::Object(host), Json::Object(service)) = (host, service) else {
            unreachable!("objects")
        };
        TestFrame { host, service }
    }

    fn node() -> Node {
        Node {
            name: "master-01".into(),
            zone: "master".into(),
        }
    }

    fn eval_vars(source: &str, vars: &Json) -> Result<bool, EvalError> {
        compile(source, node())
            .unwrap()
            .with_vars(vars.as_object())
            .matches(&frame())
    }

    fn eval(source: &str) -> Result<bool, EvalError> {
        eval_vars(source, &Json::Null)
    }

    fn error(source: &str) -> String {
        eval(source).unwrap_err().0
    }

    #[test]
    fn objects_fields_and_joins() {
        assert!(eval(r#"host.name == "web-01""#).unwrap());
        assert!(!eval(r#"host.name == "web-02""#).unwrap());
        assert!(eval(r#"host.name == "web-01" && service.name == "http""#).unwrap());
        assert!(eval(r#"obj.__name == "web-01!http""#).unwrap());
        assert!(eval("service.state != ServiceOK && host.state == HostDown").unwrap());
        assert!(eval("service.state >= 2.0 && service.state < 3").unwrap());
        assert!(eval(r#"service.host.name == "web-01""#).unwrap());
        assert!(
            eval("check_period == null").unwrap(),
            "missing joins are null"
        );
        assert!(eval("check_period.name == null").unwrap());
        assert!(eval("service.last_check_result.output == null").unwrap());
        assert!(eval(r#"host["name"] == "web-01""#).unwrap());
        assert!(
            eval(r#"string(service.host) != "" && service.host == host"#).unwrap(),
            "objects used as values are dictionaries"
        );
    }

    #[test]
    fn vars_and_indexers() {
        assert!(eval(r#"host.vars.role == "web""#).unwrap());
        assert!(eval(r#"host.vars["role"] == "web""#).unwrap());
        assert!(eval(r#"host.vars.disks["/var"].warn == 80"#).unwrap());
        assert!(eval(r#"host.vars.tags[1] == "b""#).unwrap());
        assert!(
            eval("host.vars.tags[5] == null").unwrap(),
            "ic-filter: null"
        );
        assert!(!eval(r#"host.vars.nothing == "x""#).unwrap());
        assert!(eval("host.vars.nothing == null").unwrap());
        assert!(eval("host.vars.nothing.deeper == null").unwrap());
        assert!(eval("service.vars.x == null").unwrap(), "vars may be null");
        assert!(eval("host.address == null").unwrap(), "\"\" equals null");
        assert!(eval("!host.vars.nothing").unwrap());
        assert!(eval(r#""web" in host.groups"#).unwrap());
        assert!(!eval(r#""db" in host.vars.missing"#).unwrap(), "null rhs");
        assert!(eval(r#""db" !in host.vars.missing"#).unwrap());
    }

    #[test]
    fn filter_vars_and_globals() {
        let vars = json!({ "names": ["db-01", "web-01"], "limit": { "warn": 1 } });
        assert!(eval_vars("host.name in names", &vars).unwrap());
        assert!(eval_vars("service.state > limit.warn", &vars).unwrap());
        assert!(!eval_vars("limit.nothing", &vars).unwrap());
        let many: Vec<String> = (0..1_000).map(|i| format!("web-{i:02}")).collect();
        assert!(eval_vars("host.name in names", &json!({ "names": many })).unwrap());
        assert!(
            eval_vars(r#"host.name == "web-01""#, &json!({ "host": "shadowed" })).unwrap(),
            "frame variables override filter_vars"
        );
        assert!(
            eval_vars("MatchAny == 5", &json!({ "MatchAny": 5 })).unwrap(),
            "filter_vars override globals"
        );
        assert!(
            eval(r#"OK == "OK" && Problem == "Problem" && FlappingEnd == "FlappingEnd""#).unwrap()
        );
        assert!(eval(r#"DowntimeTriggeredChildren == "DowntimeTriggeredChildren""#).unwrap());
        assert!(eval(r#"NodeName == "master-01" && ZoneName == "master""#).unwrap());
        assert!(eval("MatchAll == 0 && ServiceUnknown == 3 && HostUp == 0").unwrap());
        for global in IC_FILTER_GLOBALS {
            assert!(eval(&format!("{global} != null")).unwrap(), "{global}");
        }
    }

    #[test]
    fn functions_and_methods() {
        assert!(eval(r#"match("web*", host.name)"#).unwrap());
        assert!(!eval(r#"match("db*", host.name)"#).unwrap());
        assert!(eval(r#"match("li*", host.groups, MatchAny)"#).unwrap());
        assert!(!eval(r#"match("li*", host.groups)"#).unwrap());
        assert!(eval(r#"regex("^w.b-[0-9]+$", host.name)"#).unwrap());
        assert!(eval(r#"cidr_match("10.0.0.0/8", "10.1.2.3")"#).unwrap());
        assert!(eval(r#"host.name.contains("eb")"#).unwrap());
        assert!(eval(r#"host.groups.contains("web")"#).unwrap());
        assert!(eval(r#"host.vars.contains("role")"#).unwrap());
        assert!(eval("len(host.groups) == 2").unwrap());
        assert!(eval(r#"host.name.upper() == "WEB-01""#).unwrap());
        assert!(eval(r"typeof(host.name) == String").unwrap());
        assert!(eval("get_time() == 1000").unwrap(), "the frame's clock");
        // Deliberate ic-filter differences: methods on null and get().
        assert!(!eval(r#"host.vars.nothing.contains("x")"#).unwrap());
        assert!(!eval(r#"service.last_check_result.output.contains("x")"#).unwrap());
        assert!(eval(r#"host.vars.get("role") == "web""#).unwrap());
    }

    #[test]
    fn precedence_and_short_circuit() {
        assert!(eval("1 + 2 * 3 == 7").unwrap());
        assert!(eval("(1 + 2) * 3 == 9").unwrap());
        assert!(eval("true || undefined_thing").unwrap(), "never evaluated");
        assert!(!eval("false && undefined_thing").unwrap());
        assert!(eval("false && nothing || true").unwrap());
        assert!(eval("5m == 300 && 1h == 3600 && 500ms == 0.5 && 2d == 172800").unwrap());
        assert!(eval(r#""a" + 1 == "a1""#).unwrap());
        assert!(eval("-1 < 0 && !false == true").unwrap());
        assert!(eval("10 - 4 - 3 == 3").unwrap(), "left associative");
        let mut terms: Vec<String> = (0..2_000)
            .map(|i| format!("host.name == \"h{i}\""))
            .collect();
        terms.push("host.name == \"web-01\"".to_owned());
        assert!(eval(&terms.join(" || ")).unwrap(), "long generated filters");
    }

    #[test]
    fn icinga_errors_from_the_scope() {
        assert_eq!(
            error("nothing == 1"),
            "Tried to access undefined script variable 'nothing'"
        );
        assert_eq!(
            error("nothing.deeper == 1"),
            "Tried to access undefined script variable 'nothing'"
        );
        assert_eq!(
            error("typeof(Host) != null"),
            "Tried to access undefined script variable 'Host'"
        );
        assert_eq!(
            error("host.bogus == 1"),
            "Invalid field access (for value of type 'Host'): 'bogus'"
        );
        assert_eq!(
            error("service.host.bogus == 1"),
            "Invalid field access (for value of type 'Host'): 'bogus'"
        );
        assert_eq!(
            error("host.state_raw == 1"),
            "Accessing the field 'state_raw' for type 'Host' is not allowed in sandbox mode."
        );
        // The first error wins, as Icinga stops there.
        assert_eq!(
            error("host.bogus == nothing"),
            "Invalid field access (for value of type 'Host'): 'bogus'"
        );
        assert!(
            error("nothing.contains(1)").contains("undefined script variable"),
            "even where ic-filter would carry on"
        );
    }

    #[test]
    fn evaluation_errors_from_ic_filter() {
        assert!(error("host.name < 1").contains("cannot be applied"));
        assert!(error(r#""x" in "y""#).contains("'in'"));
        assert!(error("host.name.x == 1").contains("invalid field access"));
        assert!(error("host.vars.tags.x == 1").contains("invalid field access"));
        assert!(error("nonexistent_function(1)").contains("nonexistent_function"));
        assert!(error(r#"get_host("web-01")"#).contains("get_host"));
        assert!(error("true + 1").contains("(in `true + 1`)"));
    }

    #[test]
    fn compile_errors() {
        for source in [
            "host.name ==",
            r#"host.name == "x"#,
            "host.name $ 1",
            "var x = 1",
            "x = 1",
            "host.name = \"x\"",
            "1; 2",
            "{{ true }}",
            "function f() { }",
            &("(".repeat(10_000) + "true" + &")".repeat(10_000)),
            &("!".repeat(10_000) + "true"),
            &("[".repeat(10_000) + &"]".repeat(10_000)),
        ] {
            let error = compile(source, node()).unwrap_err();
            let text = error.to_string();
            assert!(text.starts_with("Error: "), "{source}: {text}");
            assert!(text.contains("\nLocation: in <API query>: 1:"), "{text}");
        }
        let error = compile("a ==\n  ", node()).unwrap_err();
        assert!(error.to_string().contains("in <API query>: 1:"), "{error}");
        assert!(
            compile(r#"host.name == "web\x2d01""#, node()).is_err(),
            "Icinga has no \\x escapes"
        );
        // ic-filter evaluates dictionary literals; Icinga's sandbox refuses
        // their assignments.
        assert!(eval("{ a = 1 }.a == 1").unwrap());
        let nested = "(".repeat(30) + "true" + &")".repeat(30);
        assert!(eval(&nested).unwrap());
    }

    #[test]
    fn strings_and_comments() {
        assert!(eval("host.name == {{{web-01}}}").unwrap());
        assert!(eval("host.name == \"web-01\" // trailing comment").unwrap());
        assert!(eval("/* leading */ host.name == \"web-01\"").unwrap());
        assert!(eval("\"\\101\" == \"A\"").unwrap(), "octal escapes");
    }

    #[test]
    fn empty_filters_match_nothing() {
        for source in ["", "  \n ", "// only a comment"] {
            assert!(!eval(source).unwrap(), "{source:?}");
        }
        assert!(!ApiFilter::nothing().matches(&frame()).unwrap());
    }

    #[test]
    fn values_follow_icinga_paths() {
        let json = json!({ "a": { "b": [1, 2] }, "n": null, "s": "text" });
        assert_eq!(follow_json(&json, &["a", "x"]), Some(Value::Null));
        assert_eq!(follow_json(&json, &["n", "x", "y"]), Some(Value::Null));
        assert_eq!(
            follow_json(&json, &["s", "x"]),
            None,
            "strings have no members"
        );
        assert_eq!(
            follow_json(&json, &["a", "b", "0"]),
            None,
            "arrays are left to ic-filter"
        );
        let value = Value::from_json(&json);
        assert_eq!(follow_value(&value, &["a", "x"]), Some(Value::Null));
        assert_eq!(follow_value(&value, &["s", "x"]), None);
        assert_eq!(follow_value(&value, &["s"]), Some(Value::from("text")));
    }
}
