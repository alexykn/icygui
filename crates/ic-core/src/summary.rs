//! Counting objects into a [`Summary`]: the snapshot's `overall` (tray icon
//! and tooltip) and every dashboard's summary bar.

use ic_model::{CheckableState, Host, HostState, Service, ServiceState};

use crate::snapshot::Summary;

/// Accumulates a [`Summary`] over hosts and services.
#[derive(Debug, Default)]
pub(crate) struct Tally {
    summary: Summary,
    /// Severity and state of the worst unhandled problem so far.
    worst: Option<(u32, CheckableState)>,
}

impl Tally {
    /// Counts a host.
    pub(crate) fn add_host(&mut self, host: &Host) {
        self.add(
            CheckableState::Host(host.state),
            host.is_handled(),
            host.severity(),
        );
    }

    /// Counts a service; `host` decides whether a problem is handled by a
    /// host problem (Icinga's `handled`).
    pub(crate) fn add_service(&mut self, service: &Service, host: Option<&Host>) {
        let host_problem = host.is_some_and(Host::is_problem);
        self.add(
            CheckableState::Service(service.state),
            service.is_handled(host_problem),
            service.severity(),
        );
    }

    /// Counts an object in `state`; `handled` and `severity` only matter
    /// for problems.
    pub(crate) fn add(&mut self, state: CheckableState, handled: bool, severity: u32) {
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
            self.problem(handled, severity, state);
        }
    }

    fn problem(&mut self, handled: bool, severity: u32, state: CheckableState) {
        if handled {
            self.summary.handled += 1;
            return;
        }
        self.summary.unhandled += 1;
        // Equal severities (a down host and a warning service) go to the
        // redder state, so the dot doesn't depend on the counting order.
        if self.worst.is_none_or(|(worst, worst_state)| {
            (severity, state_rank(state)) > (worst, state_rank(worst_state))
        }) {
            self.worst = Some((severity, state));
        }
    }

    /// The summary.
    pub(crate) fn finish(mut self) -> Summary {
        self.summary.worst_unhandled = self.worst.map(|(_, state)| state);
        self.summary
    }
}

/// Breaks ties between problems of equal severity: the state whose colour
/// is the more alarming wins.
fn state_rank(state: CheckableState) -> u8 {
    match state {
        CheckableState::Host(HostState::Down) => 6,
        CheckableState::Service(ServiceState::Critical) => 5,
        CheckableState::Host(HostState::Unreachable) => 4,
        CheckableState::Service(ServiceState::Unknown) => 3,
        CheckableState::Service(ServiceState::Warning) => 2,
        CheckableState::Host(HostState::Pending)
        | CheckableState::Service(ServiceState::Pending) => 1,
        CheckableState::Host(HostState::Up) | CheckableState::Service(ServiceState::Ok) => 0,
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

    #[test]
    fn equal_severities_go_to_the_redder_state() {
        // A down host and a warning service both weigh 2080.
        let down = host("down", HostState::Down);
        let warning = service("up", ServiceState::Warning);
        assert_eq!(down.severity(), warning.severity());
        for down_first in [true, false] {
            let mut tally = Tally::default();
            if down_first {
                tally.add_host(&down);
                tally.add_service(&warning, None);
            } else {
                tally.add_service(&warning, None);
                tally.add_host(&down);
            }
            assert_eq!(
                tally.finish().worst_unhandled,
                Some(CheckableState::Host(HostState::Down))
            );
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
                // An unhandled down host (32 + 2048) beats an unhandled
                // warning (32 + 2048 too, but seen later).
                worst_unhandled: Some(CheckableState::Host(HostState::Down)),
            }
        );
    }

    #[test]
    fn the_worst_unhandled_state_is_by_severity() {
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
