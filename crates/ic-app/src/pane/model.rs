//! What the service and host panes show, computed from the snapshot. Pure,
//! so it's tested without a window.

use std::sync::Arc;

use ic_core::snapshot::Snapshot;
use ic_model::{
    CheckInfo, CheckableState, Features, Host, Notified, ObjectKey, Service, ServiceState,
    Timestamp, Vars,
};
use ic_ui_kit::TreeLine;
use serde_json::Value;

use crate::format;

/// Limits for showing custom variables, so odd data can't flood the pane.
const VARS_MAX_DEPTH: usize = 8;
const VARS_MAX_LINES: usize = 400;

/// What a protected custom variable shows instead of its value.
pub(crate) const PROTECTED_VALUE: &str = "***";

/// Custom variables whose values are never shown (on screen, in shared
/// screenshots, in links): names matching these patterns, ignoring case,
/// `*` standing for any characters, at any nesting level. Icinga Web's
/// default protected custom variables (`*pw*`, `*pass*`, `community`, here
/// `*community*` so `snmp_community` is covered too) and other names the
/// ITL and common configs keep credentials in. The values stay in the
/// store: filters still see them.
const PROTECTED_VARS: &[&str] = &[
    "*pw*",
    "*pass*",
    "*community*",
    "*secret*",
    "*token*",
    "*auth_pair*",
    "*auth_key*",
    "*priv_key*",
];

/// Whether the custom variable (or dictionary key) `name` holds a secret.
pub(crate) fn is_protected_var(name: &str) -> bool {
    let name = name.to_lowercase();
    PROTECTED_VARS
        .iter()
        .any(|pattern| glob_matches(pattern, &name))
}

/// Whether `text` matches `pattern`, where `*` stands for any characters.
fn glob_matches(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        // No `*`: the whole text.
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// The service pane's subtitle after `on <host>`: `14m · hard 3/3`.
pub(crate) fn service_subtitle(service: &Service, now: Timestamp) -> String {
    let since = format::time_in_state(&service.check, now);
    let attempt = format::attempt(&service.check);
    if since.is_empty() {
        attempt
    } else {
        format!("{since} · {attempt}")
    }
}

/// The host pane's subtitle: `10.0.2.13 · up 41d · PING OK rta 0.42ms`.
pub(crate) fn host_subtitle(host: &Host, now: Timestamp) -> String {
    let state = format::state_word(CheckableState::Host(host.state));
    let since = format::time_in_state(&host.check, now);
    let state = if since.is_empty() {
        state.to_owned()
    } else {
        format!("{state} {since}")
    };
    [host.address.as_str(), state.as_str(), host.check.output()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The check details as `(label, value)` rows, the service pane's `check`
/// section and the host pane's config tab.
pub(crate) fn check_rows(check: &CheckInfo, now: Timestamp) -> Vec<(&'static str, String)> {
    let mut rows = vec![("command", check.check_command.clone())];
    rows.push((
        "interval",
        format!(
            "{} · retry {}",
            format::interval(check.check_interval),
            format::interval(check.retry_interval)
        ),
    ));
    rows.push((
        "last / next",
        format!(
            "{} / {}",
            format::ago(check.last_check, now),
            format::until(check.next_check, now)
        ),
    ));
    let source = check
        .result
        .as_ref()
        .map(|result| result.check_source.as_str())
        .filter(|source| !source.is_empty());
    let endpoint = check.command_endpoint.as_deref().or(source);
    if let Some(endpoint) = endpoint {
        rows.push(("endpoint", endpoint.to_owned()));
    }
    if let Some(zone) = check.zone.as_deref() {
        rows.push(("zone", zone.to_owned()));
    }
    rows.push(("attempt", format::attempt(check)));
    if let Some(result) = &check.result {
        rows.push((
            "latency / runtime",
            format!(
                "{} / {}",
                format::seconds(result.latency()),
                format::seconds(result.execution_time())
            ),
        ));
        if let Some(source) = source.filter(|source| Some(*source) != endpoint) {
            rows.push(("check source", source.to_owned()));
        }
    }
    rows.push((
        "notifications",
        if check.features.notifications {
            "enabled".to_owned()
        } else {
            "disabled in Icinga".to_owned()
        },
    ));
    if check.features.flap_detection || check.flapping {
        rows.push((
            "flapping",
            format!(
                "{} · {}%",
                if check.flapping { "yes" } else { "no" },
                ic_model::format_number((check.flapping_current * 10.).round() / 10.)
            ),
        ));
    }
    if !check.features.active_checks {
        rows.push(("active checks", "disabled in Icinga".to_owned()));
    }
    rows
}

/// The most users the "notified" row names before `+N`.
const NOTIFIED_USERS_SHOWN: usize = 3;

/// The "notified" row (PANE-06, design 2b: `dba-oncall · 14:32`): who
/// Icinga notified about the current problem and when it last notified
/// anyone, from its `Notification` objects. `readable` is whether the API
/// user may read them (`None` while unknown).
pub(crate) fn notified_text(notified: &Notified, readable: Option<bool>, now: Timestamp) -> String {
    if readable == Some(false) {
        return "unknown (needs objects/query/Notification)".to_owned();
    }
    let users = match notified.users.len() {
        0 => String::new(),
        count if count <= NOTIFIED_USERS_SHOWN => notified.users.join(", "),
        count => format!(
            "{} +{}",
            notified.users[..NOTIFIED_USERS_SHOWN].join(", "),
            count - NOTIFIED_USERS_SHOWN
        ),
    };
    let when = notified
        .last_notification
        .and_then(Timestamp::non_zero)
        .map(|at| format::clock(at, now));
    match (users.is_empty(), when) {
        (true, None) => "not notified".to_owned(),
        (true, Some(when)) => format!("last {when}"),
        (false, None) => users,
        (false, Some(when)) => format!("{users} · {when}"),
    }
}

/// The "late" row: when Icinga expected the result and how long ago
/// (PERF-08); `None` unless the check is late.
pub(crate) fn late_text(deadline: Option<Timestamp>, now: Timestamp) -> Option<String> {
    let deadline = deadline?;
    let overdue = deadline.elapsed_until(now);
    Some(format!(
        "expected {} · {} overdue",
        format::clock(deadline, now),
        ic_model::format_compact(overdue)
    ))
}

/// The check rows of an object with its "late" and "notified" rows: the
/// service pane's `check` section and the host pane's config tab.
pub(crate) fn object_check_rows(
    snapshot: &Snapshot,
    key: &ObjectKey,
    check: &CheckInfo,
    readable: Option<bool>,
    now: Timestamp,
) -> Vec<(&'static str, String)> {
    let mut rows = check_rows(check, now);
    if let Some(late) = late_text(snapshot.late.get(key).copied(), now) {
        // Right after "last / next", which it explains.
        let at = rows
            .iter()
            .position(|(label, _)| *label == "last / next")
            .map_or(rows.len(), |index| index + 1);
        rows.insert(at, ("late", late));
    }
    rows.push((
        "notified",
        notified_text(&snapshot.notified(key), readable, now),
    ));
    rows
}

/// Icinga's feature switches, read-only (PLAN.md D6), for the config tab.
pub(crate) fn feature_rows(features: Features) -> [(&'static str, bool); 6] {
    [
        ("active checks", features.active_checks),
        ("passive checks", features.passive_checks),
        ("notifications", features.notifications),
        ("event handler", features.event_handler),
        ("flap detection", features.flap_detection),
        ("performance data", features.perfdata),
    ]
}

/// The host pane's service rows, paged by count as every host-with-services
/// view pages ([`crate::paging`]): every service that isn't OK (worst
/// first), then OK ones in name order up to seven rows unless `expanded`;
/// `hidden` services are left for `+ N more`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HostServices {
    /// The rows to show.
    pub(crate) shown: Vec<Arc<Service>>,
    /// Services not shown.
    pub(crate) hidden: usize,
    /// All of the host's services.
    pub(crate) total: usize,
    /// Some services wait behind `+ N more` unless expanded: the paging
    /// row shows (`+ N more`, or `− show fewer` in the same slot).
    pub(crate) pages: bool,
}

/// See [`HostServices`].
pub(crate) fn host_services(snapshot: &Snapshot, host: &Host, expanded: bool) -> HostServices {
    let mut services: Vec<Arc<Service>> = snapshot.services_of(&host.name).cloned().collect();
    let total = services.len();
    services.sort_by(|a, b| {
        crate::paging::service_order(
            (a.severity(), &a.display_name),
            (b.severity(), &b.display_name),
        )
    });
    let not_ok = services
        .iter()
        .filter(|service| service.state != ServiceState::Ok)
        .count();
    let keep = crate::paging::shown_count(total, not_ok, expanded);
    services.truncate(keep);
    HostServices {
        shown: services,
        hidden: total - keep,
        total,
        pages: crate::paging::pages(total, not_ok),
    }
}

/// Custom variables as indented lines in key order (as Icinga stores them):
/// nested dictionaries and arrays get a summary line (`{3}`, `[2]`) followed
/// by their entries; arrays of plain values stay on one line. Protected
/// variables ([`is_protected_var`]) show `***`, whatever they hold.
pub(crate) fn vars_lines(vars: &Vars) -> Vec<TreeLine> {
    let mut lines = Vec::new();
    for (key, value) in sorted(vars) {
        push_var(&mut lines, 0, key.clone(), value);
    }
    if lines.len() > VARS_MAX_LINES {
        let more = lines.len() - VARS_MAX_LINES;
        lines.truncate(VARS_MAX_LINES);
        lines.push(TreeLine::summary(0, "…", format!("{more} more lines")));
    }
    lines
}

fn push_var(lines: &mut Vec<TreeLine>, depth: usize, key: String, value: &Value) {
    if lines.len() > VARS_MAX_LINES {
        return;
    }
    if is_protected_var(&key) {
        lines.push(TreeLine::new(depth, key, PROTECTED_VALUE));
        return;
    }
    let nested = depth < VARS_MAX_DEPTH;
    match value {
        Value::Object(map) if nested && !map.is_empty() => {
            lines.push(TreeLine::summary(depth, key, format!("{{{}}}", map.len())));
            for (child, value) in sorted(map) {
                push_var(lines, depth + 1, child.clone(), value);
            }
        }
        Value::Array(items)
            if nested && items.iter().any(|item| item.is_object() || item.is_array()) =>
        {
            lines.push(TreeLine::summary(depth, key, format!("[{}]", items.len())));
            for (index, item) in items.iter().enumerate() {
                push_var(lines, depth + 1, index.to_string(), item);
            }
        }
        value => lines.push(TreeLine::new(depth, key, var_value(value))),
    }
}

/// A dictionary's entries in key order, whatever order the map keeps.
fn sorted(map: &serde_json::Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by_key(|(key, _)| *key);
    entries
}

/// A variable's value on one line: strings without quotes, arrays of plain
/// values as `[a, b]`.
pub(crate) fn var_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(var_value).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) if map.is_empty() => "{}".to_owned(),
        Value::Object(_) => value.to_string(),
    }
}

/// Group names as their display names, comma separated.
pub(crate) fn group_names(
    groups: &[String],
    display_name: impl Fn(&str) -> Option<String>,
) -> String {
    groups
        .iter()
        .map(|group| display_name(group).unwrap_or_else(|| group.clone()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The host's parents and children from the dependencies.
pub(crate) fn host_relations(snapshot: &Snapshot, host: &Host) -> (Vec<ObjectKey>, Vec<ObjectKey>) {
    let key = host.key();
    let mut parents: Vec<ObjectKey> = Vec::new();
    let mut children: Vec<ObjectKey> = Vec::new();
    for dependency in snapshot.dependencies.iter() {
        if dependency.child == key && !parents.contains(&dependency.parent) {
            parents.push(dependency.parent.clone());
        }
        if dependency.parent == key && !children.contains(&dependency.child) {
            children.push(dependency.child.clone());
        }
    }
    (parents, children)
}

/// Whether a link may be opened in the browser (web links only).
pub(crate) fn is_web_link(url: &str) -> bool {
    let url = url.trim();
    url.starts_with("https://") || url.starts_with("http://")
}

/// The objects a notes or action URL's macros refer to: the host, and the
/// service in a service pane.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MacroScope<'a> {
    /// The host (a service's host in a service pane).
    pub(crate) host: Option<&'a Host>,
    /// The service, in a service pane.
    pub(crate) service: Option<&'a Service>,
}

/// A `notes_url` or `action_url` as the links Icinga Web shows for it:
/// several URLs written as `'url1' 'url2'` are split, and each one's
/// macros are resolved ([`resolve_macros`]).
pub(crate) fn link_urls(raw: &str, scope: MacroScope<'_>) -> Vec<String> {
    split_urls(raw)
        .into_iter()
        .map(|url| resolve_macros(url, scope))
        .collect()
}

/// Splits Icinga Web's list syntax for several URLs in one attribute,
/// `'url1' 'url2'`; a single URL may be quoted too.
pub(crate) fn split_urls(raw: &str) -> Vec<&str> {
    raw.split("' ")
        .map(|url| {
            url.trim()
                .trim_start_matches('\'')
                .trim_end_matches('\'')
                .trim()
        })
        .filter(|url| !url.is_empty())
        .collect()
}

/// Resolves Icinga's macros in a URL the way Icinga Web does: the classic
/// names (`$HOSTNAME$`, `$HOSTADDRESS$`, `$SERVICEDESC$`, …), the object
/// attributes (`$host.name$`, `$host.address$`, `$service.name$`,
/// `$service.display_name$`, …) and custom variables (`$host.vars.role$`,
/// `$service.vars.team$`; `$vars.x$` is the pane object's own). Values are
/// inserted as they are, as Icinga Web inserts them, so a variable can hold
/// a whole base URL or a `host:port`; a protected one ([`is_protected_var`])
/// becomes `***`. Macros that don't resolve stay as written. Characters no URL may contain (spaces, quotes, non-ASCII) are
/// then percent-encoded, as a browser would, so the platform's URL opener
/// accepts the result.
pub(crate) fn resolve_macros(url: &str, scope: MacroScope<'_>) -> String {
    let mut resolved = String::with_capacity(url.len());
    let mut rest = url;
    while let Some(start) = rest.find('$') {
        resolved.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        // Icinga Web's pattern: `$`, a name without `$` or white space, `$`.
        let name_len = after
            .find(|c: char| c == '$' || c.is_whitespace())
            .filter(|&end| end > 0 && after[end..].starts_with('$'));
        let Some(len) = name_len else {
            resolved.push('$');
            rest = after;
            continue;
        };
        let name = &after[..len];
        if let Some(value) = macro_value(name, scope) {
            resolved.push_str(&value);
        } else {
            // Unresolved: keep it as written.
            resolved.push_str(&rest[start..=start + len + 1]);
        }
        rest = &after[len + 1..];
    }
    resolved.push_str(rest);
    encode_invalid_url_chars(&resolved)
}

/// The value of macro `name`, if it resolves to something.
fn macro_value(name: &str, scope: MacroScope<'_>) -> Option<String> {
    let MacroScope { host, service } = scope;
    let value = match name {
        "HOSTNAME" | "host.name" => host.map(|host| host.name.to_string()),
        "HOSTDISPLAYNAME" | "HOSTALIAS" | "host.display_name" => {
            host.map(|host| host.display_name.clone())
        }
        "HOSTADDRESS" | "host.address" => host.map(|host| host.address.clone()),
        "HOSTADDRESS6" | "host.address6" => host.map(|host| host.address6.clone()),
        "SERVICEDESC" | "service.name" | "service.description" => {
            service.map(|service| service.key.name.to_string())
        }
        "SERVICEDISPLAYNAME" | "service.display_name" => {
            service.map(|service| service.display_name.clone())
        }
        _ => {
            let (vars, path) = if let Some(path) = name.strip_prefix("host.vars.") {
                (&host?.vars, path)
            } else if let Some(path) = name.strip_prefix("service.vars.") {
                (&service?.vars, path)
            } else {
                // `vars.x`: the pane object's own variable.
                let path = name.strip_prefix("vars.")?;
                let own = match service {
                    Some(service) => &service.vars,
                    None => &host?.vars,
                };
                (own, path)
            };
            var_at(vars, path)
        }
    };
    value.filter(|value| !value.is_empty())
}

/// A custom variable's plain value by name, or by a dotted path into nested
/// dictionaries (`disks.root`). Dictionaries, arrays and null don't resolve;
/// a protected variable, or one inside a protected dictionary, is `***`.
fn var_at(vars: &Vars, path: &str) -> Option<String> {
    let mut value = vars.get(path);
    let mut protected = is_protected_var(path);
    if value.is_none() {
        let mut parts = path.split('.');
        let first = parts.next()?;
        protected = is_protected_var(first);
        value = vars.get(first);
        for part in parts {
            protected |= is_protected_var(part);
            value = value?.as_object()?.get(part);
        }
    }
    if protected && value.is_some_and(|value| !value.is_null()) {
        return Some(PROTECTED_VALUE.to_owned());
    }
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// Percent-encodes the bytes a URL can't contain literally (RFC 3986):
/// white space, control characters, quotes, angle brackets, braces, the
/// backslash, caret, backtick and pipe, and everything outside ASCII.
fn encode_invalid_url_chars(url: &str) -> String {
    const ALLOWED: &[u8] = b"-._~:/?#[]@!$&'()*+,;=%";
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(url.len());
    for byte in url.bytes() {
        if byte.is_ascii_alphanumeric() || ALLOWED.contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ic_model::{CheckResult, Dependency, ServiceKey, StateType};
    use serde_json::json;

    use super::*;
    use crate::fixture;

    const NOW: f64 = 1_790_000_000.;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(NOW)
    }

    #[test]
    fn subtitles_follow_the_design() {
        let demo = fixture::build(now());
        let service =
            &demo.snapshot.services[&ServiceKey::new("db-prod-03", "postgres-replication")];
        assert_eq!(service_subtitle(service, now()), "14m · hard 3/3");
        let host = &demo.snapshot.hosts[&ic_model::HostName::new("db-prod-03")];
        assert_eq!(
            host_subtitle(host, now()),
            "10.0.2.13 · up 41d · PING OK rta 0.42ms"
        );
        let mut pending = Host::new("new");
        pending.address = "10.9.9.9".to_owned();
        assert_eq!(host_subtitle(&pending, now()), "10.9.9.9 · pending");
    }

    #[test]
    fn check_rows_match_the_design() {
        let demo = fixture::build(now());
        let service =
            &demo.snapshot.services[&ServiceKey::new("db-prod-03", "postgres-replication")];
        let rows: BTreeMap<_, _> = check_rows(&service.check, now()).into_iter().collect();
        assert_eq!(rows["command"], "check_postgres");
        assert_eq!(rows["interval"], "60s · retry 15s");
        assert_eq!(rows["last / next"], "12s ago / in 48s");
        assert_eq!(rows["endpoint"], "sat-ams-01");
        assert_eq!(rows["attempt"], "hard 3/3");
        assert_eq!(rows["latency / runtime"], "0.010s / 1.82s");
        assert_eq!(rows["notifications"], "enabled");
        assert_eq!(rows["flapping"], "no · 4.2%");
        assert!(!rows.contains_key("check source"), "same as the endpoint");
    }

    #[test]
    fn check_rows_flag_disabled_checks_and_show_other_sources() {
        let mut check = CheckInfo {
            command_endpoint: Some("agent-01".to_owned()),
            result: Some(CheckResult {
                check_source: "sat-fra-01".to_owned(),
                ..CheckResult::default()
            }),
            state_type: StateType::Soft,
            attempt: 2,
            ..CheckInfo::default()
        };
        check.features.active_checks = false;
        check.features.notifications = false;
        let rows: BTreeMap<_, _> = check_rows(&check, now()).into_iter().collect();
        assert_eq!(rows["check source"], "sat-fra-01");
        assert_eq!(rows["active checks"], "disabled in Icinga");
        assert_eq!(rows["notifications"], "disabled in Icinga");
        assert_eq!(rows["attempt"], "soft 2/1");
        assert_eq!(rows["last / next"], "never / not scheduled");
        assert!(!rows.contains_key("flapping"));
    }

    #[test]
    fn host_services_collapse_ok_ones_like_the_design() {
        let demo = fixture::build(now());
        let host = &demo.snapshot.hosts[&ic_model::HostName::new("db-prod-03")];
        let preview = host_services(&demo.snapshot, host, false);
        let names: Vec<&str> = preview
            .shown
            .iter()
            .map(|service| service.display_name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "postgres-replication",
                "pg-connections",
                "disk /",
                "disk /var/lib/postgresql",
                "load",
                "memory",
                "ntp-offset"
            ]
        );
        assert_eq!((preview.hidden, preview.total), (16, 23));
        let all = host_services(&demo.snapshot, host, true);
        assert_eq!((all.shown.len(), all.hidden), (23, 0));
    }

    #[test]
    fn hosts_with_many_problems_show_them_all() {
        let mut snapshot = Snapshot::default();
        let host = Host::new("h");
        let services: BTreeMap<ServiceKey, Arc<Service>> = (0..10)
            .map(|index| {
                let mut service = Service::new("h", &format!("s{index}"));
                service.state = ServiceState::Critical;
                (service.key.clone(), Arc::new(service))
            })
            .collect();
        snapshot.services = Arc::new(services);
        let preview = host_services(&snapshot, &host, false);
        assert_eq!((preview.shown.len(), preview.hidden), (10, 0));
        let empty = host_services(&Snapshot::default(), &host, false);
        assert_eq!(empty, HostServices::default());
    }

    #[test]
    fn vars_become_an_indented_tree() {
        let mut vars = Vars::new();
        vars.insert("env".to_owned(), json!("prod"));
        vars.insert(
            "disks".to_owned(),
            json!({ "/": { "warn": "80%" }, "/var": {} }),
        );
        vars.insert("tags".to_owned(), json!(["a", 1, true]));
        vars.insert("slots".to_owned(), json!([{ "name": "repl" }]));
        let lines = vars_lines(&vars);
        let expected = [
            (0, "disks", "{2}", true),
            (1, "/", "{1}", true),
            (2, "warn", "80%", false),
            (1, "/var", "{}", false),
            (0, "env", "prod", false),
            (0, "slots", "[1]", true),
            (1, "0", "{1}", true),
            (2, "name", "repl", false),
            (0, "tags", "[a, 1, true]", false),
        ];
        let lines: Vec<(usize, &str, &str, bool)> = lines
            .iter()
            .map(|line| {
                (
                    line.depth,
                    line.key.as_str(),
                    line.value.as_str(),
                    line.summary,
                )
            })
            .collect();
        assert_eq!(lines, expected);
    }

    #[test]
    fn protected_vars_never_show_their_values() {
        let mut vars = Vars::new();
        vars.insert("mysql_password".to_owned(), json!("hunter2"));
        vars.insert("SNMP_Community".to_owned(), json!("public"));
        vars.insert("api_token".to_owned(), json!({ "id": 1, "key": "abc" }));
        vars.insert(
            "http_vhosts".to_owned(),
            json!({ "site": { "http_uri": "/", "http_auth_pair": "user:pw" } }),
        );
        vars.insert("snmpv3_priv_key".to_owned(), json!(["k1"]));
        vars.insert("role".to_owned(), json!("db"));
        let lines = vars_lines(&vars);
        let shown: Vec<(usize, &str, &str)> = lines
            .iter()
            .map(|line| (line.depth, line.key.as_str(), line.value.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (0, "SNMP_Community", "***"),
                (0, "api_token", "***"),
                (0, "http_vhosts", "{1}"),
                (1, "site", "{2}"),
                (2, "http_auth_pair", "***"),
                (2, "http_uri", "/"),
                (0, "mysql_password", "***"),
                (0, "role", "db"),
                (0, "snmpv3_priv_key", "***"),
            ]
        );
        for secret in ["hunter2", "public", "abc", "user:pw", "k1"] {
            assert!(
                lines.iter().all(|line| !line.value.contains(secret)),
                "{secret} shows"
            );
        }
        // Neither do links that use them.
        let mut host = Host::new("db-01");
        host.vars = vars;
        let scope = MacroScope {
            host: Some(&host),
            service: None,
        };
        assert_eq!(
            resolve_macros(
                "https://x/?p=$host.vars.mysql_password$&v=$vars.http_vhosts.site.http_auth_pair$&r=$vars.role$",
                scope
            ),
            "https://x/?p=***&v=***&r=db"
        );
    }

    #[test]
    fn protected_names_match_like_icinga_web() {
        for name in [
            "pw",
            "db_pw",
            "PASSWORD",
            "passphrase",
            "community",
            "snmp_community",
            "client_secret",
            "vault_token",
        ] {
            assert!(is_protected_var(name), "{name}");
        }
        for name in ["role", "address", "power_supply", "snmp_version", "keys"] {
            assert!(!is_protected_var(name), "{name}");
        }
        assert!(glob_matches("a*b*c", "axxbyyc"));
        assert!(!glob_matches("a*b*c", "axxcyyb"));
        assert!(glob_matches("exact", "exact"));
        assert!(!glob_matches("exact", "exactly"));
    }

    #[test]
    fn huge_vars_are_cut_off() {
        let mut vars = Vars::new();
        vars.insert(
            "many".to_owned(),
            Value::Array((0..1000).map(|index| json!({ "i": index })).collect()),
        );
        let lines = vars_lines(&vars);
        assert_eq!(lines.len(), VARS_MAX_LINES + 1);
        let last = lines.last().unwrap();
        assert!(last.value.ends_with("more lines"));
        assert!(last.summary);
    }

    #[test]
    fn relations_come_from_dependencies() {
        let snapshot = Snapshot {
            dependencies: Arc::new(vec![
                Dependency {
                    name: "a!up".to_owned(),
                    child: ObjectKey::host("a"),
                    parent: ObjectKey::host("switch"),
                },
                Dependency {
                    name: "a!up2".to_owned(),
                    child: ObjectKey::host("a"),
                    parent: ObjectKey::host("switch"),
                },
                Dependency {
                    name: "b!up".to_owned(),
                    child: ObjectKey::host("b"),
                    parent: ObjectKey::host("a"),
                },
            ]),
            ..Snapshot::default()
        };
        let (parents, children) = host_relations(&snapshot, &Host::new("a"));
        assert_eq!(parents, [ObjectKey::host("switch")]);
        assert_eq!(children, [ObjectKey::host("b")]);
    }

    #[test]
    fn group_names_and_links() {
        let names = group_names(&["db-prod".to_owned(), "x".to_owned()], |name| {
            (name == "db-prod").then(|| "Production databases".to_owned())
        });
        assert_eq!(names, "Production databases, x");
        assert!(is_web_link("https://wiki.example.com/x"));
        assert!(!is_web_link("javascript:alert(1)"));
        assert!(!is_web_link("file:///etc/passwd"));
    }

    fn macro_objects() -> (Host, Service) {
        let mut host = Host::new("db-prod-03");
        host.display_name = "DB prod 03".to_owned();
        host.address = "10.0.2.13".to_owned();
        host.address6 = "fd00::13".to_owned();
        host.vars.insert("role".to_owned(), json!("postgres"));
        host.vars.insert("rack".to_owned(), json!(12));
        host.vars
            .insert("grafana".to_owned(), json!("https://grafana.example.com"));
        host.vars
            .insert("disks".to_owned(), json!({ "root": "/dev/sda1" }));
        host.vars.insert("tags".to_owned(), json!(["a", "b"]));
        let mut service = Service::new("db-prod-03", "disk /var");
        service.display_name = "Disk /var".to_owned();
        service.vars.insert("team".to_owned(), json!("dba"));
        (host, service)
    }

    #[test]
    fn macros_resolve_like_icinga_web() {
        let (host, service) = macro_objects();
        let scope = MacroScope {
            host: Some(&host),
            service: Some(&service),
        };
        let cases = [
            ("https://wiki/$HOSTNAME$", "https://wiki/db-prod-03"),
            ("https://wiki/$host.name$", "https://wiki/db-prod-03"),
            ("http://$HOSTADDRESS$/status", "http://10.0.2.13/status"),
            ("http://[$host.address6$]/", "http://[fd00::13]/"),
            (
                "https://g/d/x?var-host=$host.name$&var-svc=$SERVICEDESC$",
                "https://g/d/x?var-host=db-prod-03&var-svc=disk%20/var",
            ),
            ("https://w/$service.name$", "https://w/disk%20/var"),
            ("https://w/$service.display_name$", "https://w/Disk%20/var"),
            ("https://w/$HOSTDISPLAYNAME$", "https://w/DB%20prod%2003"),
            (
                "https://w/$host.vars.role$/$host.vars.rack$",
                "https://w/postgres/12",
            ),
            ("https://w/$service.vars.team$", "https://w/dba"),
            ("https://w/$vars.team$", "https://w/dba"),
            ("https://w/$host.vars.disks.root$", "https://w//dev/sda1"),
            (
                "$host.vars.grafana$/d/pg?var-host=$HOSTNAME$",
                "https://grafana.example.com/d/pg?var-host=db-prod-03",
            ),
            // Unknown, unset and non-scalar macros stay as written.
            ("https://w/$host.vars.nope$", "https://w/$host.vars.nope$"),
            ("https://w/$host.vars.tags$", "https://w/$host.vars.tags$"),
            ("https://w/$USER1$", "https://w/$USER1$"),
            // Not macros: a lone `$`, `$$`, white space inside.
            ("https://w/?price=5$", "https://w/?price=5$"),
            ("https://w/$$x", "https://w/$$x"),
            ("https://w/$a b$HOSTNAME$", "https://w/$a%20bdb-prod-03"),
            ("https://w/ü", "https://w/%C3%BC"),
        ];
        for (url, expected) in cases {
            assert_eq!(resolve_macros(url, scope), expected, "{url}");
        }
    }

    #[test]
    fn host_panes_leave_service_macros_alone() {
        let (host, _) = macro_objects();
        let scope = MacroScope {
            host: Some(&host),
            service: None,
        };
        assert_eq!(
            resolve_macros("https://w/$SERVICEDESC$/$vars.role$", scope),
            "https://w/$SERVICEDESC$/postgres",
            "`vars.` is the host's own in a host pane"
        );
        assert_eq!(
            resolve_macros("https://w/$HOSTNAME$", MacroScope::default()),
            "https://w/$HOSTNAME$"
        );
    }

    #[test]
    fn several_quoted_urls_become_several_links() {
        assert_eq!(
            split_urls("'https://a/1' 'https://b/2'  'https://c/3'"),
            ["https://a/1", "https://b/2", "https://c/3"]
        );
        assert_eq!(split_urls("https://a/1"), ["https://a/1"]);
        assert_eq!(split_urls("'https://a/1'"), ["https://a/1"]);
        assert!(split_urls("  ").is_empty());
        let (host, _) = macro_objects();
        let scope = MacroScope {
            host: Some(&host),
            service: None,
        };
        assert_eq!(
            link_urls("'https://a/$HOSTNAME$' '/grafana/$HOSTNAME$'", scope),
            ["https://a/db-prod-03", "/grafana/db-prod-03"]
        );
    }

    #[test]
    fn feature_rows_list_every_switch() {
        let rows = feature_rows(Features::default());
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0], ("active checks", true));
        assert_eq!(rows[4], ("flap detection", false));
    }

    #[test]
    fn the_notified_row_names_users_and_time() {
        let at = Timestamp::from_unix_seconds(NOW - 600.);
        let notified = |users: &[&str], last: Option<Timestamp>| Notified {
            last_notification: last,
            users: users.iter().map(|user| (*user).to_owned()).collect(),
        };
        let clock = format::clock(at, now());
        assert_eq!(
            notified_text(&notified(&["dba-oncall"], Some(at)), Some(true), now()),
            format!("dba-oncall · {clock}")
        );
        assert_eq!(
            notified_text(&notified(&["a", "b", "c", "d", "e"], None), None, now()),
            "a, b, c +2"
        );
        assert_eq!(
            notified_text(&notified(&[], Some(at)), Some(true), now()),
            format!("last {clock}"),
            "after a recovery nobody is notified about a problem"
        );
        assert_eq!(
            notified_text(&Notified::default(), Some(true), now()),
            "not notified"
        );
        assert_eq!(
            notified_text(&Notified::default(), Some(false), now()),
            "unknown (needs objects/query/Notification)"
        );
    }

    #[test]
    fn late_checks_get_a_row_after_last_and_next() {
        let fixture = fixture::build(now());
        let key = ObjectKey::service("db-prod-03", "postgres-replication");
        let service = fixture
            .snapshot
            .services
            .get(key.as_service().unwrap())
            .unwrap();
        let rows = object_check_rows(&fixture.snapshot, &key, &service.check, Some(true), now());
        assert!(rows.iter().all(|(label, _)| *label != "late"));
        assert_eq!(
            rows.last().unwrap(),
            &("notified", "not notified".to_owned())
        );

        let mut snapshot = fixture.snapshot.clone();
        let deadline = Timestamp::from_unix_seconds(NOW - 12. * 60.);
        snapshot.late = Arc::new([(key.clone(), deadline)].into_iter().collect());
        let rows = object_check_rows(&snapshot, &key, &service.check, Some(true), now());
        let late = rows.iter().position(|(label, _)| *label == "late").unwrap();
        assert_eq!(rows[late - 1].0, "last / next");
        assert!(rows[late].1.ends_with("12m overdue"), "{}", rows[late].1);
        assert_eq!(late_text(None, now()), None);
    }
}
