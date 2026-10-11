//! Counting objects into a [`Summary`]: the snapshot's `overall` (tray icon
//! and tooltip) and every dashboard's summary bar.

use ic_model::{CheckableState, Host, HostState, Service, ServiceState};

use crate::snapshot::Summary;

/// Accumulates a [`Summary`] over hosts and services: the one place that
/// decides how objects count. The worst unhandled problem is the one with
/// the highest [`CheckableState::severity_rank`], the order every dot in
/// the app follows (the sidebar, the tray, view headers, group headers), so
/// a down host outranks an unknown service even though Icinga's severity
/// weighs host and service states on different scales. The demo's fixture
/// uses it too.
#[derive(Debug, Default)]
pub struct Tally {
    summary: Summary,
    /// The worst unhandled problem so far.
    worst: Option<CheckableState>,
}

impl Tally {
    /// Counts a host.
    pub fn add_host(&mut self, host: &Host) {
        self.add(CheckableState::Host(host.state), host.is_handled());
    }

    /// Counts a service; `host` decides whether a problem is handled by a
    /// host problem (Icinga's `handled`).
    pub fn add_service(&mut self, service: &Service, host: Option<&Host>) {
        let host_problem = host.is_some_and(Host::is_problem);
        self.add(
            CheckableState::Service(service.state),
            service.is_handled(host_problem),
        );
    }

    /// Counts an object in `state`; `handled` only matters for problems.
    pub fn add(&mut self, state: CheckableState, handled: bool) {
        let counter = match state {
            CheckableState::Host(HostState::Up) | CheckableState::Service(ServiceState::Ok) => {
                &mut self.summary.ok
            }
            CheckableState::Host(HostState::Down) => &mut self.summary.down,
            CheckableState::Host(HostState::Unreachable) => &mut self.summary.unreachable,
            CheckableState::Host(HostState::Pending)
            | CheckableState::Service(ServiceState::Pending) => &mut self.summary.pending,
            CheckableState::Service(ServiceState::Warning) => &mut self.summary.warning,
            CheckableState::Service(ServiceState::Critical) => &mut self.summary.critical,
            CheckableState::Service(ServiceState::Unknown) => &mut self.summary.unknown,
        };
        *counter += 1;
        if state.is_problem() {
            self.problem(handled, state);
        }
    }

    fn problem(&mut self, handled: bool, state: CheckableState) {
        if handled {
            self.summary.handled += 1;
            return;
        }
        self.summary.unhandled += 1;
        if self
            .worst
            .is_none_or(|worst| state.severity_rank() > worst.severity_rank())
        {
            self.worst = Some(state);
        }
    }

    /// The summary.
    pub fn finish(mut self) -> Summary {
        self.summary.worst_unhandled = self.worst;
        self.summary
    }
}

#[cfg(test)]
mod tests {
    use ic_model::AckKind;

    use super::*;

    fn host(name: &str, state: HostState) -> Host {
        let mut host = Host::new(name);
        host.state = state;
        host
    }

    fn service(host: &str, state: ServiceState) -> Service {
        let mut service = Service::new(host, "s");
        service.state = state;
        service
    }

    /// Services (with their host, if any) and the worst state they make.
    type Case<'h> = (Vec<(Option<&'h Host>, Service)>, CheckableState);

    /// The worst unhandled problem is the reddest state
    /// ([`CheckableState::severity_rank`]), whatever Icinga's severity says
    /// and in any counting order (review of 025864c: a down host lost to an
    /// unknown service, which Icinga weighs 2112 against 2080).
    #[test]
    fn the_worst_is_the_reddest_state_in_any_order() {
        let up = host("up", HostState::Up);
        let down = host("down", HostState::Down);
        let mut behind_dependency = service("up", ServiceState::Critical);
        behind_dependency.check.reachable = false;
        assert!(
            behind_dependency.severity() < service("up", ServiceState::Warning).severity(),
            "Icinga weighs a critical behind a failed dependency below a warning"
        );
        let cases: [Case<'_>; 3] = [
            (
                vec![(None, service("up", ServiceState::Warning))],
                CheckableState::Host(HostState::Down),
            ),
            (
                vec![(Some(&up), service("up", ServiceState::Unknown))],
                CheckableState::Host(HostState::Down),
            ),
            (
                vec![
                    (Some(&up), service("up", ServiceState::Warning)),
                    (Some(&up), behind_dependency.clone()),
                ],
                CheckableState::Service(ServiceState::Critical),
            ),
        ];
        for (index, (services, expected)) in cases.into_iter().enumerate() {
            let with_down = index < 2;
            for down_first in [true, false] {
                let mut tally = Tally::default();
                if with_down && down_first {
                    tally.add_host(&down);
                }
                for (host, service) in &services {
                    tally.add_service(service, *host);
                }
                if with_down && !down_first {
                    tally.add_host(&down);
                }
                assert_eq!(
                    tally.finish().worst_unhandled,
                    Some(expected),
                    "case {index}"
                );
            }
        }
    }

    #[test]
    fn counts_states_and_handling() {
        let up = host("up", HostState::Up);
        let down = host("down", HostState::Down);
        let mut acked = host("acked", HostState::Unreachable);
        acked.check.acknowledgement = AckKind::Normal;
        let mut tally = Tally::default();
        for host in [&up, &down, &acked, &host("new", HostState::Pending)] {
            tally.add_host(host);
        }
        tally.add_service(&service("up", ServiceState::Ok), Some(&up));
        tally.add_service(&service("up", ServiceState::Warning), Some(&up));
        // Handled by its host's problem.
        tally.add_service(&service("down", ServiceState::Critical), Some(&down));
        let mut in_downtime = service("up", ServiceState::Unknown);
        in_downtime.check.downtime_depth = 1;
        tally.add_service(&in_downtime, Some(&up));
        tally.add_service(&service("gone", ServiceState::Pending), None);

        let summary = tally.finish();
        assert_eq!(
            summary,
            Summary {
                critical: 1,
                warning: 1,
                unknown: 1,
                down: 1,
                unreachable: 1,
                ok: 2,
                pending: 2,
                handled: 3,
                unhandled: 2,
                // An unhandled down host is redder than a warning.
                worst_unhandled: Some(CheckableState::Host(HostState::Down)),
            }
        );
    }

    #[test]
    fn critical_is_the_worst_service_state() {
        let up = host("up", HostState::Up);
        let mut tally = Tally::default();
        tally.add_service(&service("up", ServiceState::Warning), Some(&up));
        tally.add_service(&service("up", ServiceState::Critical), Some(&up));
        tally.add_service(&service("up", ServiceState::Unknown), Some(&up));
        assert_eq!(
            tally.finish().worst_unhandled,
            Some(CheckableState::Service(ServiceState::Critical))
        );
        assert_eq!(Tally::default().finish(), Summary::default());
    }
}
