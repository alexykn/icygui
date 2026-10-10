//! Heartbeats (icygui's PLAN.md §4.2 B3): always-OK `dummy` checks that
//! prove a zone, or one endpoint of an HA zone, runs its checks. The
//! server runs them in real time at their `check_interval`
//! ([`Scenario::realtime`]), simulation or not; a check pinned to an
//! endpoint that isn't connected comes back UNKNOWN with Icinga's own
//! output, and [`crate::MockControl::stop_checks`] stops every check, the
//! way a hung checker does.

use ic_model::{Host, HostState, ObjectKey, Service, ServiceState, StateType, Timestamp, Vars};

use super::{Scenario, build::raw_check_result};

/// The custom variable that marks a heartbeat (icygui's default).
pub const HEARTBEAT_VARIABLE: &str = "icygui_heartbeat";

impl Scenario {
    /// Adds heartbeats every `interval` seconds, as the user guide
    /// configures them (mock-up 16a's topology): for every zone of masters
    /// or satellites (not the agents' zones, and not the global ones) a
    /// host `icygui-hb-<zone>` in that zone; a satellite zone gets a service
    /// `beat` (the zone's beat); a zone with more than one endpoint gets a
    /// service `beat-<endpoint>` pinned to each endpoint with
    /// `command_endpoint`. The masters' HA zone has only its pinned beats
    /// (they prove the zone too). Each has `vars.icygui_heartbeat = true`,
    /// is OK, and runs in real time.
    #[must_use]
    pub fn with_heartbeats(mut self, interval: f64) -> Self {
        let at = self.time_base;
        let zones: Vec<(String, Vec<String>, bool)> = self
            .zones
            .iter()
            .filter(|zone| !zone.global)
            .map(|zone| {
                let endpoints: Vec<String> = self
                    .endpoints
                    .iter()
                    .filter(|endpoint| endpoint.zone == zone.name)
                    .map(|endpoint| endpoint.name.clone())
                    .collect();
                (zone.name.clone(), endpoints, zone.parent.is_none())
            })
            .filter(|(zone, endpoints, _)| {
                // An agent's zone: a leaf named like its only endpoint.
                match endpoints.as_slice() {
                    [] => false,
                    [only] => only != zone,
                    _ => true,
                }
            })
            .collect();
        for (zone, endpoints, top) in zones {
            let host_name = format!("icygui-hb-{zone}");
            let mut host = Host::new(&host_name);
            host.state = HostState::Up;
            "dummy".clone_into(&mut host.check.check_command);
            host.check.check_interval = 300.0;
            host.check.retry_interval = 60.0;
            host.check.max_attempts = 1;
            host.check.state_type = StateType::Hard;
            host.check.zone = Some(zone.clone());
            host.check.result = Some(raw_check_result("icygui heartbeat host", &[], 0, at));
            host.check.last_check = Some(at);
            host.check.last_state_change =
                Timestamp::from_unix_seconds(at.as_unix_seconds() - 86_400.0);
            self.hosts.push(host);
            let ha = endpoints.len() > 1;
            let mut beats = if top && ha {
                Vec::new()
            } else {
                vec![(String::from("beat"), None)]
            };
            if ha {
                beats.extend(
                    endpoints
                        .iter()
                        .map(|endpoint| (format!("beat-{endpoint}"), Some(endpoint.clone()))),
                );
            }
            for (name, endpoint) in beats {
                let mut service = Service::new(&host_name, &name);
                service.state = ServiceState::Ok;
                let mut vars = Vars::new();
                vars.insert(HEARTBEAT_VARIABLE.to_owned(), serde_json::Value::Bool(true));
                service.vars = vars;
                "dummy".clone_into(&mut service.check.check_command);
                service.check.check_interval = interval;
                service.check.retry_interval = interval;
                service.check.max_attempts = 1;
                service.check.state_type = StateType::Hard;
                service.check.zone = Some(zone.clone());
                service.check.command_endpoint = endpoint;
                service.check.result = Some(raw_check_result("icygui heartbeat", &[], 0, at));
                service.check.last_check = Some(at);
                service.check.last_state_change =
                    Timestamp::from_unix_seconds(at.as_unix_seconds() - 86_400.0);
                service.check.last_hard_state_change = service.check.last_state_change;
                self.realtime.push(service.object_key());
                self.services.push(service);
            }
        }
        self
    }

    /// Adds a connected endpoint `name` to zone `zone` (the demo makes
    /// `prod-cluster`'s zone `fra` an HA zone with it).
    #[must_use]
    pub fn with_endpoint(mut self, name: &str, zone: &str) -> Self {
        self.endpoints.push(super::Endpoint {
            name: name.to_owned(),
            zone: zone.to_owned(),
            connected: true,
        });
        self
    }

    /// The heartbeat services ([`Scenario::with_heartbeats`]).
    #[must_use]
    pub fn heartbeats(&self) -> Vec<ObjectKey> {
        self.services
            .iter()
            .filter(|service| service.vars.contains_key(HEARTBEAT_VARIABLE))
            .map(Service::object_key)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_satellite_zone_and_ha_endpoint_gets_a_beat() {
        let scenario = crate::scenarios::prod_cluster().with_heartbeats(10.0);
        let beats: Vec<String> = scenario
            .heartbeats()
            .iter()
            .map(ObjectKey::full_name)
            .collect();
        // The masters' HA zone: pinned beats only; a satellite zone: its
        // own beat, and pinned ones too once it is HA.
        assert!(
            !beats.contains(&"icygui-hb-master!beat".to_owned()),
            "{beats:?}"
        );
        assert!(
            beats.contains(&"icygui-hb-ams!beat".to_owned()),
            "{beats:?}"
        );
        let ha = crate::scenarios::prod_cluster()
            .with_endpoint("sat-fra-02", "fra")
            .with_heartbeats(10.0)
            .heartbeats()
            .iter()
            .map(ObjectKey::full_name)
            .collect::<Vec<_>>();
        for beat in [
            "icygui-hb-fra!beat",
            "icygui-hb-fra!beat-sat-fra-01",
            "icygui-hb-fra!beat-sat-fra-02",
        ] {
            assert!(ha.contains(&beat.to_owned()), "{ha:?}");
        }
        assert_eq!(ha.len(), 6, "mock-up 16a's six beats");
        assert!(
            beats.contains(&"icygui-hb-master!beat-master-01".to_owned()),
            "{beats:?}"
        );
        assert!(
            beats.contains(&"icygui-hb-master!beat-master-02".to_owned()),
            "{beats:?}"
        );
        assert_eq!(scenario.realtime.len(), beats.len());
        let pinned = scenario
            .service("icygui-hb-master", "beat-master-02")
            .unwrap();
        assert_eq!(pinned.check.command_endpoint.as_deref(), Some("master-02"));
        assert_eq!(pinned.check.zone.as_deref(), Some("master"));
        assert!((pinned.check.check_interval - 10.0).abs() < f64::EPSILON);
    }
}
