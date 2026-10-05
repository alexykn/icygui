//! `lab`: nearly empty, one host still pending.

use ic_model::{Host, HostState, Service, ServiceState};
use serde_json::json;

use super::Scenario;
use super::build::{Builder, vars};

/// Builds the `lab` scenario: one host up, one host never checked.
#[must_use]
pub fn lab() -> Scenario {
    let mut b = Builder::new("lab", "lab-icinga", "r2.14.3-1", 5);
    b.endpoint("lab-icinga", "master", false);
    b.host(
        "lab-01",
        "192.168.56.11",
        &["linux-servers"],
        vars(&[("role", json!("sandbox")), ("env", json!("lab"))]),
    );
    for (service, command) in [
        ("ping4", "ping4"),
        ("ssh", "ssh"),
        ("disk /", "disk"),
        ("load", "load"),
    ] {
        b.service(
            "lab-01",
            service,
            command,
            &[],
            vars(&[("env", json!("lab"))]),
        );
    }
    // A host that was just added: no check result yet, and passive only, so
    // it stays pending until someone submits a result.
    let mut pending = Host::new("lab-02");
    "192.168.56.12".clone_into(&mut pending.address);
    pending.groups = vec!["linux-servers".to_owned()];
    pending.vars = vars(&[("role", json!("sandbox")), ("env", json!("lab"))]);
    pending.state = HostState::Pending;
    "hostalive".clone_into(&mut pending.check.check_command);
    pending.check.max_attempts = 3;
    pending.check.check_interval = 60.0;
    pending.check.features.active_checks = false;
    b.scenario.hosts.push(pending);
    let mut service = Service::new("lab-02", "ping4");
    service.state = ServiceState::Pending;
    "ping4".clone_into(&mut service.check.check_command);
    service.check.max_attempts = 3;
    service.check.check_interval = 60.0;
    service.check.features.active_checks = false;
    b.scenario.services.push(service);
    b.finish()
}
