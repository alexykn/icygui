//! The demo's dashboard filters and overall counts. Dashboards are
//! evaluated by `ic_core::evaluate_dashboard`, exactly as the engine does.

use ic_core::snapshot::{Snapshot, Summary};
use ic_model::{CheckableState, Host, HostState, Service, ServiceState};

/// Which objects a demo dashboard matches, as a filter expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DemoFilter {
    /// The empty filter: everything.
    All,
    /// `host.vars.env == "…"`.
    Env(&'static str),
    /// `host.vars.role in [...]`.
    Roles(&'static [&'static str]),
    /// Certificate checks.
    Certificates,
    /// `service.vars.deploy_check == true`.
    DeployChecks,
}

impl DemoFilter {
    /// The filter expression the dashboard shows.
    pub(crate) fn expression(self) -> String {
        match self {
            Self::All => String::new(),
            Self::Env(env) => format!("host.vars.env == \"{env}\""),
            Self::Roles(roles) => {
                let quoted: Vec<_> = roles.iter().map(|role| format!("\"{role}\"")).collect();
                format!("host.vars.role in [{}]", quoted.join(", "))
            }
            Self::Certificates => {
                "match(\"*cert*\", service.name) || service.name == \"http-tls\"".to_owned()
            }
            Self::DeployChecks => "service.vars.deploy_check == true".to_owned(),
        }
    }
}

/// What the overall counts need of a host or service.
struct Match {
    state: CheckableState,
    problem: bool,
    handled: bool,
    severity: u32,
}

impl Match {
    fn host(host: &Host) -> Self {
        Self {
            state: CheckableState::Host(host.state),
            problem: host.is_problem(),
            handled: host.counts_as_handled(),
            severity: host.severity(),
        }
    }

    fn service(service: &Service, host: Option<&Host>) -> Self {
        let host_problem = host.is_some_and(Host::is_problem);
        Self {
            state: CheckableState::Service(service.state),
            problem: service.is_problem(),
            handled: service.counts_as_handled(host_problem),
            severity: service.severity(),
        }
    }
}

/// Every host's and service's counts, as the core's `Snapshot::overall`.
pub(super) fn overall(snapshot: &Snapshot) -> Summary {
    let matches: Vec<Match> = snapshot
        .hosts
        .values()
        .map(|host| Match::host(host))
        .chain(snapshot.services.values().map(|service| {
            Match::service(service, snapshot.host_of(&service.key).map(AsRef::as_ref))
        }))
        .collect();
    summarize(&matches)
}

fn summarize(matches: &[Match]) -> Summary {
    let mut summary = Summary::default();
    let mut worst: Option<&Match> = None;
    for object in matches {
        let counter = match object.state {
            CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up) => {
                &mut summary.ok
            }
            CheckableState::Service(ServiceState::Warning) => &mut summary.warning,
            CheckableState::Service(ServiceState::Critical) => &mut summary.critical,
            CheckableState::Service(ServiceState::Unknown) => &mut summary.unknown,
            CheckableState::Host(HostState::Down) => &mut summary.down,
            CheckableState::Host(HostState::Unreachable) => &mut summary.unreachable,
            CheckableState::Service(ServiceState::Pending)
            | CheckableState::Host(HostState::Pending) => &mut summary.pending,
        };
        *counter += 1;
        if !object.problem {
            continue;
        }
        if object.handled {
            summary.handled += 1;
        } else {
            summary.unhandled += 1;
            if worst.is_none_or(|worst| object.severity > worst.severity) {
                worst = Some(object);
            }
        }
    }
    summary.worst_unhandled = worst.map(|object| object.state);
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_render_as_icinga_expressions() {
        assert_eq!(DemoFilter::All.expression(), "");
        assert_eq!(
            DemoFilter::Roles(&["switch", "edge"]).expression(),
            "host.vars.role in [\"switch\", \"edge\"]"
        );
        assert_eq!(
            DemoFilter::Env("prod").expression(),
            "host.vars.env == \"prod\""
        );
    }
}
