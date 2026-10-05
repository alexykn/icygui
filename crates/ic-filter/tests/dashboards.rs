//! Realistic dashboard and API filters evaluated against `ic-model` hosts and
//! services through [`HostScope`], [`ServiceScope`], [`VarsScope`] and
//! [`Chain`].

use std::collections::BTreeMap;

use ic_filter::{Chain, Filter, HostScope, ParseError, Scope, ServiceScope, Value, VarsScope};
use ic_model::{
    AckKind, CheckResult, Host, HostState, Service, ServiceState, StateType, Timestamp,
};
use serde_json::json;

const NOW: f64 = 1_700_000_000.0;

fn host(
    name: &str,
    state: HostState,
    address: &str,
    groups: &[&str],
    vars: &serde_json::Value,
) -> Host {
    let mut host = Host::new(name);
    host.state = state;
    address.clone_into(&mut host.address);
    host.groups = groups.iter().map(|group| (*group).to_owned()).collect();
    host.vars = vars.as_object().cloned().unwrap_or_default();
    if state != HostState::Pending {
        host.check.result = Some(CheckResult {
            output: format!(
                "PING {}",
                if state == HostState::Up {
                    "OK"
                } else {
                    "CRITICAL"
                }
            ),
            exit_status: if state == HostState::Up { 0 } else { 2 },
            ..CheckResult::default()
        });
    }
    if state == HostState::Unreachable {
        host.check.reachable = false;
    }
    host
}

struct ServiceSpec<'a> {
    host: &'a str,
    name: &'a str,
    state: ServiceState,
    state_type: StateType,
    output: &'a str,
    since_minutes: f64,
}

fn service(spec: &ServiceSpec<'_>) -> Service {
    let mut service = Service::new(spec.host, spec.name);
    service.state = spec.state;
    service.check.state_type = spec.state_type;
    service.check.last_state_change = Timestamp::from_unix_seconds(NOW - spec.since_minutes * 60.0);
    if spec.state != ServiceState::Pending {
        service.check.result = Some(CheckResult {
            output: spec.output.to_owned(),
            exit_status: match spec.state {
                ServiceState::Ok | ServiceState::Pending => 0,
                ServiceState::Warning => 1,
                ServiceState::Critical => 2,
                ServiceState::Unknown => 3,
            },
            ..CheckResult::default()
        });
    }
    service
}

/// A small production cluster, like the design's sample data.
struct Cluster {
    hosts: BTreeMap<String, Host>,
    services: Vec<Service>,
}

#[expect(
    clippy::too_many_lines,
    reason = "fixture data, one object per line before formatting"
)]
fn cluster() -> Cluster {
    let hosts = [
        host(
            "db-prod-01",
            HostState::Up,
            "10.0.4.1",
            &["databases", "linux"],
            &json!({ "role": "postgres", "tags": ["primary"] }),
        ),
        host(
            "db-prod-03",
            HostState::Up,
            "10.0.4.3",
            &["databases", "linux"],
            &json!({ "role": "postgres", "disks": { "/var": { "warn": "80%" } } }),
        ),
        host(
            "mq-prod-01",
            HostState::Up,
            "10.0.5.1",
            &["queues", "linux"],
            &json!({ "role": "rabbitmq" }),
        ),
        host(
            "k8s-node-07",
            HostState::Down,
            "10.0.9.7",
            &["kubernetes"],
            &json!({}),
        ),
        host(
            "k8s-pod-a",
            HostState::Unreachable,
            "10.0.9.70",
            &["kubernetes"],
            &json!({}),
        ),
        host(
            "web-01",
            HostState::Up,
            "192.168.1.10",
            &["web"],
            &json!({ "role": "web" }),
        ),
        host("new-host", HostState::Pending, "", &[], &json!({})),
    ];
    let critical = ServiceState::Critical;
    let hard = StateType::Hard;
    let specs = [
        ServiceSpec {
            host: "db-prod-03",
            name: "postgres-replication",
            state: critical,
            state_type: hard,
            output: "CRITICAL - lag 412s",
            since_minutes: 14.0,
        },
        ServiceSpec {
            host: "db-prod-03",
            name: "pg_connections",
            state: ServiceState::Warning,
            state_type: StateType::Soft,
            output: "WARNING - 180 connections",
            since_minutes: 2.0,
        },
        ServiceSpec {
            host: "db-prod-01",
            name: "pg_backup",
            state: critical,
            state_type: hard,
            output: "CRITICAL - last backup 3d ago",
            since_minutes: 300.0,
        },
        ServiceSpec {
            host: "db-prod-01",
            name: "pg_main",
            state: ServiceState::Ok,
            state_type: hard,
            output: "OK",
            since_minutes: 9000.0,
        },
        ServiceSpec {
            host: "mq-prod-01",
            name: "rabbitmq-queue",
            state: ServiceState::Warning,
            state_type: hard,
            output: "WARNING - 12000 messages",
            since_minutes: 61.0,
        },
        ServiceSpec {
            host: "k8s-node-07",
            name: "kubelet",
            state: critical,
            state_type: hard,
            output: "CRITICAL - connection refused",
            since_minutes: 30.0,
        },
        ServiceSpec {
            host: "k8s-pod-a",
            name: "http",
            state: ServiceState::Unknown,
            state_type: hard,
            output: "UNKNOWN - no route",
            since_minutes: 30.0,
        },
        ServiceSpec {
            host: "web-01",
            name: "http",
            state: ServiceState::Ok,
            state_type: hard,
            output: "HTTP OK",
            since_minutes: 500.0,
        },
        ServiceSpec {
            host: "web-01",
            name: "disk",
            state: critical,
            state_type: hard,
            output: "DISK CRITICAL - / 97%",
            since_minutes: 45.0,
        },
        ServiceSpec {
            host: "new-host",
            name: "ping",
            state: ServiceState::Pending,
            state_type: hard,
            output: "",
            since_minutes: 0.0,
        },
    ];
    let mut services: Vec<Service> = specs.iter().map(service).collect();
    for service in &mut services {
        // Services behind a host problem fail their dependency.
        if service.key.host.as_str().starts_with("k8s-") {
            service.check.reachable = false;
        }
        match service.key.to_string().as_str() {
            "db-prod-01!pg_backup" => service.check.acknowledgement = AckKind::Normal,
            "web-01!disk" => service.check.downtime_depth = 1,
            "db-prod-03!postgres-replication" => {
                service.vars = json!({ "team": "dba" })
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                service.groups = vec!["postgres".to_owned()];
            }
            _ => {}
        }
    }
    Cluster {
        hosts: hosts
            .into_iter()
            .map(|host| (host.name.to_string(), host))
            .collect(),
        services,
    }
}

impl Cluster {
    /// Full names of the services the filter matches, with extra scopes
    /// (filter vars) after the service scope.
    fn matching_services(
        &self,
        source: &str,
        extra: &[&dyn Scope],
    ) -> Result<Vec<String>, ParseError> {
        let filter = Filter::parse(source)?;
        let mut names: Vec<String> = self
            .services
            .iter()
            .filter(|service| {
                let service_scope = ServiceScope {
                    service,
                    host: self.hosts.get(service.key.host.as_str()),
                };
                let mut scopes: Vec<&dyn Scope> = vec![&service_scope];
                scopes.extend_from_slice(extra);
                filter.matches_at(
                    &Chain { scopes: &scopes },
                    Timestamp::from_unix_seconds(NOW),
                )
            })
            .map(|service| service.key.full_name())
            .collect();
        names.sort();
        Ok(names)
    }

    fn matching_hosts(&self, source: &str) -> Result<Vec<String>, ParseError> {
        let filter = Filter::parse(source)?;
        Ok(self
            .hosts
            .values()
            .filter(|host| filter.matches(&HostScope { host }))
            .map(|host| host.name.to_string())
            .collect())
    }
}

/// Checks which services each filter matches.
fn check_services(cases: &[(&str, &[&str])]) -> Result<(), ParseError> {
    let cluster = cluster();
    for (source, expected) in cases {
        assert_eq!(
            cluster.matching_services(source, &[])?,
            *expected,
            "{source}"
        );
    }
    Ok(())
}

#[test]
fn dashboard_service_filters() {
    check_services(&[
        (
            r#"host.vars.role == "postgres" && service.state != 0"#,
            &[
                "db-prod-01!pg_backup",
                "db-prod-03!pg_connections",
                "db-prod-03!postgres-replication",
            ],
        ),
        (
            r#"match("pg_*", service.name)"#,
            &[
                "db-prod-01!pg_backup",
                "db-prod-01!pg_main",
                "db-prod-03!pg_connections",
            ],
        ),
        (
            r#""databases" in host.groups"#,
            &[
                "db-prod-01!pg_backup",
                "db-prod-01!pg_main",
                "db-prod-03!pg_connections",
                "db-prod-03!postgres-replication",
            ],
        ),
        (
            "!service.handled && service.state_type == 1",
            &[
                "db-prod-01!pg_main",
                "db-prod-03!postgres-replication",
                "mq-prod-01!rabbitmq-queue",
                "new-host!ping",
                "web-01!http",
            ],
        ),
        (
            "service.problem && !service.handled && service.state_type == 1",
            &[
                "db-prod-03!postgres-replication",
                "mq-prod-01!rabbitmq-queue",
            ],
        ),
        (
            "service.state == ServiceCritical",
            &[
                "db-prod-01!pg_backup",
                "db-prod-03!postgres-replication",
                "k8s-node-07!kubelet",
                "web-01!disk",
            ],
        ),
        // As in Icinga, a pending host has state 1 (DOWN) but is no problem.
        (
            "host.state == HostDown",
            &["k8s-node-07!kubelet", "k8s-pod-a!http", "new-host!ping"],
        ),
        ("host.problem", &["k8s-node-07!kubelet", "k8s-pod-a!http"]),
        // Pending services have state 3 (UNKNOWN) but are no problem.
        (
            "service.state == ServiceUnknown",
            &["k8s-pod-a!http", "new-host!ping"],
        ),
        (
            "service.state == ServiceUnknown && service.problem",
            &["k8s-pod-a!http"],
        ),
        (
            "host.state == HostDown && !host.last_reachable",
            &["k8s-pod-a!http"],
        ),
    ])
    .unwrap();
}

#[test]
fn more_service_filters() {
    check_services(&[
        (
            r#"regex("^db-prod-\\d+$", host.name)"#,
            &[
                "db-prod-01!pg_backup",
                "db-prod-01!pg_main",
                "db-prod-03!pg_connections",
                "db-prod-03!postgres-replication",
            ],
        ),
        (
            r#"cidr_match("192.168.0.0/16", host.address)"#,
            &["web-01!disk", "web-01!http"],
        ),
        (
            r#"service.last_check_result.output.contains("lag")"#,
            &["db-prod-03!postgres-replication"],
        ),
        (
            r#"host.vars.disks["/var"].warn == "80%""#,
            &[
                "db-prod-03!pg_connections",
                "db-prod-03!postgres-replication",
            ],
        ),
        (
            r#"service.vars.team in ["dba", "sre"]"#,
            &["db-prod-03!postgres-replication"],
        ),
        (
            r#""postgres" in service.groups"#,
            &["db-prod-03!postgres-replication"],
        ),
        (
            r#"host.vars.nonexistent == "x" || host.vars.tags[0] == "primary""#,
            &["db-prod-01!pg_backup", "db-prod-01!pg_main"],
        ),
        ("service.acknowledgement != 0", &["db-prod-01!pg_backup"]),
        ("service.downtime_depth > 0", &["web-01!disk"]),
        (
            "service.severity >= 2048",
            &[
                "db-prod-03!pg_connections",
                "db-prod-03!postgres-replication",
                "mq-prod-01!rabbitmq-queue",
            ],
        ),
        (
            "get_time() - service.last_state_change > 1h && service.state != 0",
            &["db-prod-01!pg_backup", "mq-prod-01!rabbitmq-queue"],
        ),
        (
            r#"match("linux", host.groups, MatchAny) && service.state == ServiceWarning"#,
            &["db-prod-03!pg_connections", "mq-prod-01!rabbitmq-queue"],
        ),
        (
            r#"service.host_name == "web-01" && service.name == "http""#,
            &["web-01!http"],
        ),
        (
            r#"service.host.vars.role == "rabbitmq""#,
            &["mq-prod-01!rabbitmq-queue"],
        ),
        ("service.last_check_result == null", &["new-host!ping"]),
        (
            "(\n  host.vars.role == \"postgres\" &&\n  service.state_type == 1 # hard states only\n)",
            &[
                "db-prod-01!pg_backup",
                "db-prod-01!pg_main",
                "db-prod-03!postgres-replication",
            ],
        ),
        (
            "",
            &[
                "db-prod-01!pg_backup",
                "db-prod-01!pg_main",
                "db-prod-03!pg_connections",
                "db-prod-03!postgres-replication",
                "k8s-node-07!kubelet",
                "k8s-pod-a!http",
                "mq-prod-01!rabbitmq-queue",
                "new-host!ping",
                "web-01!disk",
                "web-01!http",
            ],
        ),
    ])
    .unwrap();
}

#[test]
fn api_filters_with_filter_vars() {
    // The filters ic-api sends with actions, with `filter_vars`.
    let cluster = cluster();
    let vars = BTreeMap::from([(
        "names".to_owned(),
        Value::from(vec![
            Value::from("web-01!http"),
            Value::from("db-prod-03!postgres-replication"),
            Value::from("nope!x"),
        ]),
    )]);
    let filter_vars = VarsScope { vars: &vars };
    assert_eq!(
        cluster
            .matching_services("service.__name in names", &[&filter_vars])
            .unwrap(),
        ["db-prod-03!postgres-replication", "web-01!http"]
    );
    let host_vars = BTreeMap::from([(
        "names".to_owned(),
        Value::from(vec![Value::from("web-01"), Value::from("k8s-node-07")]),
    )]);
    let filter = Filter::parse("host.name in names").unwrap();
    let mut hosts: Vec<String> = cluster
        .hosts
        .values()
        .filter(|host| {
            let host_scope = HostScope { host };
            let vars_scope = VarsScope { vars: &host_vars };
            filter.matches(&Chain {
                scopes: &[&host_scope, &vars_scope],
            })
        })
        .map(|host| host.name.to_string())
        .collect();
    hosts.sort();
    assert_eq!(hosts, ["k8s-node-07", "web-01"]);
    // The API documentation's example with two filter variables.
    let vars = BTreeMap::from([
        ("state".to_owned(), Value::Number(2.0)),
        ("pattern".to_owned(), Value::from("pg*")),
    ]);
    let filter_vars = VarsScope { vars: &vars };
    assert_eq!(
        cluster
            .matching_services(
                "service.state==state && match(pattern,service.name)",
                &[&filter_vars]
            )
            .unwrap(),
        ["db-prod-01!pg_backup"]
    );
}

#[test]
fn host_dashboards() {
    let cluster = cluster();
    let cases: &[(&str, &[&str])] = &[
        (
            "host.state != 0 && !host.handled",
            &["k8s-node-07", "k8s-pod-a", "new-host"],
        ),
        (
            "host.problem && !host.handled",
            &["k8s-node-07", "k8s-pod-a"],
        ),
        (
            r#""linux" in host.groups && host.vars.role == "postgres""#,
            &["db-prod-01", "db-prod-03"],
        ),
        ("host.last_check_result == null", &["new-host"]),
        (
            r#"cidr_match("10.0.9.0/24", host.address)"#,
            &["k8s-node-07", "k8s-pod-a"],
        ),
        (r#"obj.name == "web-01""#, &["web-01"]),
        (
            "len(host.groups) > 1",
            &["db-prod-01", "db-prod-03", "mq-prod-01"],
        ),
        (r#"host.vars.tags.contains("primary")"#, &["db-prod-01"]),
        // Hosts without `tags` don't contain anything, so the negation and
        // other alternatives still work for them.
        (
            r#"!host.vars.tags.contains("primary") && host.vars.role"#,
            &["db-prod-03", "mq-prod-01", "web-01"],
        ),
        (
            r#"host.vars.tags.contains("primary") || host.name == "web-01""#,
            &["db-prod-01", "web-01"],
        ),
        (
            "host.vars == null",
            &["k8s-node-07", "k8s-pod-a", "new-host"],
        ),
    ];
    for (source, expected) in cases {
        assert_eq!(
            cluster.matching_hosts(source).unwrap(),
            *expected,
            "{source}"
        );
    }
}

#[test]
fn missing_check_results_are_empty_not_errors() {
    let cluster = cluster();
    let filter = Filter::parse(r#"service.last_check_result.output.contains("OK")"#).unwrap();
    let pending = cluster
        .services
        .iter()
        .find(|service| service.state == ServiceState::Pending)
        .unwrap();
    let scope = ServiceScope {
        service: pending,
        host: cluster.hosts.get(pending.key.host.as_str()),
    };
    assert_eq!(filter.evaluate(&scope), Ok(Value::Bool(false)));
    assert_eq!(
        cluster
            .matching_services(r#"service.last_check_result.output.contains("OK")"#, &[])
            .unwrap(),
        ["db-prod-01!pg_main", "web-01!http"]
    );
    assert_eq!(
        cluster
            .matching_services(r#"!service.last_check_result.output.contains("OK")"#, &[])
            .unwrap()
            .len(),
        8,
        "the pending service's output doesn't contain OK either"
    );
}

#[test]
fn errors_on_some_objects_only_exclude_those() {
    let cluster = cluster();
    // `number()` fails for outputs that aren't numbers, which is all but
    // one of them here; only the objects it fails on are excluded.
    let mut cluster = cluster;
    if let Some(result) = cluster
        .services
        .iter_mut()
        .find(|service| service.key.full_name() == "web-01!http")
        .and_then(|service| service.check.result.as_mut())
    {
        "200".clone_into(&mut result.output);
    }
    let source = "number(service.last_check_result.output) >= 200 || service.name == \"disk\"";
    let filter = Filter::parse(source).unwrap();
    let failing = &cluster.services[0];
    let scope = ServiceScope {
        service: failing,
        host: cluster.hosts.get(failing.key.host.as_str()),
    };
    assert_eq!(
        filter.evaluate(&scope).unwrap_err().message,
        "can't convert 'CRITICAL - lag 412s' to a floating point number \
         (in `number(service.last_check_result.output)`)"
    );
    assert!(!filter.matches(&scope));
    assert_eq!(
        cluster.matching_services(source, &[]).unwrap(),
        ["web-01!http"],
        "web-01!disk fails in number() before its name is compared"
    );
}

#[test]
fn evaluation_returns_values_not_just_booleans() {
    let cluster = cluster();
    let service = &cluster.services[0];
    let scope = ServiceScope {
        service,
        host: cluster.hosts.get(service.key.host.as_str()),
    };
    let value = |source: &str| Filter::parse(source).unwrap().evaluate(&scope).unwrap();
    assert_eq!(
        value("service.__name"),
        Value::from("db-prod-03!postgres-replication")
    );
    assert_eq!(value("host.vars.role || \"none\""), Value::from("postgres"));
    assert_eq!(value("host.vars.missing || \"none\""), Value::from("none"));
    assert_eq!(
        value("service.check_attempt + \"/\" + service.max_check_attempts"),
        Value::from("1/1")
    );
    assert_eq!(
        value("host.groups"),
        Value::from(vec![Value::from("databases"), Value::from("linux")])
    );
}
