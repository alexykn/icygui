//! Generated hosts and services for load testing the list
//! (`ICYGUI_DEMO_ROWS=20000`): deterministic, realistic-looking checks in a
//! separate `load` environment, so the design's sample data stays as it is.

use ic_model::{AckKind, Host, HostState, Service, ServiceState, Timestamp};
use serde_json::json;

use super::objects::{DAY, HOUR, MINUTE, check_info, check_result, service};

/// Services per generated host.
pub(super) const SERVICES_PER_HOST: usize = 20;

/// The `host.vars.env` of generated hosts.
pub(super) const ENV: &str = "load";

const CHECKS: [&str; SERVICES_PER_HOST] = [
    "apt",
    "disk /",
    "disk /var",
    "dns",
    "http",
    "imap",
    "ldap",
    "load",
    "memory",
    "mysql",
    "nginx",
    "ntp-offset",
    "procs",
    "raid",
    "redis",
    "smart /dev/sda",
    "smtp",
    "ssh",
    "swap",
    "zfs-pool",
];

/// A small deterministic hash (`SplitMix64`), so the same index always gets
/// the same state and age.
fn mix(index: usize) -> u64 {
    let mut value = (index as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

/// `count` services on `count / 20` (rounded up) hosts named `load-00001`….
pub(super) fn generate(now: Timestamp, count: usize) -> (Vec<Host>, Vec<Service>) {
    let host_count = count.div_ceil(SERVICES_PER_HOST);
    let hosts: Vec<Host> = (0..host_count)
        .map(|index| {
            let mut host = Host::new(&format!("load-{:05}", index + 1));
            host.address = format!(
                "10.{}.{}.{}",
                100 + index / 65_536,
                index / 256 % 256,
                index % 256
            );
            host.state = HostState::Up;
            host.check = check_info(now, 20. * DAY, 31., 60.);
            "hostalive".clone_into(&mut host.check.check_command);
            host.check.result = Some(check_result(now, "PING OK rta 0.51ms", 0, 31.));
            host.vars.insert("env".to_owned(), json!(ENV));
            host.vars.insert("role".to_owned(), json!("generated"));
            host.groups = vec!["load-test".to_owned()];
            host
        })
        .collect();
    let services = (0..count)
        .filter_map(|index| {
            let host = hosts.get(index / SERVICES_PER_HOST)?;
            let name = CHECKS[index % SERVICES_PER_HOST];
            let hash = mix(index);
            let (state, output) = match hash % 100 {
                0..70 => (ServiceState::Ok, format!("OK - {name} healthy")),
                70..85 => (
                    ServiceState::Warning,
                    format!("WARNING - {name} at {}% of its limit", 80 + hash % 15),
                ),
                85..95 => (
                    ServiceState::Critical,
                    format!("CRITICAL - {name} failed ({} errors in 5m)", 10 + hash % 90),
                ),
                _ => (
                    ServiceState::Unknown,
                    format!("UNKNOWN - {name}: plugin timed out after 30s"),
                ),
            };
            #[expect(clippy::cast_precision_loss, reason = "a hash spread over a week")]
            let since = MINUTE + (hash >> 8) as f64 % (7. * DAY);
            let mut generated = service(now, host, name, state, since, &output);
            if state.is_problem() {
                match (hash >> 40) & 0xf {
                    0 => generated.check.acknowledgement = AckKind::Normal,
                    1 => generated.check.downtime_depth = 1,
                    2 => generated.check.flapping = true,
                    _ => {}
                }
            }
            generated.check.check_interval = 5. * MINUTE;
            generated.check.retry_interval = MINUTE;
            if generated.check.last_state_change.as_unix_seconds() > now.as_unix_seconds() - HOUR {
                generated.check.state_type = ic_model::StateType::Soft;
                generated.check.attempt = 2;
            }
            Some(generated)
        })
        .collect();
    (hosts, services)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_exactly_the_requested_services() {
        let now = Timestamp::from_unix_seconds(1_790_000_000.);
        let (hosts, services) = generate(now, 45);
        assert_eq!(hosts.len(), 3);
        assert_eq!(services.len(), 45);
        assert_eq!(hosts[0].name.as_str(), "load-00001");
        assert!(
            services
                .iter()
                .all(|service| service.key.host.as_str().starts_with("load-"))
        );
        let (no_hosts, no_services) = generate(now, 0);
        assert!(no_hosts.is_empty() && no_services.is_empty());
    }

    #[test]
    fn states_are_mixed_and_deterministic() {
        let now = Timestamp::from_unix_seconds(1_790_000_000.);
        let (_, first) = generate(now, 2_000);
        let (_, second) = generate(now, 2_000);
        assert_eq!(first, second);
        let problems = first.iter().filter(|service| service.is_problem()).count();
        assert!(
            (400..800).contains(&problems),
            "about 30 % problems: {problems}"
        );
        assert!(
            first
                .iter()
                .any(|service| service.state == ServiceState::Unknown)
        );
        assert!(
            first
                .iter()
                .any(|service| service.check.acknowledgement.is_acknowledged())
        );
    }
}
