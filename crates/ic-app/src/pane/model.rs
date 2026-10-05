//! What the service and host panes show, computed from the snapshot. Pure,
//! so it's tested without a window.

use std::sync::Arc;

use ic_core::snapshot::Snapshot;
use ic_model::{
    CheckInfo, CheckableState, Comment, CommentKind, Downtime, Features, Host, ObjectKey, Service,
    ServiceState, Timestamp, Vars,
};
use serde_json::Value;

use crate::format;

/// How many service rows the host pane shows before collapsing the OK ones
/// into "+ N more ok" (screen 2c: two problems and five OK services).
pub(crate) const HOST_SERVICES_PREVIEW: usize = 7;

/// Limits for showing custom variables, so odd data can't flood the pane.
const VARS_MAX_DEPTH: usize = 8;
const VARS_MAX_LINES: usize = 400;

/// The service pane's subtitle after `on <host>`: `14m · hard 3/3`.
pub(crate) fn service_subtitle(service: &Service, now: Timestamp) -> String {
    let since = format::since(service.check.last_state_change, now);
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
    let since = format::since(host.check.last_state_change, now);
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

/// The host pane's service rows: every service that isn't OK (worst first),
/// then OK ones in name order up to [`HOST_SERVICES_PREVIEW`] rows unless
/// `expanded`; `hidden` OK services are left for "+ N more ok".
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HostServices {
    /// The rows to show.
    pub(crate) shown: Vec<Arc<Service>>,
    /// OK services not shown.
    pub(crate) hidden: usize,
    /// All of the host's services.
    pub(crate) total: usize,
}

/// See [`HostServices`].
pub(crate) fn host_services(snapshot: &Snapshot, host: &Host, expanded: bool) -> HostServices {
    let mut services: Vec<Arc<Service>> = snapshot.services_of(&host.name).cloned().collect();
    let total = services.len();
    services.sort_by(|a, b| {
        b.severity()
            .cmp(&a.severity())
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    let not_ok = services
        .iter()
        .filter(|service| service.state != ServiceState::Ok)
        .count();
    let keep = if expanded {
        total
    } else {
        HOST_SERVICES_PREVIEW.max(not_ok).min(total)
    };
    services.truncate(keep);
    HostServices {
        shown: services,
        hidden: total - keep,
        total,
    }
}

/// Custom variables as indented `(depth, key, value)` lines in key order
/// (as Icinga stores them): nested dictionaries and arrays get a summary
/// line (`{3}`, `[2]`) followed by their entries; arrays of plain values
/// stay on one line.
pub(crate) fn vars_lines(vars: &Vars) -> Vec<(usize, String, String)> {
    let mut lines = Vec::new();
    for (key, value) in sorted(vars) {
        push_var(&mut lines, 0, key.clone(), value);
    }
    if lines.len() > VARS_MAX_LINES {
        let more = lines.len() - VARS_MAX_LINES;
        lines.truncate(VARS_MAX_LINES);
        lines.push((0, "…".to_owned(), format!("{more} more lines")));
    }
    lines
}

fn push_var(lines: &mut Vec<(usize, String, String)>, depth: usize, key: String, value: &Value) {
    if lines.len() > VARS_MAX_LINES {
        return;
    }
    let nested = depth < VARS_MAX_DEPTH;
    match value {
        Value::Object(map) if nested && !map.is_empty() => {
            lines.push((depth, key, format!("{{{}}}", map.len())));
            for (child, value) in sorted(map) {
                push_var(lines, depth + 1, child.clone(), value);
            }
        }
        Value::Array(items)
            if nested && items.iter().any(|item| item.is_object() || item.is_array()) =>
        {
            lines.push((depth, key, format!("[{}]", items.len())));
            for (index, item) in items.iter().enumerate() {
                push_var(lines, depth + 1, index.to_string(), item);
            }
        }
        value => lines.push((depth, key, var_value(value))),
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

/// A comment or acknowledgement as the pane lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Note {
    /// `#` for comments, `✓` for acknowledgements, `~` for flapping notes.
    pub(crate) marker: &'static str,
    /// Who wrote it.
    pub(crate) author: String,
    /// Faint details after the author: when, expiry.
    pub(crate) meta: Vec<String>,
    /// The text.
    pub(crate) body: String,
    /// The full name, to remove it.
    pub(crate) name: String,
}

/// The pane's view of a comment.
pub(crate) fn comment_note(comment: &Comment, now: Timestamp) -> Note {
    let marker = match comment.kind {
        CommentKind::User => "#",
        CommentKind::Acknowledgement => "✓",
        CommentKind::Downtime => "↓",
        CommentKind::Flapping => "~",
    };
    let mut meta = vec![format::clock(comment.entry_time, now)];
    if comment.kind == CommentKind::Acknowledgement {
        meta.insert(0, "acknowledged".to_owned());
    }
    if let Some(expiry) = comment.expire_time.and_then(Timestamp::non_zero) {
        meta.push(format!("expires {}", format::clock(expiry, now)));
    }
    Note {
        marker,
        author: comment.author.clone(),
        meta,
        body: comment.text.clone(),
        name: comment.name.clone(),
    }
}

/// The pane's view of a downtime: `a.ivanova downtime 14:21 → 16:00 · 50m
/// left`.
pub(crate) fn downtime_note(downtime: &Downtime, now: Timestamp) -> Note {
    let window = format!(
        "{} → {}",
        format::clock(downtime.start_time, now),
        format::clock(downtime.end_time, now)
    );
    let status = if downtime.in_effect {
        format!(
            "{} left",
            ic_model::format_compact(downtime.end_time.remaining_from(now))
        )
    } else if downtime.start_time > now {
        format!(
            "starts in {}",
            ic_model::format_compact(downtime.start_time.remaining_from(now))
        )
    } else if !downtime.fixed {
        "flexible, not triggered".to_owned()
    } else {
        "ended".to_owned()
    };
    let mut meta = vec!["downtime".to_owned(), window, status];
    if downtime.config_owned {
        meta.push("from config".to_owned());
    }
    Note {
        marker: "↓",
        author: downtime.author.clone(),
        meta,
        body: downtime.comment.clone(),
        name: downtime.name.clone(),
    }
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ic_model::{CheckResult, Dependency, ServiceKey, StateType};
    use serde_json::json;

    use super::*;
    use crate::demo;

    const NOW: f64 = 1_790_000_000.;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(NOW)
    }

    fn ago(seconds: f64) -> Timestamp {
        Timestamp::from_unix_seconds(NOW - seconds)
    }

    #[test]
    fn subtitles_follow_the_design() {
        let demo = demo::build(now());
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
        let demo = demo::build(now());
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
        let demo = demo::build(now());
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
            (0, "disks", "{2}"),
            (1, "/", "{1}"),
            (2, "warn", "80%"),
            (1, "/var", "{}"),
            (0, "env", "prod"),
            (0, "slots", "[1]"),
            (1, "0", "{1}"),
            (2, "name", "repl"),
            (0, "tags", "[a, 1, true]"),
        ];
        let lines: Vec<(usize, &str, &str)> = lines
            .iter()
            .map(|(depth, key, value)| (*depth, key.as_str(), value.as_str()))
            .collect();
        assert_eq!(lines, expected);
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
        assert!(lines.last().unwrap().2.ends_with("more lines"));
    }

    #[test]
    fn notes_for_comments_acks_and_downtimes() {
        let comment = Comment {
            name: "h!s!1".to_owned(),
            object: ObjectKey::service("h", "s"),
            author: "m.keller".to_owned(),
            text: "renewal in progress".to_owned(),
            kind: CommentKind::Acknowledgement,
            entry_time: ago(60.),
            expire_time: None,
            persistent: false,
        };
        let note = comment_note(&comment, now());
        assert_eq!(note.marker, "✓");
        assert_eq!(note.meta[0], "acknowledged");
        assert_eq!(note.body, "renewal in progress");

        let demo = demo::build(now());
        let downtime = &demo.snapshot.downtimes[&ObjectKey::host("edge-fra-04")][0];
        let note = downtime_note(downtime, now());
        assert_eq!(note.marker, "↓");
        assert_eq!(note.meta[0], "downtime");
        assert_eq!(note.meta[2], "50m left");
        assert_eq!(note.body, "rack maintenance");
    }

    #[test]
    fn future_and_untriggered_downtimes() {
        let demo = demo::build(now());
        let mut downtime = demo.snapshot.downtimes[&ObjectKey::host("edge-fra-04")][0].clone();
        downtime.in_effect = false;
        downtime.start_time = Timestamp::from_unix_seconds(NOW + 600.);
        assert_eq!(downtime_note(&downtime, now()).meta[2], "starts in 10m");
        downtime.start_time = ago(60.);
        downtime.fixed = false;
        assert_eq!(
            downtime_note(&downtime, now()).meta[2],
            "flexible, not triggered"
        );
        downtime.config_owned = true;
        assert_eq!(
            downtime_note(&downtime, now()).meta.last().unwrap(),
            "from config"
        );
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

    #[test]
    fn feature_rows_list_every_switch() {
        let rows = feature_rows(Features::default());
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0], ("active checks", true));
        assert_eq!(rows[4], ("flap detection", false));
    }
}
