//! Evaluation speed over a large installation: 2 000 hosts with 10 services
//! each. Ignored by default; run with
//! `cargo test -p ic-filter --release --test performance -- --ignored --nocapture`.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ic_filter::{Chain, Filter, Scope, ServiceScope, Value, VarsScope};
use ic_model::{CheckResult, Host, HostState, Service, ServiceState, StateType};
use serde_json::json;

const HOSTS: usize = 2_000;
const SERVICES_PER_HOST: usize = 10;

/// The budget per filter evaluation over all services: "well under 50 ms"
/// in release builds, with plenty of room for unoptimised test builds.
fn budget() -> Duration {
    if cfg!(debug_assertions) {
        Duration::from_secs(1)
    } else {
        Duration::from_millis(50)
    }
}

/// A small deterministic generator, so runs are comparable.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let bound = u64::try_from(bound).unwrap_or(u64::MAX);
        usize::try_from((self.0 >> 33) % bound).unwrap_or(0)
    }
}

fn installation() -> (BTreeMap<String, Host>, Vec<Service>) {
    let mut random = Lcg(42);
    let roles = ["postgres", "web", "rabbitmq", "k8s", "redis"];
    let service_names = [
        "ping",
        "ssh",
        "disk",
        "load",
        "memory",
        "ntp",
        "pg_main",
        "pg_backup",
        "http",
        "procs",
    ];
    let mut hosts = BTreeMap::new();
    let mut services = Vec::with_capacity(HOSTS * SERVICES_PER_HOST);
    for index in 0..HOSTS {
        let role = roles[random.next(roles.len())];
        let name = format!("{role}-{index:04}");
        let mut host = Host::new(&name);
        host.state = if random.next(50) == 0 {
            HostState::Down
        } else {
            HostState::Up
        };
        host.address = format!(
            "10.{}.{}.{}",
            random.next(256),
            random.next(256),
            random.next(256)
        );
        host.groups = vec![
            if role == "postgres" {
                "databases"
            } else {
                "servers"
            }
            .to_owned(),
            "linux".to_owned(),
        ];
        host.vars = json!({
            "role": role,
            "os": "Linux",
            "disks": { "/": { "warn": "80%" }, "/var": { "warn": "90%" } },
            "tags": ["prod", role, format!("rack-{}", random.next(40))],
        })
        .as_object()
        .cloned()
        .unwrap_or_default();
        host.check.result = Some(CheckResult::default());
        for service_name in service_names.iter().take(SERVICES_PER_HOST) {
            let mut service = Service::new(&name, service_name);
            service.state = match random.next(20) {
                0 => ServiceState::Critical,
                1 => ServiceState::Warning,
                2 => ServiceState::Unknown,
                _ => ServiceState::Ok,
            };
            service.check.state_type = if random.next(4) == 0 {
                StateType::Soft
            } else {
                StateType::Hard
            };
            service.check.result = Some(CheckResult {
                output: format!("{service_name} output {}", random.next(1_000)),
                ..CheckResult::default()
            });
            service.vars = json!({ "team": if random.next(2) == 0 { "dba" } else { "sre" } })
                .as_object()
                .cloned()
                .unwrap_or_default();
            services.push(service);
        }
        hosts.insert(name, host);
    }
    (hosts, services)
}

#[test]
#[ignore = "benchmark; run with --release -- --ignored"]
fn typical_dashboard_filters_over_20k_services() {
    let (hosts, services) = installation();
    assert_eq!(services.len(), 20_000);
    let names: Vec<Value> = services
        .iter()
        .step_by(200)
        .map(|service| Value::from(service.key.full_name()))
        .collect();
    let vars = BTreeMap::from([("names".to_owned(), Value::from(names))]);
    let filter_vars = VarsScope { vars: &vars };

    let filters = [
        r#"host.vars.role == "postgres" && service.state != 0"#,
        r#"match("pg_*", service.name)"#,
        r#""databases" in host.groups"#,
        "service.__name in names",
        "!service.handled && service.state_type == 1",
        r#"regex("^postgres-\\d+$", host.name) && service.state == ServiceCritical"#,
        r#"cidr_match("10.128.0.0/9", host.address)"#,
        r#"host.vars.disks["/var"].warn == "90%" && "rack-7" in host.vars.tags"#,
        r#"service.problem && !service.handled && service.vars.team in ["dba", "sre"] && match("*output 9*", service.last_check_result.output)"#,
    ];
    let mut slowest = Duration::ZERO;
    for source in filters {
        let filter = Filter::parse(source).unwrap();
        let run = || {
            services
                .iter()
                .filter(|service| {
                    let scope = ServiceScope {
                        service,
                        host: hosts.get(service.key.host.as_str()),
                    };
                    let scopes: [&dyn Scope; 2] = [&scope, &filter_vars];
                    filter.matches(&Chain { scopes: &scopes })
                })
                .count()
        };
        let matched = run();
        let best = (0..5)
            .map(|_| {
                let start = Instant::now();
                assert_eq!(run(), matched);
                start.elapsed()
            })
            .min()
            .unwrap_or_default();
        eprintln!("{best:>10.2?}  {matched:>6} matches  {source}");
        slowest = slowest.max(best);
    }
    assert!(
        slowest < budget(),
        "slowest filter took {slowest:?}, budget {:?}",
        budget()
    );
}
