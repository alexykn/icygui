//! Severity: the number the problem lists sort by ("severity ↓").
//!
//! This is Icinga 2's own formula (`Service::GetSeverity` and
//! `Host::GetSeverity` in `lib/icinga/{service,host}.cpp`), the value Icinga
//! DB Web sorts by. The API also returns a `severity` attribute, but events
//! don't carry it, so the client recomputes it after every update.
//!
//! - pending: 16, OK / UP: 0
//! - otherwise a state weight (service: warning 32, unknown 64, critical 128;
//!   host: down 32) plus a handling weight: acknowledged 512, else in
//!   downtime 256, else unreachable 1024, else unhandled 2048.

use crate::object::{CheckInfo, Host, Service};
use crate::state::{HostState, ServiceState};

const PENDING: u32 = 16;
const ACKNOWLEDGED: u32 = 512;
const IN_DOWNTIME: u32 = 256;
const UNREACHABLE: u32 = 1024;
const UNHANDLED: u32 = 2048;

/// Severity of a service.
#[must_use]
pub fn service_severity(service: &Service) -> u32 {
    let weight = match service.state {
        ServiceState::Pending => return PENDING,
        ServiceState::Ok => return 0,
        ServiceState::Warning => 32,
        ServiceState::Unknown => 64,
        ServiceState::Critical => 128,
    };
    weight + handling_weight(&service.check)
}

/// Severity of a host.
#[must_use]
pub fn host_severity(host: &Host) -> u32 {
    match host.state {
        HostState::Pending => PENDING,
        HostState::Up => 0,
        HostState::Down | HostState::Unreachable => 32 + handling_weight(&host.check),
    }
}

fn handling_weight(check: &CheckInfo) -> u32 {
    if check.acknowledgement.is_acknowledged() {
        ACKNOWLEDGED
    } else if check.downtime_depth > 0 {
        IN_DOWNTIME
    } else if !check.reachable {
        UNREACHABLE
    } else {
        UNHANDLED
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::AckKind;

    fn service(state: ServiceState) -> Service {
        let mut service = Service::new("h", "s");
        service.state = state;
        service
    }

    #[test]
    fn matches_icinga_values() {
        assert_eq!(service_severity(&service(ServiceState::Pending)), 16);
        assert_eq!(service_severity(&service(ServiceState::Ok)), 0);
        assert_eq!(service_severity(&service(ServiceState::Warning)), 32 + 2048);
        assert_eq!(service_severity(&service(ServiceState::Unknown)), 64 + 2048);
        assert_eq!(
            service_severity(&service(ServiceState::Critical)),
            128 + 2048
        );

        let mut acked = service(ServiceState::Critical);
        acked.check.acknowledgement = AckKind::Normal;
        acked.check.downtime_depth = 1;
        assert_eq!(
            service_severity(&acked),
            128 + 512,
            "ack wins over downtime"
        );

        let mut unreachable = service(ServiceState::Critical);
        unreachable.check.reachable = false;
        assert_eq!(service_severity(&unreachable), 128 + 1024);
    }

    #[test]
    fn unhandled_problems_sort_first() {
        let unhandled_warning = service(ServiceState::Warning);
        let mut acked_critical = service(ServiceState::Critical);
        acked_critical.check.acknowledgement = AckKind::Sticky;
        assert!(service_severity(&unhandled_warning) > service_severity(&acked_critical));
        assert!(
            service_severity(&service(ServiceState::Critical))
                > service_severity(&service(ServiceState::Unknown))
        );
    }

    #[test]
    fn host_values() {
        let mut host = Host::new("h");
        assert_eq!(host_severity(&host), 16);
        host.state = HostState::Up;
        assert_eq!(host_severity(&host), 0);
        host.state = HostState::Down;
        assert_eq!(host_severity(&host), 32 + 2048);
        host.check.downtime_depth = 2;
        assert_eq!(host_severity(&host), 32 + 256);
    }
}
