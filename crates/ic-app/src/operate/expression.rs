//! Text to copy for objects (PANE-05): their names, and an Icinga filter
//! expression that matches exactly them, to paste into a dashboard's
//! filter, Icinga Web's URL filter or an API call.

use std::collections::BTreeMap;

use ic_model::ObjectKey;

/// The objects' full names (`host`, `host!service`), one per line.
pub(crate) fn names(objects: &[ObjectKey]) -> String {
    objects
        .iter()
        .map(ObjectKey::full_name)
        .collect::<Vec<_>>()
        .join("\n")
}

/// An Icinga filter expression matching `objects`:
/// `host.name == "db-01" && service.name == "disk"` for one service,
/// `host.name in ["a", "b"]` for hosts, services grouped by host, joined
/// with `||`. Empty for no objects.
pub(crate) fn filter(objects: &[ObjectKey]) -> String {
    let mut hosts: Vec<&str> = Vec::new();
    let mut services: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for object in objects {
        match object {
            ObjectKey::Host { name } => {
                if !hosts.contains(&name.as_str()) {
                    hosts.push(name.as_str());
                }
            }
            ObjectKey::Service { key } => {
                let names = services.entry(key.host.as_str()).or_default();
                if !names.contains(&key.name.as_ref()) {
                    names.push(&key.name);
                }
            }
        }
    }
    let mut clauses: Vec<String> = Vec::new();
    match hosts.as_slice() {
        [] => {}
        [one] => clauses.push(format!("host.name == {}", quote(one))),
        many => clauses.push(format!("host.name in {}", list(many))),
    }
    for (host, names) in &services {
        let service = match names.as_slice() {
            [one] => format!("service.name == {}", quote(one)),
            many => format!("service.name in {}", list(many)),
        };
        clauses.push(format!("host.name == {} && {service}", quote(host)));
    }
    if clauses.len() > 1 {
        clauses
            .iter()
            .map(|clause| {
                if clause.contains("&&") {
                    format!("({clause})")
                } else {
                    clause.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" || ")
    } else {
        clauses.pop().unwrap_or_default()
    }
}

/// A string literal in Icinga's language (`"…"` with `\"` and `\\`).
fn quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\t' => quoted.push_str("\\t"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

fn list(names: &[&str]) -> String {
    format!(
        "[{}]",
        names
            .iter()
            .map(|name| quote(name))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use ic_filter::{Filter, HostScope, ServiceScope};
    use ic_model::{Host, Service};

    use super::*;

    #[test]
    fn names_one_per_line() {
        assert_eq!(
            names(&[ObjectKey::host("a"), ObjectKey::service("b", "disk")]),
            "a\nb!disk"
        );
        assert_eq!(names(&[]), "");
    }

    #[test]
    fn filters_for_one_object() {
        assert_eq!(
            filter(&[ObjectKey::host("db-01")]),
            "host.name == \"db-01\""
        );
        assert_eq!(
            filter(&[ObjectKey::service("db-01", "disk /")]),
            "host.name == \"db-01\" && service.name == \"disk /\""
        );
        assert_eq!(filter(&[]), "");
    }

    #[test]
    fn filters_for_several_objects_group_by_host() {
        let objects = [
            ObjectKey::service("db-01", "disk"),
            ObjectKey::host("web-01"),
            ObjectKey::service("db-01", "load"),
            ObjectKey::host("web-02"),
            ObjectKey::service("db-02", "disk"),
            ObjectKey::service("db-01", "disk"),
        ];
        assert_eq!(
            filter(&objects),
            "host.name in [\"web-01\", \"web-02\"] || \
             (host.name == \"db-01\" && service.name in [\"disk\", \"load\"]) || \
             (host.name == \"db-02\" && service.name == \"disk\")"
        );
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        assert_eq!(quote(r#"a "b" \c"#), r#""a \"b\" \\c""#);
    }

    #[test]
    fn the_expression_matches_exactly_the_objects() {
        let objects = [
            ObjectKey::service("db-01", "disk \"root\""),
            ObjectKey::service("db-01", "load"),
            ObjectKey::host("web-01"),
        ];
        let expression = filter(&objects);
        let parsed = Filter::parse(&expression).unwrap();
        let db = Host::new("db-01");
        let web = Host::new("web-01");
        let matches = |service: &Service, host: &Host| {
            parsed.matches(&ServiceScope {
                service,
                host: Some(host),
            })
        };
        assert!(matches(&Service::new("db-01", "disk \"root\""), &db));
        assert!(matches(&Service::new("db-01", "load"), &db));
        assert!(!matches(&Service::new("db-01", "swap"), &db));
        assert!(parsed.matches(&HostScope { host: &web }));
        assert!(!parsed.matches(&HostScope { host: &db }));
    }
}
