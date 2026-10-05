//! Realistic plugin output and performance data per check command, for the
//! scenarios and the simulator.

use crate::rng::Rng;

/// A check result's output and performance data.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PluginResult {
    pub(crate) output: String,
    pub(crate) perfdata: Vec<String>,
}

fn result(output: String, perfdata: Vec<String>) -> PluginResult {
    PluginResult { output, perfdata }
}

/// Output for `state` (0 OK .. 3 UNKNOWN) of a service check command.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per check command keeps the templates readable"
)]
pub(crate) fn service_output(command: &str, state: u8, rng: &mut Rng) -> PluginResult {
    match (command, state) {
        ("ping4" | "ping", 0) => {
            let rta = rng.range(0.2, 2.5);
            result(
                format!("PING OK - Packet loss = 0%, RTA = {rta:.2} ms"),
                vec![
                    format!("rta={:.6}ms;3000.000000;5000.000000;0.000000", rta),
                    "pl=0%;80;100;0".to_owned(),
                ],
            )
        }
        ("ping4" | "ping", 1) => {
            let rta = rng.range(3_000.0, 4_900.0);
            result(
                format!("PING WARNING - Packet loss = 20%, RTA = {rta:.2} ms"),
                vec![
                    format!("rta={rta:.6}ms;3000.000000;5000.000000;0.000000"),
                    "pl=20%;80;100;0".to_owned(),
                ],
            )
        }
        ("ping4" | "ping", 2) => result(
            "PING CRITICAL - Packet loss = 100%".to_owned(),
            vec![
                "rta=5000.000000ms;3000.000000;5000.000000;0.000000".to_owned(),
                "pl=100%;80;100;0".to_owned(),
            ],
        ),
        ("ssh", 0) => {
            let time = rng.range(0.004, 0.05);
            result(
                "SSH OK - OpenSSH_9.2p1 Debian-2+deb12u3 (protocol 2.0)".to_owned(),
                vec![format!("time={time:.6}s;;;0.000000;10.000000")],
            )
        }
        ("ssh", _) => result(
            "CRITICAL - Socket timeout after 10 seconds".to_owned(),
            Vec::new(),
        ),
        ("disk", 0) => {
            let used = rng.range(18.0, 68.0);
            result(
                format!("DISK OK - {used:.0}% used"),
                vec![format!("/={:.1}GB;35.2;39.6;0;44.0", used * 0.44)],
            )
        }
        ("disk", 1) => {
            let used = rng.range(81.0, 89.0);
            result(
                format!("DISK WARNING - {used:.0}% used"),
                vec![format!("/={:.1}GB;35.2;39.6;0;44.0", used * 0.44)],
            )
        }
        ("disk", 2) => {
            let used = rng.range(91.0, 99.0);
            result(
                format!("DISK CRITICAL - {used:.0}% used"),
                vec![format!("/={:.1}GB;35.2;39.6;0;44.0", used * 0.44)],
            )
        }
        ("load", 0) => {
            let l1 = rng.range(0.2, 3.2);
            let l5 = l1 * rng.range(0.8, 1.1);
            let l15 = l5 * rng.range(0.8, 1.05);
            load_result("OK", l1, l5, l15)
        }
        ("load", 1) => {
            let l1 = rng.range(6.0, 9.5);
            load_result("WARNING", l1, l1 * 0.9, l1 * 0.8)
        }
        ("load", 2) => {
            let l1 = rng.range(10.5, 24.0);
            load_result("CRITICAL", l1, l1 * 0.9, l1 * 0.8)
        }
        ("mem" | "memory", 0) => {
            let used = rng.range(28.0, 74.0);
            result(
                format!("OK - {used:.0}% used"),
                vec![format!("used={used:.1}%;85;95;0;100")],
            )
        }
        ("mem" | "memory", 1) => {
            let used = rng.range(86.0, 94.0);
            result(
                format!("WARNING - {used:.0}% used"),
                vec![format!("used={used:.1}%;85;95;0;100")],
            )
        }
        ("mem" | "memory", 2) => {
            let used = rng.range(95.5, 99.5);
            result(
                format!("CRITICAL - {used:.0}% used"),
                vec![format!("used={used:.1}%;85;95;0;100")],
            )
        }
        ("ntp_time", 0) => {
            let offset = rng.range(0.0005, 0.02);
            result(
                format!("OK - offset {offset:.3}s"),
                vec![format!("offset={offset:.6}s;0.500000;1.000000;")],
            )
        }
        ("ntp_time", 1) => {
            let offset = rng.range(0.55, 0.95);
            result(
                format!("WARNING - offset {offset:.2}s"),
                vec![format!("offset={offset:.6}s;0.500000;1.000000;")],
            )
        }
        ("ntp_time", 2) => {
            let offset = rng.range(1.2, 4.0);
            result(
                format!("CRITICAL - offset {offset:.2}s"),
                vec![format!("offset={offset:.6}s;0.500000;1.000000;")],
            )
        }
        ("http", 0) => {
            let time = rng.range(0.008, 0.21);
            let size = 2_000 + rng.below(40_000);
            result(
                format!(
                    "HTTP OK: HTTP/1.1 200 OK - {size} bytes in {time:.3} second response time"
                ),
                vec![
                    format!("time={time:.6}s;;;0.000000;10.000000"),
                    format!("size={size}B;;;0"),
                ],
            )
        }
        ("http", 1) => {
            let time = rng.range(1.6, 4.5);
            result(
                format!("HTTP WARNING: HTTP/1.1 200 OK - 4521 bytes in {time:.3} second response time"),
                vec![format!("time={time:.6}s;1.500000;5.000000;0.000000;10.000000")],
            )
        }
        ("http", 2) => result(
            "HTTP CRITICAL: HTTP/1.1 503 Service Unavailable - 312 bytes in 0.004 second response time"
                .to_owned(),
            vec!["time=0.004000s;;;0.000000;10.000000".to_owned()],
        ),
        ("swap", 0) => {
            let free = rng.range(80.0, 100.0);
            result(
                format!("SWAP OK - {free:.0}% free"),
                vec![format!("swap={free:.0}%;25;10;0;100")],
            )
        }
        ("swap", 1) => result(
            "SWAP WARNING - 22% free".to_owned(),
            vec!["swap=22%;25;10;0;100".to_owned()],
        ),
        ("apt", 0) => result(
            "APT OK: 0 packages available for upgrade (0 critical updates).".to_owned(),
            vec![
                "available_upgrades=0;;;0".to_owned(),
                "critical_updates=0;;;0".to_owned(),
            ],
        ),
        ("apt", 1) => {
            let count = 3 + rng.below(30);
            result(
                format!("APT WARNING: {count} packages available for upgrade (0 critical updates)."),
                vec![
                    format!("available_upgrades={count};;;0"),
                    "critical_updates=0;;;0".to_owned(),
                ],
            )
        }
        ("apt", 2) => result(
            "APT CRITICAL: 14 packages available for upgrade (3 critical updates).".to_owned(),
            vec![
                "available_upgrades=14;;;0".to_owned(),
                "critical_updates=3;;;0".to_owned(),
            ],
        ),
        ("procs", 0) => {
            let count = 120 + rng.below(200);
            result(
                format!("PROCS OK: {count} processes"),
                vec![format!("procs={count};500;800;0;")],
            )
        }
        ("procs", 1) => result(
            "PROCS WARNING: 612 processes".to_owned(),
            vec!["procs=612;500;800;0;".to_owned()],
        ),
        (_, 3) => result(
            match rng.below(3) {
                0 => "UNKNOWN - connection refused".to_owned(),
                1 => "UNKNOWN - plugin timed out after 60 seconds".to_owned(),
                _ => "UNKNOWN - no data received from agent".to_owned(),
            },
            Vec::new(),
        ),
        (other, 0) => result(format!("OK - {other} healthy"), Vec::new()),
        (other, 1) => result(format!("WARNING - {other} degraded"), Vec::new()),
        (other, _) => result(format!("CRITICAL - {other} failed"), Vec::new()),
    }
}

fn load_result(label: &str, l1: f64, l5: f64, l15: f64) -> PluginResult {
    result(
        format!("{label} - load average {l1:.1}, {l5:.1}, {l15:.1}"),
        vec![
            format!("load1={l1:.3};8.000;12.000;0;"),
            format!("load5={l5:.3};6.000;10.000;0;"),
            format!("load15={l15:.3};4.000;8.000;0;"),
        ],
    )
}

/// Output of a host check: up (`ok`) or down.
pub(crate) fn host_output(address: &str, ok: bool, rng: &mut Rng) -> PluginResult {
    if ok {
        let rta = rng.range(0.15, 1.8);
        result(
            format!("PING OK - Packet loss = 0%, RTA = {rta:.2} ms"),
            vec![
                format!("rta={:.6}s;3.000000;5.000000;0.000000", rta / 1000.0),
                "pl=0%;80;100;0".to_owned(),
            ],
        )
    } else {
        result(
            format!("CRITICAL - Host Unreachable ({address})"),
            vec![
                "rta=0.000000s;3.000000;5.000000;0.000000".to_owned(),
                "pl=100%;80;100;0".to_owned(),
            ],
        )
    }
}

/// Output of a service whose host is down: network checks fail, agent
/// checks can't reach the agent (`endpoint` is who runs the check).
pub(crate) fn host_down_output(
    command: &str,
    host: &str,
    address: &str,
    endpoint: &str,
) -> (u8, PluginResult) {
    match command {
        "ping4" | "ping" => (
            2,
            result(
                "PING CRITICAL - Packet loss = 100%".to_owned(),
                vec![
                    "rta=5000.000000ms;3000.000000;5000.000000;0.000000".to_owned(),
                    "pl=100%;80;100;0".to_owned(),
                ],
            ),
        ),
        "ssh" | "http" | "tcp" => (
            2,
            result(
                format!("connect to address {address} and port 22: No route to host"),
                Vec::new(),
            ),
        ),
        _ => (
            3,
            result(
                format!("Remote Icinga instance '{host}' is not connected to '{endpoint}'"),
                Vec::new(),
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outputs_are_deterministic_and_plausible() {
        let a = service_output("ping4", 0, &mut Rng::new(1));
        let b = service_output("ping4", 0, &mut Rng::new(1));
        assert_eq!(a, b);
        assert!(a.output.starts_with("PING OK"));
        assert_eq!(a.perfdata.len(), 2);
        for command in ["disk", "load", "mem", "ntp_time", "http", "apt", "whatever"] {
            for state in 0..=3 {
                let out = service_output(command, state, &mut Rng::new(9));
                assert!(!out.output.is_empty(), "{command} {state}");
                for entry in &out.perfdata {
                    assert!(
                        ic_model::parse_perfdata_entry(entry).is_some(),
                        "{command}: {entry}"
                    );
                }
            }
        }
        assert!(
            host_output("10.0.0.1", false, &mut Rng::new(1))
                .output
                .contains("10.0.0.1")
        );
    }
}
