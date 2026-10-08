//! The demo's dashboard filters and overall counts. Dashboards are
//! evaluated by `ic_core::evaluate_dashboard`, exactly as the engine does.

use ic_core::Tally;
use ic_core::snapshot::{Snapshot, Summary};

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

/// Every host's and service's counts, as the core's `Snapshot::overall`
/// (the same `Tally`).
pub(super) fn overall(snapshot: &Snapshot) -> Summary {
    let mut tally = Tally::default();
    for host in snapshot.hosts.values() {
        tally.add_host(host);
    }
    for service in snapshot.services.values() {
        tally.add_service(service, snapshot.host_of(&service.key).map(AsRef::as_ref));
    }
    tally.finish()
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
