//! What a dashboard row shows, computed from the snapshot for the rows on
//! screen only. Pure, so it's tested without a window.

use ic_config::{GroupBy, ObjectKind, View};
use ic_core::snapshot::Snapshot;
use ic_model::{
    CheckInfo, CheckableState, Comment, CommentKind, Host, HostState, ObjectKey, ServiceState,
    Timestamp,
};

use crate::format;

/// A host or service row: `● postgres-replication on db-prod-03`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ObjectRow {
    /// The state circle's colour.
    pub(crate) state: CheckableState,
    /// Acknowledged, in downtime or behind a host problem: a hollow ring.
    pub(crate) handled: bool,
    /// Time in state under the circle (`14m`).
    pub(crate) since: String,
    /// Service or host display name.
    pub(crate) name: String,
    /// The host's display name, for services.
    pub(crate) host: Option<String>,
    /// First line of the plugin output.
    pub(crate) output: String,
    /// The right-aligned tag: why the problem is handled, or flapping.
    pub(crate) tag: Option<String>,
    /// `late 12m`: Icinga still reported the check overdue when asked
    /// (PERF-08).
    pub(crate) late: Option<String>,
}

/// A group header (`group_by`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupRow {
    /// Host name or group display name.
    pub(crate) label: String,
    /// `2 problems`, `5 services`.
    pub(crate) count: String,
    /// When grouping by host: the host itself, so the header shows its
    /// state and output.
    pub(crate) host: Option<ObjectRow>,
}

/// The row for `key`; `None` if the snapshot doesn't have the object (the
/// list then shows a placeholder until the next snapshot).
pub(crate) fn object_row(
    snapshot: &Snapshot,
    key: &ObjectKey,
    now: Timestamp,
) -> Option<ObjectRow> {
    let comments = snapshot.comments.get(key).map(Vec::as_slice);
    let late = late_label(snapshot, key, now);
    match key {
        ObjectKey::Host { name } => {
            let host = snapshot.hosts.get(name)?;
            Some(ObjectRow {
                state: CheckableState::Host(host.state),
                handled: host.is_handled(),
                since: format::since(host.check.last_state_change, now),
                name: host.display_name.clone(),
                host: None,
                output: output(&host.check, CheckableState::Host(host.state)),
                tag: tag(&host.check, comments, None),
                late,
            })
        }
        ObjectKey::Service { key: service_key } => {
            let service = snapshot.services.get(service_key)?;
            let host = snapshot.host_of(service_key);
            let host_problem = host.is_some_and(|host| host.is_problem());
            let state = CheckableState::Service(service.state);
            Some(ObjectRow {
                state,
                handled: service.is_handled(host_problem),
                since: format::since(service.check.last_state_change, now),
                name: service.display_name.clone(),
                host: Some(host.map_or_else(
                    || service_key.host.to_string(),
                    |host| host.display_name.clone(),
                )),
                output: output(&service.check, state),
                tag: tag(
                    &service.check,
                    comments,
                    host.map(AsRef::as_ref).filter(|_| service.is_problem()),
                ),
                late,
            })
        }
    }
}

/// `late 12m` for an object whose check is late: how long ago Icinga
/// expected its result (the deadline is on Icinga's clock, close enough to
/// ours for minutes).
pub(crate) fn late_label(snapshot: &Snapshot, key: &ObjectKey, now: Timestamp) -> Option<String> {
    let deadline = snapshot.late.get(key)?;
    let overdue = deadline.elapsed_until(now);
    Some(if overdue.as_secs() == 0 {
        "late".to_owned()
    } else {
        format!("late {}", ic_model::format_compact(overdue))
    })
}

/// The header row for a group of `count` rows labelled `label`.
pub(crate) fn group_row(
    snapshot: &Snapshot,
    view: &View,
    label: &str,
    count: usize,
    now: Timestamp,
) -> GroupRow {
    let host = (view.group_by == GroupBy::Host)
        .then(|| object_row(snapshot, &ObjectKey::host(label), now))
        .flatten();
    GroupRow {
        label: label.to_owned(),
        count: count_label(count, view),
        host,
    }
}

/// `1 problem`, `3 services`, `2 hosts`.
pub(crate) fn count_label(count: usize, view: &View) -> String {
    let (one, many) = match (view.problems_only, view.object_kind) {
        (true, _) => ("problem", "problems"),
        (false, ObjectKind::Services) => ("service", "services"),
        (false, ObjectKind::Hosts) => ("host", "hosts"),
    };
    format!("{count} {}", if count == 1 { one } else { many })
}

fn output(check: &CheckInfo, state: CheckableState) -> String {
    match (&check.result, state) {
        (Some(result), _) => result.output.clone(),
        (
            None,
            CheckableState::Service(ServiceState::Pending)
            | CheckableState::Host(HostState::Pending),
        ) => "waiting for the first check".to_owned(),
        (None, _) => String::new(),
    }
}

/// Why a problem is handled, or that it's flapping: `ack m.keller`,
/// `downtime`, `host down`, `flapping`. `problem_host` is the host of a
/// service in a problem state.
fn tag(
    check: &CheckInfo,
    comments: Option<&[Comment]>,
    problem_host: Option<&Host>,
) -> Option<String> {
    if check.acknowledgement.is_acknowledged() {
        let author = comments
            .into_iter()
            .flatten()
            .rev()
            .find(|comment| comment.kind == CommentKind::Acknowledgement)
            .map(|comment| comment.author.as_str())
            .filter(|author| !author.is_empty());
        return Some(author.map_or_else(|| "ack".to_owned(), |author| format!("ack {author}")));
    }
    if check.downtime_depth > 0 {
        return Some("downtime".to_owned());
    }
    if let Some(host) = problem_host.filter(|host| host.is_problem()) {
        return Some(format!(
            "host {}",
            format::state_word(CheckableState::Host(host.state))
        ));
    }
    check.flapping.then(|| "flapping".to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ic_model::{AckKind, CheckResult, Service};

    use super::*;

    const NOW: f64 = 1_790_000_000.;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(NOW)
    }

    fn ago(seconds: f64) -> Timestamp {
        Timestamp::from_unix_seconds(NOW - seconds)
    }

    fn snapshot(hosts: Vec<Host>, services: Vec<Service>, comments: Vec<Comment>) -> Snapshot {
        let mut by_object: std::collections::BTreeMap<ObjectKey, Vec<Comment>> =
            std::collections::BTreeMap::new();
        for comment in comments {
            by_object
                .entry(comment.object.clone())
                .or_default()
                .push(comment);
        }
        Snapshot {
            hosts: Arc::new(
                hosts
                    .into_iter()
                    .map(|host| (host.name.clone(), Arc::new(host)))
                    .collect(),
            ),
            services: Arc::new(
                services
                    .into_iter()
                    .map(|service| (service.key.clone(), Arc::new(service)))
                    .collect(),
            ),
            comments: Arc::new(by_object),
            ..Snapshot::default()
        }
    }

    fn host(name: &str, state: HostState) -> Host {
        let mut host = Host::new(name);
        host.state = state;
        host.check.last_state_change = ago(41. * 86_400.);
        host.check.result = Some(CheckResult {
            output: "PING OK".to_owned(),
            ..CheckResult::default()
        });
        host
    }

    fn service(host: &str, name: &str, state: ServiceState, output: &str) -> Service {
        let mut service = Service::new(host, name);
        service.state = state;
        service.check.last_state_change = ago(14. * 60.);
        service.check.result = Some(CheckResult {
            output: output.to_owned(),
            ..CheckResult::default()
        });
        service
    }

    fn ack_comment(object: ObjectKey, author: &str) -> Comment {
        Comment {
            name: format!("{object}!ack"),
            object,
            author: author.to_owned(),
            text: "renewal in progress".to_owned(),
            kind: CommentKind::Acknowledgement,
            entry_time: ago(60.),
            expire_time: None,
            persistent: false,
        }
    }

    #[test]
    fn service_rows_read_service_on_host() {
        let snapshot = snapshot(
            vec![host("db-prod-03", HostState::Up)],
            vec![service(
                "db-prod-03",
                "postgres-replication",
                ServiceState::Critical,
                "CRITICAL - standby lag 412s (> 300s)",
            )],
            Vec::new(),
        );
        let row = object_row(
            &snapshot,
            &ObjectKey::service("db-prod-03", "postgres-replication"),
            now(),
        )
        .unwrap();
        assert_eq!(row.name, "postgres-replication");
        assert_eq!(row.host.as_deref(), Some("db-prod-03"));
        assert_eq!(row.since, "14m");
        assert_eq!(row.output, "CRITICAL - standby lag 412s (> 300s)");
        assert_eq!(row.state, CheckableState::Service(ServiceState::Critical));
        assert!(!row.handled);
        assert_eq!(row.tag, None);
    }

    #[test]
    fn acknowledged_rows_name_the_author() {
        let mut acked = service(
            "web-edge-02",
            "http-tls",
            ServiceState::Critical,
            "SSL CRITICAL",
        );
        acked.check.acknowledgement = AckKind::Normal;
        let key = acked.object_key();
        let snapshot = snapshot(
            vec![host("web-edge-02", HostState::Up)],
            vec![acked],
            vec![ack_comment(key.clone(), "m.keller")],
        );
        let row = object_row(&snapshot, &key, now()).unwrap();
        assert!(row.handled);
        assert_eq!(row.tag.as_deref(), Some("ack m.keller"));
    }

    #[test]
    fn an_ack_without_its_comment_still_says_ack() {
        let mut acked = service("h", "s", ServiceState::Warning, "WARNING");
        acked.check.acknowledgement = AckKind::Sticky;
        let key = acked.object_key();
        let snapshot = snapshot(vec![host("h", HostState::Up)], vec![acked], Vec::new());
        assert_eq!(
            object_row(&snapshot, &key, now()).unwrap().tag.as_deref(),
            Some("ack")
        );
    }

    #[test]
    fn downtime_host_problems_and_flapping_tags() {
        let mut in_downtime = service("h", "a", ServiceState::Warning, "WARNING");
        in_downtime.check.downtime_depth = 1;
        let behind_down_host = service("down", "b", ServiceState::Critical, "CRITICAL");
        let mut flapping = service("h", "c", ServiceState::Ok, "OK");
        flapping.check.flapping = true;
        let ok_on_down_host = service("down", "d", ServiceState::Ok, "OK");
        let keys: Vec<ObjectKey> = [&in_downtime, &behind_down_host, &flapping, &ok_on_down_host]
            .iter()
            .map(|service| service.object_key())
            .collect();
        let snapshot = snapshot(
            vec![host("h", HostState::Up), host("down", HostState::Down)],
            vec![in_downtime, behind_down_host, flapping, ok_on_down_host],
            Vec::new(),
        );
        let tags: Vec<_> = keys
            .iter()
            .map(|key| object_row(&snapshot, key, now()).unwrap().tag)
            .collect();
        assert_eq!(
            tags,
            [
                Some("downtime".to_owned()),
                Some("host down".to_owned()),
                Some("flapping".to_owned()),
                None
            ]
        );
        let behind = object_row(&snapshot, &keys[1], now()).unwrap();
        assert!(behind.handled, "a host problem handles its services");
    }

    #[test]
    fn host_rows_have_no_context() {
        let mut down = host("k8s-node-11", HostState::Down);
        down.check.last_state_change = ago(12. * 60.);
        let snapshot = snapshot(vec![down], Vec::new(), Vec::new());
        let row = object_row(&snapshot, &ObjectKey::host("k8s-node-11"), now()).unwrap();
        assert_eq!(row.host, None);
        assert_eq!(row.since, "12m");
        assert_eq!(row.state, CheckableState::Host(HostState::Down));
    }

    #[test]
    fn pending_and_missing_objects() {
        let pending = Service::new("h", "new-check");
        let key = pending.object_key();
        let snapshot = snapshot(vec![host("h", HostState::Up)], vec![pending], Vec::new());
        let row = object_row(&snapshot, &key, now()).unwrap();
        assert_eq!(row.output, "waiting for the first check");
        assert_eq!(row.since, "");
        assert!(object_row(&snapshot, &ObjectKey::service("h", "gone"), now()).is_none());
        assert!(object_row(&snapshot, &ObjectKey::host("gone"), now()).is_none());
    }

    #[test]
    fn a_service_without_its_host_falls_back_to_the_host_name() {
        let orphan = service("vanished", "s", ServiceState::Critical, "CRITICAL");
        let key = orphan.object_key();
        let snapshot = snapshot(Vec::new(), vec![orphan], Vec::new());
        let row = object_row(&snapshot, &key, now()).unwrap();
        assert_eq!(row.host.as_deref(), Some("vanished"));
        assert!(!row.handled);
    }

    #[test]
    fn host_group_headers_carry_the_host() {
        let snapshot = snapshot(
            vec![host("db-prod-03", HostState::Up)],
            Vec::new(),
            Vec::new(),
        );
        let view = View {
            group_by: GroupBy::Host,
            ..View::default()
        };
        let header = group_row(&snapshot, &view, "db-prod-03", 2, now());
        assert_eq!(header.count, "2 problems");
        let host = header.host.unwrap();
        assert_eq!(host.state, CheckableState::Host(HostState::Up));
        assert_eq!(host.since, "41d");

        let by_group = View {
            group_by: GroupBy::HostGroup,
            problems_only: false,
            ..View::default()
        };
        let header = group_row(&snapshot, &by_group, "Production databases", 1, now());
        assert_eq!(header.count, "1 service");
        assert_eq!(header.host, None);
    }

    #[test]
    fn late_checks_are_flagged() {
        let mut snapshot = snapshot(
            vec![host("h", HostState::Up)],
            vec![service("h", "s", ServiceState::Ok, "OK")],
            Vec::new(),
        );
        let key = ObjectKey::service("h", "s");
        assert_eq!(object_row(&snapshot, &key, now()).unwrap().late, None);
        snapshot.late = Arc::new([(key.clone(), ago(12. * 60.))].into_iter().collect());
        assert_eq!(
            object_row(&snapshot, &key, now()).unwrap().late.as_deref(),
            Some("late 12m")
        );
        snapshot.late = Arc::new([(key.clone(), now())].into_iter().collect());
        assert_eq!(
            object_row(&snapshot, &key, now()).unwrap().late.as_deref(),
            Some("late")
        );
        assert_eq!(
            object_row(&snapshot, &ObjectKey::host("h"), now())
                .unwrap()
                .late,
            None
        );
    }

    #[test]
    fn counts_name_what_the_view_lists() {
        let hosts = View {
            object_kind: ObjectKind::Hosts,
            problems_only: false,
            ..View::default()
        };
        assert_eq!(count_label(2, &hosts), "2 hosts");
        assert_eq!(count_label(1, &View::default()), "1 problem");
    }
}
