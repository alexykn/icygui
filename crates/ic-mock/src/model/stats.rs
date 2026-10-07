//! `/v1/status`: the status functions (`cib.cpp`, `IcingaApplication`,
//! `ApiListener`, `CheckerComponent`).

use std::collections::VecDeque;

use serde_json::{Map, Value as Json};

use super::World;
use super::logic::DepType;
use crate::json::{int, num};

/// The status functions of Icinga 2.15's packages, in its (sorted) order.
/// Features that aren't enabled still answer, with an empty status.
pub(crate) const STATUS_FUNCTIONS: &[&str] = &[
    "ApiListener",
    "CIB",
    "CheckerComponent",
    "ElasticsearchWriter",
    "FileLogger",
    "GelfWriter",
    "GraphiteWriter",
    "IcingaApplication",
    "IdoMysqlConnection",
    "IdoPgsqlConnection",
    "Influxdb2Writer",
    "InfluxdbWriter",
    "JournaldLogger",
    "NotificationComponent",
    "OpenTsdbWriter",
    "PerfdataWriter",
    "SyslogLogger",
];

const ACTIVE_HOST: usize = 0;
const PASSIVE_HOST: usize = 1;
const ACTIVE_SERVICE: usize = 2;
const PASSIVE_SERVICE: usize = 3;

/// Processed check results per second over the last 15 minutes
/// (`CIB::Update*ChecksStatistics`).
#[derive(Clone, Debug, Default)]
pub(crate) struct CheckStats {
    buckets: VecDeque<(i64, [u32; 4])>,
}

impl CheckStats {
    pub(crate) fn record(&mut self, at: f64, active: bool, host: bool) {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Unix seconds fit in i64; Icinga also truncates to long"
        )]
        let second = at.floor() as i64;
        // Icinga's `UpdatePassiveHostChecksStatistics` adds to the passive
        // *service* counter (a long-standing bug the API faithfully shows).
        let slot = match (active, host) {
            (true, true) => ACTIVE_HOST,
            (true, false) => ACTIVE_SERVICE,
            (false, _) => PASSIVE_SERVICE,
        };
        match self.buckets.back_mut() {
            Some((last, counts)) if *last == second => counts[slot] += 1,
            _ => {
                let mut counts = [0; 4];
                counts[slot] = 1;
                self.buckets.push_back((second, counts));
            }
        }
        while self
            .buckets
            .front()
            .is_some_and(|(first, _)| *first < second - 900)
        {
            self.buckets.pop_front();
        }
    }

    fn count(&self, slot: usize, now: f64, window: f64) -> u32 {
        self.buckets
            .iter()
            .filter(|(second, _)| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "Unix seconds fit a double exactly"
                )]
                let at = *second as f64;
                at > now - window
            })
            .map(|(_, counts)| counts[slot])
            .sum()
    }

    /// Active host and service checks in the last minute.
    pub(crate) fn checks_last_minute(&self, now: f64) -> u32 {
        self.count(ACTIVE_HOST, now, 60.0) + self.count(ACTIVE_SERVICE, now, 60.0)
    }
}

fn perfdata_value(label: &str, value: f64) -> Json {
    let mut map = Map::new();
    map.insert("counter".into(), Json::Bool(false));
    map.insert("crit".into(), Json::Null);
    map.insert("label".into(), Json::String(label.to_owned()));
    map.insert("max".into(), Json::Null);
    map.insert("min".into(), Json::Null);
    map.insert("type".into(), Json::String("PerfdataValue".into()));
    map.insert("unit".into(), Json::String(String::new()));
    map.insert("value".into(), num(value));
    map.insert("warn".into(), Json::Null);
    Json::Object(map)
}

/// Latency and execution time statistics over checked objects.
struct CheckTimes {
    min_latency: f64,
    max_latency: f64,
    avg_latency: f64,
    min_execution_time: f64,
    max_execution_time: f64,
    avg_execution_time: f64,
}

impl World {
    /// The entries of `/v1/status`, or one named entry.
    pub(crate) fn status_entry(&self, name: &str) -> Option<Json> {
        let (status, perfdata) = match name {
            "ApiListener" => self.api_listener_status(),
            "CIB" => (self.cib_status(), Vec::new()),
            "CheckerComponent" => {
                let idle = self
                    .all_checkables()
                    .filter(|c| c.enable_active_checks)
                    .count();
                let mut checker = Map::new();
                checker.insert("idle".into(), int(i64::try_from(idle).unwrap_or(i64::MAX)));
                checker.insert("pending".into(), int(0));
                let mut nodes = Map::new();
                nodes.insert("checker".into(), Json::Object(checker));
                let mut status = Map::new();
                status.insert("checkercomponent".into(), Json::Object(nodes));
                #[expect(clippy::cast_precision_loss, reason = "object counts fit a double")]
                let idle = idle as f64;
                (
                    status,
                    vec![
                        perfdata_value("checkercomponent_checker_idle", idle),
                        perfdata_value("checkercomponent_checker_pending", 0.0),
                    ],
                )
            }
            "IcingaApplication" => (self.icinga_application_status(), Vec::new()),
            "NotificationComponent" => {
                let mut nodes = Map::new();
                nodes.insert("notification".into(), int(1));
                let mut status = Map::new();
                status.insert("notificationcomponent".into(), Json::Object(nodes));
                (status, Vec::new())
            }
            other if STATUS_FUNCTIONS.contains(&other) => {
                // A feature that isn't enabled: no instances to report.
                let mut status = Map::new();
                status.insert(other.to_lowercase(), Json::Object(Map::new()));
                (status, Vec::new())
            }
            _ => return None,
        };
        let mut entry = Map::new();
        entry.insert("name".into(), Json::String(name.to_owned()));
        entry.insert("perfdata".into(), Json::Array(perfdata));
        entry.insert("status".into(), Json::Object(status));
        Some(Json::Object(entry))
    }

    fn icinga_application_status(&self) -> Map<String, Json> {
        let app = &self.app;
        let mut values = Map::new();
        values.insert(
            "enable_event_handlers".into(),
            Json::Bool(app.enable_event_handlers),
        );
        values.insert("enable_flapping".into(), Json::Bool(app.enable_flapping));
        values.insert(
            "enable_host_checks".into(),
            Json::Bool(app.enable_host_checks),
        );
        values.insert(
            "enable_notifications".into(),
            Json::Bool(app.enable_notifications),
        );
        values.insert("enable_perfdata".into(), Json::Bool(app.enable_perfdata));
        values.insert(
            "enable_service_checks".into(),
            Json::Bool(app.enable_service_checks),
        );
        values.insert("environment".into(), Json::String(app.environment.clone()));
        values.insert("node_name".into(), Json::String(app.node_name.clone()));
        values.insert("pid".into(), int(app.pid));
        values.insert("program_start".into(), num(app.program_start));
        values.insert("version".into(), Json::String(app.version.clone()));
        let mut nodes = Map::new();
        nodes.insert("app".into(), Json::Object(values));
        let mut status = Map::new();
        status.insert("icingaapplication".into(), Json::Object(nodes));
        status
    }

    fn api_listener_status(&self) -> (Map<String, Json>, Vec<Json>) {
        let identity = self.app.node_name.clone();
        let local_zone = self
            .endpoints
            .get(&identity)
            .map(|e| e.member_of.clone())
            .unwrap_or_default();
        let local_parent = self
            .zones
            .get(&local_zone)
            .map(|z| z.parent.clone())
            .unwrap_or_default();
        let mut connected = Vec::new();
        let mut not_connected = Vec::new();
        let mut zones = Map::new();
        for zone in self.zones.values() {
            let relevant =
                zone.name == local_zone || zone.parent == local_zone || zone.name == local_parent;
            if !relevant || zone.global {
                continue;
            }
            let mut zone_connected = false;
            let mut counted = 0;
            for endpoint_name in &zone.endpoints {
                if *endpoint_name == identity {
                    continue;
                }
                counted += 1;
                match self.endpoints.get(endpoint_name) {
                    Some(endpoint) if endpoint.connected => {
                        connected.push(Json::String(endpoint_name.clone()));
                        zone_connected = true;
                    }
                    _ => not_connected.push(Json::String(endpoint_name.clone())),
                }
            }
            if zone.endpoints.len() == 1 && counted == 0 {
                zone_connected = true;
            }
            let mut stats = Map::new();
            stats.insert("client_log_lag".into(), num(0.0));
            stats.insert("connected".into(), Json::Bool(zone_connected));
            stats.insert(
                "endpoints".into(),
                Json::Array(zone.endpoints.iter().cloned().map(Json::String).collect()),
            );
            stats.insert("parent_zone".into(), Json::String(zone.parent.clone()));
            zones.insert(zone.name.clone(), Json::Object(stats));
        }
        let total = connected.len() + not_connected.len();
        #[expect(clippy::cast_precision_loss, reason = "endpoint counts fit a double")]
        let (total_f, connected_f, not_connected_f) = (
            total as f64,
            connected.len() as f64,
            not_connected.len() as f64,
        );
        let mut json_rpc = Map::new();
        json_rpc.insert("anonymous_clients".into(), int(0));
        json_rpc.insert("relay_queue_item_rate".into(), num(0.0));
        json_rpc.insert("relay_queue_items".into(), int(0));
        json_rpc.insert("sync_queue_item_rate".into(), num(0.0));
        json_rpc.insert("sync_queue_items".into(), int(0));
        json_rpc.insert("work_queue_item_rate".into(), num(0.0));
        let mut http = Map::new();
        http.insert("clients".into(), int(1));
        let mut api = Map::new();
        api.insert("conn_endpoints".into(), Json::Array(connected));
        api.insert("http".into(), Json::Object(http));
        api.insert("identity".into(), Json::String(identity));
        api.insert("json_rpc".into(), Json::Object(json_rpc));
        api.insert("not_conn_endpoints".into(), Json::Array(not_connected));
        api.insert("num_conn_endpoints".into(), num(connected_f));
        api.insert("num_endpoints".into(), num(total_f));
        api.insert("num_not_conn_endpoints".into(), num(not_connected_f));
        api.insert("zones".into(), Json::Object(zones));
        let mut status = Map::new();
        status.insert("api".into(), Json::Object(api));
        let perfdata = vec![
            perfdata_value("api_num_conn_endpoints", connected_f),
            perfdata_value("api_num_endpoints", total_f),
            perfdata_value("api_num_http_clients", 1.0),
            perfdata_value("api_num_json_rpc_anonymous_clients", 0.0),
            perfdata_value("api_num_json_rpc_relay_queue_item_rate", 0.0),
            perfdata_value("api_num_json_rpc_relay_queue_items", 0.0),
            perfdata_value("api_num_json_rpc_sync_queue_item_rate", 0.0),
            perfdata_value("api_num_json_rpc_sync_queue_items", 0.0),
            perfdata_value("api_num_json_rpc_work_queue_item_rate", 0.0),
            perfdata_value("api_num_not_conn_endpoints", not_connected_f),
        ];
        (status, perfdata)
    }

    fn service_check_times(&self) -> CheckTimes {
        let mut count = 0.0;
        let (mut min_l, mut max_l, mut sum_l) = (-1.0_f64, 0.0_f64, 0.0);
        let (mut min_e, mut max_e, mut sum_e) = (-1.0_f64, 0.0_f64, 0.0);
        for cr in self.all_services().filter_map(|s| s.cr.as_ref()) {
            let execution = cr.execution_end - cr.execution_start;
            let latency = ((cr.schedule_end - cr.schedule_start) - execution).max(0.0);
            if min_l < 0.0 || latency < min_l {
                min_l = latency;
            }
            max_l = max_l.max(latency);
            sum_l += latency;
            if min_e < 0.0 || execution < min_e {
                min_e = execution;
            }
            max_e = max_e.max(execution);
            sum_e += execution;
            count += 1.0;
        }
        if count == 0.0 {
            min_l = 0.0;
            min_e = 0.0;
        }
        CheckTimes {
            min_latency: min_l,
            max_latency: max_l,
            // 0 / 0 is NaN in Icinga too, written as null.
            avg_latency: sum_l / count,
            min_execution_time: min_e,
            max_execution_time: max_e,
            avg_execution_time: sum_e / count,
        }
    }

    /// The CIB's host and service counts by state (`num_hosts_*`,
    /// `num_services_*`), as `cib_status` reports them.
    pub(crate) fn object_counts(&self) -> ic_model::ObjectCounts {
        let mut counts = ic_model::ObjectCounts::default();
        for service in self.all_services() {
            match service.state_raw {
                0 => counts.services_ok += 1,
                1 => counts.services_warning += 1,
                2 => counts.services_critical += 1,
                _ => counts.services_unknown += 1,
            }
            counts.services_pending += u32::from(!service.has_been_checked());
        }
        for host in self.hosts.values() {
            if !self.is_reachable(&host.full_name(), DepType::State) {
                counts.hosts_unreachable += 1;
            } else if host.state() == 0 {
                counts.hosts_up += 1;
            } else {
                counts.hosts_down += 1;
            }
            counts.hosts_pending += u32::from(!host.has_been_checked());
        }
        counts
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one entry per CIB statistic, as in cib.cpp"
    )]
    fn cib_status(&self) -> Map<String, Json> {
        let now = self.now();
        let interval = (now - self.app.program_start).clamp(1.0, 60.0);
        let mut status = Map::new();
        let s = &self.stats;
        let window = |slot: usize, secs: f64| f64::from(s.count(slot, now, secs));
        status.insert(
            "active_host_checks".into(),
            num(window(ACTIVE_HOST, interval) / interval),
        );
        status.insert(
            "passive_host_checks".into(),
            num(window(PASSIVE_HOST, interval) / interval),
        );
        status.insert(
            "active_host_checks_1min".into(),
            num(window(ACTIVE_HOST, 60.0)),
        );
        status.insert(
            "passive_host_checks_1min".into(),
            num(window(PASSIVE_HOST, 60.0)),
        );
        status.insert(
            "active_host_checks_5min".into(),
            num(window(ACTIVE_HOST, 300.0)),
        );
        status.insert(
            "passive_host_checks_5min".into(),
            num(window(PASSIVE_HOST, 300.0)),
        );
        status.insert(
            "active_host_checks_15min".into(),
            num(window(ACTIVE_HOST, 900.0)),
        );
        status.insert(
            "passive_host_checks_15min".into(),
            num(window(PASSIVE_HOST, 900.0)),
        );
        status.insert(
            "active_service_checks".into(),
            num(window(ACTIVE_SERVICE, interval) / interval),
        );
        status.insert(
            "passive_service_checks".into(),
            num(window(PASSIVE_SERVICE, interval) / interval),
        );
        status.insert(
            "active_service_checks_1min".into(),
            num(window(ACTIVE_SERVICE, 60.0)),
        );
        status.insert(
            "passive_service_checks_1min".into(),
            num(window(PASSIVE_SERVICE, 60.0)),
        );
        status.insert(
            "active_service_checks_5min".into(),
            num(window(ACTIVE_SERVICE, 300.0)),
        );
        status.insert(
            "passive_service_checks_5min".into(),
            num(window(PASSIVE_SERVICE, 300.0)),
        );
        status.insert(
            "active_service_checks_15min".into(),
            num(window(ACTIVE_SERVICE, 900.0)),
        );
        status.insert(
            "passive_service_checks_15min".into(),
            num(window(PASSIVE_SERVICE, 900.0)),
        );
        status.insert("remote_check_queue".into(), int(0));
        status.insert("current_pending_callbacks".into(), int(0));
        status.insert("current_concurrent_checks".into(), int(0));

        let times = self.service_check_times();
        status.insert("min_latency".into(), num(times.min_latency));
        status.insert("max_latency".into(), num(times.max_latency));
        status.insert("avg_latency".into(), num(times.avg_latency));
        status.insert("min_execution_time".into(), num(times.min_execution_time));
        status.insert("max_execution_time".into(), num(times.max_execution_time));
        status.insert("avg_execution_time".into(), num(times.avg_execution_time));

        let mut services = [0u32; 11];
        for service in self.all_services() {
            let full = service.full_name();
            match service.state_raw {
                0 => services[0] += 1,
                1 => services[1] += 1,
                2 => services[2] += 1,
                _ => services[3] += 1,
            }
            services[4] += u32::from(!service.has_been_checked());
            services[5] += u32::from(!self.is_reachable(&full, DepType::State));
            services[6] += u32::from(self.is_flapping(service));
            services[7] += u32::from(self.downtime_depth(&full) > 0);
            services[8] += u32::from(self.is_acknowledged(service));
            services[9] += u32::from(self.handled(service));
            services[10] += u32::from(service.problem());
        }
        for (key, value) in [
            "num_services_ok",
            "num_services_warning",
            "num_services_critical",
            "num_services_unknown",
            "num_services_pending",
            "num_services_unreachable",
            "num_services_flapping",
            "num_services_in_downtime",
            "num_services_acknowledged",
            "num_services_handled",
            "num_services_problem",
        ]
        .into_iter()
        .zip(services)
        {
            status.insert(key.into(), int(value));
        }
        status.insert("uptime".into(), num(now - self.app.program_start));

        let mut hosts = [0u32; 9];
        for host in self.hosts.values() {
            let full = host.full_name();
            if self.is_reachable(&full, DepType::State) {
                if host.state() == 0 {
                    hosts[0] += 1;
                } else {
                    hosts[1] += 1;
                }
            } else {
                hosts[3] += 1;
            }
            hosts[2] += u32::from(!host.has_been_checked());
            hosts[4] += u32::from(self.is_flapping(host));
            hosts[5] += u32::from(self.downtime_depth(&full) > 0);
            hosts[6] += u32::from(self.is_acknowledged(host));
            hosts[7] += u32::from(self.handled(host));
            hosts[8] += u32::from(host.problem());
        }
        for (key, value) in [
            "num_hosts_up",
            "num_hosts_down",
            "num_hosts_pending",
            "num_hosts_unreachable",
            "num_hosts_flapping",
            "num_hosts_in_downtime",
            "num_hosts_acknowledged",
            "num_hosts_handled",
            "num_hosts_problem",
        ]
        .into_iter()
        .zip(hosts)
        {
            status.insert(key.into(), int(value));
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_checks_in_windows() {
        let mut stats = CheckStats::default();
        stats.record(1000.2, true, false);
        stats.record(1000.7, true, true);
        stats.record(1030.0, false, false);
        stats.record(1030.0, false, true);
        assert_eq!(stats.count(ACTIVE_SERVICE, 1031.0, 60.0), 1);
        assert_eq!(stats.count(ACTIVE_HOST, 1031.0, 60.0), 1);
        assert_eq!(
            stats.count(PASSIVE_SERVICE, 1031.0, 60.0),
            2,
            "Icinga's quirk"
        );
        assert_eq!(stats.count(PASSIVE_HOST, 1031.0, 60.0), 0);
        assert_eq!(stats.count(ACTIVE_SERVICE, 1080.0, 60.0), 0);
        assert_eq!(stats.checks_last_minute(1031.0), 2);
        // Old buckets are dropped after 15 minutes.
        stats.record(2000.0, true, false);
        assert_eq!(stats.count(ACTIVE_SERVICE, 2000.0, 10_000.0), 1);
    }
}
