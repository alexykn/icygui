//! `staging`: a small environment with three warnings.

use std::time::Duration;

use ic_model::ServiceState;
use serde_json::json;

use super::Scenario;
use super::build::{Builder, vars};

/// Builds the `staging` scenario: ten hosts, three warnings.
#[must_use]
pub fn staging() -> Scenario {
    let mut b = Builder::new("staging", "stg-master-01", "r2.14.3-1", 77);
    b.endpoint("stg-master-01", "master", false);
    let hosts: &[(&str, &str, &str, &[&str])] = &[
        ("stg-web-01", "10.20.1.11", "web", &["web-frontend"]),
        ("stg-web-02", "10.20.1.12", "web", &["web-frontend"]),
        ("stg-api-01", "10.20.3.11", "api", &["api"]),
        ("stg-api-02", "10.20.3.12", "api", &["api"]),
        ("stg-db-01", "10.20.2.11", "postgres", &["databases"]),
        ("stg-mq-01", "10.20.5.11", "rabbitmq", &["queue"]),
        ("stg-cache-01", "10.20.7.11", "redis", &["cache"]),
        ("stg-k8s-01", "10.20.4.11", "k8s-node", &["kubernetes"]),
        ("stg-k8s-02", "10.20.4.12", "k8s-node", &["kubernetes"]),
        ("stg-lb-01", "10.20.0.11", "haproxy", &["loadbalancers"]),
    ];
    for (name, address, role, groups) in hosts {
        let mut all_groups = groups.to_vec();
        all_groups.push("linux-servers");
        b.host(
            name,
            address,
            &all_groups,
            vars(&[("role", json!(role)), ("env", json!("staging"))]),
        );
        let service_vars = vars(&[("env", json!("staging"))]);
        for (service, command) in [
            ("ping4", "ping4"),
            ("ssh", "ssh"),
            ("load", "load"),
            ("disk /", "disk"),
            ("memory", "mem"),
            ("apt", "apt"),
        ] {
            b.service(name, service, command, &[], service_vars.clone());
        }
        let extra: &[(&str, &str)] = match *role {
            "web" | "api" => &[("http", "http")],
            "postgres" => &[
                ("pg-connections", "check_postgres"),
                ("postgres-replication", "check_postgres"),
            ],
            "rabbitmq" => &[("rabbitmq-queue", "rabbitmq")],
            "redis" => &[("redis-memory", "redis")],
            "k8s-node" => &[("kubelet", "kubelet")],
            "haproxy" => &[("haproxy-backend", "haproxy")],
            _ => &[],
        };
        for (service, command) in extra {
            b.service(name, service, command, &[], service_vars.clone());
        }
    }
    b.generated_problem(
        "stg-web-01",
        "apt",
        ServiceState::Warning,
        Duration::from_hours(5),
    );
    b.generated_problem(
        "stg-db-01",
        "disk /",
        ServiceState::Warning,
        Duration::from_mins(47),
    );
    b.problem(
        "stg-db-01",
        "pg-connections",
        ServiceState::Warning,
        "WARNING - 91 of 100 connections used",
        &["connections=91;80;95;0;100"],
        Duration::from_mins(13),
        None,
    );
    b.finish()
}
