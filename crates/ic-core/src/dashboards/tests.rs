use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ic_config::{Dashboard, DashboardGroup, Environment, GroupBy, ObjectKind, Sort, SortKey, View};
use ic_model::{
    AckKind, CheckableState, Host, HostGroup, HostState, ObjectKey, Service, ServiceGroup,
    ServiceState, Timestamp,
};
use ic_rules::DashboardRef;
use serde_json::json;

use super::*;
use crate::snapshot::DashboardRow;
use crate::store::Changes;

fn host(name: &str, state: HostState, groups: &[&str], role: &str) -> Host {
    let mut host = Host::new(name);
    host.state = state;
    host.groups = groups.iter().map(|g| (*g).to_owned()).collect();
    host.vars.insert("role".to_owned(), json!(role));
    host
}

fn service(host: &str, name: &str, state: ServiceState, since: f64) -> Service {
    let mut service = Service::new(host, name);
    service.state = state;
    service.check.last_state_change = Timestamp::from_unix_seconds(since);
    service
}

fn data(hosts: Vec<Host>, services: Vec<Service>) -> Data {
    Data {
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
        host_groups: Arc::new(vec![
            HostGroup {
                name: "db".to_owned(),
                display_name: "Databases".to_owned(),
            },
            HostGroup {
                name: "web".to_owned(),
                display_name: "Web servers".to_owned(),
            },
        ]),
        service_groups: Arc::new(vec![ServiceGroup {
            name: "disks".to_owned(),
            display_name: "Disks".to_owned(),
        }]),
        now: Timestamp::from_unix_seconds(10_000.0),
    }
}

/// The scenario most tests share:
/// - db-1 (up, groups db): pg CRITICAL (acknowledged), disk WARNING, ssh OK
/// - db-2 (down, groups db, web): pg CRITICAL (handled by the host), ssh OK
/// - web-1 (up, groups web): http UNKNOWN, ssh OK
/// - lone (up, no groups): ssh WARNING
fn sample() -> Data {
    let mut pg = service("db-1", "pg", ServiceState::Critical, 100.0);
    pg.check.acknowledgement = AckKind::Normal;
    let mut disk = service("db-1", "disk", ServiceState::Warning, 300.0);
    disk.groups = vec!["disks".to_owned()];
    data(
        vec![
            host("db-1", HostState::Up, &["db"], "db"),
            host("db-2", HostState::Down, &["db", "web"], "db"),
            host("web-1", HostState::Up, &["web"], "web"),
            host("lone", HostState::Up, &[], "misc"),
        ],
        vec![
            pg,
            disk,
            service("db-1", "ssh", ServiceState::Ok, 50.0),
            service("db-2", "pg", ServiceState::Critical, 200.0),
            service("db-2", "ssh", ServiceState::Ok, 50.0),
            service("web-1", "http", ServiceState::Unknown, 400.0),
            service("web-1", "ssh", ServiceState::Ok, 50.0),
            service("lone", "ssh", ServiceState::Warning, 500.0),
        ],
    )
}

fn view(filter: &str) -> View {
    View {
        object_kind: ObjectKind::Services,
        filter: filter.to_owned(),
        problems_only: false,
        hide_handled: false,
        sort: Sort::default(),
        group_by: GroupBy::None,
    }
}

fn reference(id: &str) -> DashboardRef {
    DashboardRef {
        group_id: "g".to_owned(),
        dashboard_id: id.to_owned(),
    }
}

fn environment(views: &[(&str, View)]) -> Environment {
    Environment {
        groups: vec![DashboardGroup {
            id: "g".to_owned(),
            name: "group".to_owned(),
            dashboards: views
                .iter()
                .map(|(id, view)| Dashboard {
                    id: (*id).to_owned(),
                    name: (*id).to_owned(),
                    view: view.clone(),
                    ..Dashboard::default()
                })
                .collect(),
            ..DashboardGroup::default()
        }],
        ..Environment::default()
    }
}

fn all() -> Changes {
    Changes {
        any: true,
        all: true,
        ..Changes::default()
    }
}

fn some(objects: &[ObjectKey]) -> Changes {
    Changes {
        any: true,
        objects: objects.iter().cloned().collect(),
        ..Changes::default()
    }
}

fn evaluate(views: &[(&str, View)], data: &Data) -> Dashboards {
    let mut dashboards = Dashboards::default();
    dashboards.configure(&environment(views));
    dashboards.update(data, &all(), false, &AtomicBool::new(false));
    dashboards
}

fn result<'a>(dashboards: &'a Dashboards, id: &str) -> &'a DashboardResult {
    &dashboards.results()[&reference(id)]
}

/// Rows as `host!service` names, groups as `# label (count)`.
fn names(result: &DashboardResult) -> Vec<String> {
    result
        .rows
        .iter()
        .map(|row| match row {
            DashboardRow::Group { label, count } => format!("# {label} ({count})"),
            DashboardRow::Object(object) => object.to_string(),
        })
        .collect()
}

fn s(host: &str, name: &str) -> ObjectKey {
    ObjectKey::service(host, name)
}

#[test]
fn filters_select_members_and_display_flags_select_rows() {
    let data = sample();
    let mut problems = view("host.vars.role == \"db\"");
    problems.problems_only = true;
    problems.hide_handled = true;
    let mut all_db = view("host.vars.role == \"db\"");
    all_db.problems_only = false;
    let dashboards = evaluate(&[("problems", problems), ("all", all_db)], &data);

    // Only db-1's warning is an unhandled problem: pg is acknowledged and
    // db-2's services are handled by their host's problem.
    assert_eq!(names(result(&dashboards, "problems")), ["db-1!disk"]);
    assert_eq!(
        names(result(&dashboards, "all")),
        ["db-2!pg", "db-1!disk", "db-1!pg", "db-1!ssh", "db-2!ssh"],
        "by severity (a host problem doesn't lower it, an acknowledgement does), then by host"
    );

    // Membership ignores problems_only and hide_handled.
    for id in ["problems", "all"] {
        assert!(
            dashboards
                .memberships(&s("db-2", "ssh"))
                .contains(&reference(id))
        );
    }
    assert!(dashboards.memberships(&s("web-1", "http")).is_empty());

    // The summary counts every member, before the display flags.
    let summary = result(&dashboards, "problems").summary;
    assert_eq!(summary, result(&dashboards, "all").summary);
    assert_eq!(
        summary,
        Summary {
            critical: 2,
            warning: 1,
            ok: 2,
            handled: 2,
            unhandled: 1,
            worst_unhandled: Some(CheckableState::Service(ServiceState::Warning)),
            ..Summary::default()
        }
    );
    assert_eq!(result(&dashboards, "problems").error, None);
}

#[test]
fn sorts_with_the_tie_breaks() {
    let mut data = sample();
    // Same severity and last change as db-1!disk: the host name decides.
    let services = Arc::make_mut(&mut data.services);
    let twin = service("a-host", "disk", ServiceState::Warning, 300.0);
    services.insert(twin.key.clone(), Arc::new(twin));
    Arc::make_mut(&mut data.hosts).insert(
        "a-host".into(),
        Arc::new(host("a-host", HostState::Up, &[], "x")),
    );
    let sorted = |key: SortKey, descending: bool| {
        let mut v = view("service.state != 0");
        v.sort = Sort { key, descending };
        names(result(&evaluate(&[("d", v)], &data), "d"))
    };
    assert_eq!(
        sorted(SortKey::Severity, true),
        [
            "db-2!pg",
            "web-1!http",
            "lone!ssh",
            "a-host!disk",
            "db-1!disk",
            "db-1!pg"
        ],
        "unhandled first (critical, unknown, warning newest first, then by host), the acknowledged critical last"
    );
    assert_eq!(
        sorted(SortKey::Severity, false),
        [
            "db-1!pg",
            "lone!ssh",
            "a-host!disk",
            "db-1!disk",
            "web-1!http",
            "db-2!pg"
        ],
        "ascending turns only the primary key around"
    );
    assert_eq!(
        sorted(SortKey::LastStateChange, true),
        [
            "lone!ssh",
            "web-1!http",
            "a-host!disk",
            "db-1!disk",
            "db-2!pg",
            "db-1!pg"
        ]
    );
    assert_eq!(
        sorted(SortKey::Host, false),
        [
            "a-host!disk",
            "db-1!disk",
            "db-1!pg",
            "db-2!pg",
            "lone!ssh",
            "web-1!http"
        ],
        "by host, then severity within a host"
    );
    assert_eq!(
        sorted(SortKey::Service, true),
        [
            "lone!ssh",
            "db-2!pg",
            "db-1!pg",
            "web-1!http",
            "a-host!disk",
            "db-1!disk"
        ]
    );
}

#[test]
fn host_views_evaluate_hosts() {
    let data = sample();
    let mut hosts = view("\"db\" in host.groups");
    hosts.object_kind = ObjectKind::Hosts;
    hosts.sort = Sort {
        key: SortKey::Service,
        descending: false,
    };
    let dashboards = evaluate(&[("h", hosts)], &data);
    let result = result(&dashboards, "h");
    assert_eq!(names(result), ["db-1", "db-2"], "service sort = host name");
    assert_eq!(result.summary.down, 1);
    assert_eq!(result.summary.ok, 1);
    assert_eq!(
        result.summary.worst_unhandled,
        Some(CheckableState::Host(HostState::Down))
    );
}

#[test]
fn group_by_orders_groups_by_worst_severity_with_ungrouped_last() {
    let data = sample();
    let mut by_host_group = view("service.state != 0");
    by_host_group.group_by = GroupBy::HostGroup;
    let mut by_service_group = view("service.state != 0");
    by_service_group.group_by = GroupBy::ServiceGroup;
    let mut by_host = view("service.state != 0");
    by_host.group_by = GroupBy::Host;
    let dashboards = evaluate(
        &[
            ("hg", by_host_group),
            ("sg", by_service_group),
            ("h", by_host),
        ],
        &data,
    );
    assert_eq!(
        names(result(&dashboards, "hg")),
        [
            // db-2!pg (unhandled critical) is in both groups.
            "# Databases (3)",
            "db-2!pg",
            "db-1!disk",
            "db-1!pg",
            "# Web servers (2)",
            "db-2!pg",
            "web-1!http",
            "# ungrouped (1)",
            "lone!ssh",
        ]
    );
    assert_eq!(
        names(result(&dashboards, "sg")),
        [
            "# Disks (1)",
            "db-1!disk",
            "# ungrouped (4)",
            "db-2!pg",
            "web-1!http",
            "lone!ssh",
            "db-1!pg",
        ]
    );
    assert_eq!(
        names(result(&dashboards, "h")),
        [
            "# db-2 (1)",
            "db-2!pg",
            "# web-1 (1)",
            "web-1!http",
            "# db-1 (2)",
            "db-1!disk",
            "db-1!pg",
            "# lone (1)",
            "lone!ssh",
        ],
        "groups with the same worst severity go by label"
    );
}

#[test]
fn filter_errors_empty_the_rows_and_name_the_object() {
    let data = sample();
    let dashboards = evaluate(
        &[("bad", view("service.state ==")), ("ok", view(""))],
        &data,
    );
    let bad = result(&dashboards, "bad");
    let error = bad.error.as_deref().unwrap();
    assert!(error.contains("line 1, column"), "{error}");
    assert!(bad.rows.is_empty());
    assert_eq!(bad.summary, Summary::default());
    assert_eq!(result(&dashboards, "ok").rows.len(), 8);

    // Fails for one object only: the dashboard shows the error until the
    // object changes.
    let mut data = data;
    let mut odd = host("web-1", HostState::Up, &["web"], "web");
    odd.vars.insert("limit".to_owned(), json!("high"));
    Arc::make_mut(&mut data.hosts).insert("web-1".into(), Arc::new(odd));
    let mut dashboards = evaluate(&[("limit", view("host.vars.limit < 5"))], &data);
    let error = result(&dashboards, "limit").error.clone().unwrap();
    assert!(error.contains("web-1!"), "{error}");
    assert!(
        error.contains("1 other object"),
        "both web-1 services: {error}"
    );
    Arc::make_mut(&mut data.hosts).insert(
        "web-1".into(),
        Arc::new(host("web-1", HostState::Up, &["web"], "web")),
    );
    dashboards.update(
        &data,
        &some(&[ObjectKey::host("web-1")]),
        false,
        &AtomicBool::new(false),
    );
    let fixed = result(&dashboards, "limit");
    assert_eq!(fixed.error, None);
    assert_eq!(fixed.rows.len(), 8, "null < 5 holds (null counts as 0)");
}

/// Applies `change` to `data` and updates incrementally; the result must
/// equal a full evaluation of the new data.
fn check_incremental(
    dashboards: &mut Dashboards,
    views: &[(&str, View)],
    data: &mut Data,
    change: impl FnOnce(&mut Data) -> Vec<ObjectKey>,
) {
    let dirty = change(data);
    dashboards.update(data, &some(&dirty), false, &AtomicBool::new(false));
    let full = evaluate(views, data);
    assert_eq!(**dashboards.results(), **full.results());
    for (id, _) in views {
        let reference = reference(id);
        for object in data
            .hosts
            .keys()
            .map(|name| ObjectKey::host(name.as_str()))
            .chain(data.services.keys().cloned().map(ObjectKey::from))
        {
            assert_eq!(
                dashboards.memberships(&object).contains(&reference),
                full.memberships(&object).contains(&reference),
                "{id} {object}"
            );
        }
    }
}

#[test]
fn incremental_updates_match_full_evaluations() {
    let mut problems = view("host.vars.role == \"db\" || service.name == \"http\"");
    problems.problems_only = true;
    problems.hide_handled = true;
    let mut grouped = view("");
    grouped.group_by = GroupBy::HostGroup;
    grouped.sort = Sort {
        key: SortKey::LastStateChange,
        descending: true,
    };
    let mut hosts = view("host.state != 0 || host.vars.role == \"web\"");
    hosts.object_kind = ObjectKind::Hosts;
    let views = [("p", problems), ("g", grouped), ("h", hosts)];
    let mut data = sample();
    let mut dashboards = evaluate(&views, &data);

    // A state change.
    check_incremental(&mut dashboards, &views, &mut data, |data| {
        let services = Arc::make_mut(&mut data.services);
        let key = ServiceKey::new("db-1", "ssh");
        let mut ssh = (*services[&key]).clone();
        ssh.state = ServiceState::Critical;
        ssh.check.last_state_change = Timestamp::from_unix_seconds(900.0);
        services.insert(key.clone(), Arc::new(ssh));
        vec![key.into()]
    });
    // A host recovering un-handles its services.
    check_incremental(&mut dashboards, &views, &mut data, |data| {
        Arc::make_mut(&mut data.hosts).insert(
            "db-2".into(),
            Arc::new(host("db-2", HostState::Up, &["db", "web"], "db")),
        );
        vec![ObjectKey::host("db-2")]
    });
    // An acknowledgement, a group change, a new and a deleted service.
    check_incremental(&mut dashboards, &views, &mut data, |data| {
        let services = Arc::make_mut(&mut data.services);
        let key = ServiceKey::new("lone", "ssh");
        let mut ssh = (*services[&key]).clone();
        ssh.check.acknowledgement = AckKind::Sticky;
        services.insert(key.clone(), Arc::new(ssh));
        let new = service("web-1", "tls", ServiceState::Critical, 50.0);
        services.insert(new.key.clone(), Arc::new(new.clone()));
        services.remove(&ServiceKey::new("web-1", "ssh"));
        Arc::make_mut(&mut data.hosts).insert(
            "lone".into(),
            Arc::new(host("lone", HostState::Up, &["web"], "misc")),
        );
        vec![
            key.into(),
            new.object_key(),
            ObjectKey::service("web-1", "ssh"),
            ObjectKey::host("lone"),
        ]
    });
    // A host and its services disappear.
    check_incremental(&mut dashboards, &views, &mut data, |data| {
        Arc::make_mut(&mut data.hosts).remove(&HostName::new("db-1"));
        let services = Arc::make_mut(&mut data.services);
        let gone: Vec<ServiceKey> = services
            .keys()
            .filter(|key| key.host.as_str() == "db-1")
            .cloned()
            .collect();
        for key in &gone {
            services.remove(key);
        }
        gone.into_iter()
            .map(ObjectKey::from)
            .chain([ObjectKey::host("db-1")])
            .collect()
    });
}

#[test]
fn untouched_dashboards_keep_their_rows() {
    let views = [("p", view("service.state != 0")), ("all", view(""))];
    let mut data = sample();
    let mut dashboards = evaluate(&views, &data);
    let before = Arc::clone(dashboards.results());
    let rows = Arc::clone(&result(&dashboards, "p").rows);

    // A new check result that changes nothing a dashboard shows.
    let services = Arc::make_mut(&mut data.services);
    let key = ServiceKey::new("db-1", "ssh");
    let mut ssh = (*services[&key]).clone();
    ssh.check.attempt = 1;
    ssh.check.result = Some(ic_model::CheckResult {
        output: "SSH OK".to_owned(),
        ..ic_model::CheckResult::default()
    });
    services.insert(key.clone(), Arc::new(ssh));
    let after = dashboards.update(&data, &some(&[key.into()]), false, &AtomicBool::new(false));
    assert!(Arc::ptr_eq(&before, &after), "nothing changed");
    assert!(Arc::ptr_eq(&rows, &result(&dashboards, "p").rows));

    // Nothing changed at all.
    let after = dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert!(Arc::ptr_eq(&before, &after));
}

#[test]
fn display_settings_restyle_without_re_evaluating() {
    let mut data = sample();
    let mut v = view("service.state != 0");
    let mut dashboards = evaluate(&[("d", v.clone())], &data);
    assert_eq!(result(&dashboards, "d").rows.len(), 5);

    // A filter that would match less, sneaked into the data without
    // telling: a restyle must not notice it, a recompile must.
    let services = Arc::make_mut(&mut data.services);
    services.remove(&ServiceKey::new("lone", "ssh"));
    v.hide_handled = true;
    dashboards.configure(&environment(&[("d", v.clone())]));
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert_eq!(
        names(result(&dashboards, "d")),
        ["web-1!http", "lone!ssh", "db-1!disk"],
        "handled ones hidden, members kept"
    );

    v.filter = "service.state == 2".to_owned();
    dashboards.configure(&environment(&[("d", v.clone())]));
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert!(
        result(&dashboards, "d").rows.is_empty(),
        "both criticals are handled"
    );
    assert_eq!(result(&dashboards, "d").summary.critical, 2);

    // Removing the dashboard drops its result.
    dashboards.configure(&environment(&[]));
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert!(dashboards.results().is_empty());
}

#[test]
fn filters_on_the_time_refresh_with_it() {
    let mut data = sample();
    let recent = view("service.last_state_change > get_time() - 9700");
    let mut dashboards = evaluate(&[("recent", recent)], &data);
    assert!(dashboards.time_dependent());
    // now = 10 000: changes after 300.
    assert_eq!(
        names(result(&dashboards, "recent")),
        ["web-1!http", "lone!ssh"]
    );
    data.now = Timestamp::from_unix_seconds(10_150.0);
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert_eq!(result(&dashboards, "recent").rows.len(), 2, "not refreshed");
    dashboards.update(&data, &Changes::default(), true, &AtomicBool::new(false));
    assert_eq!(names(result(&dashboards, "recent")), ["lone!ssh"]);
    assert!(!evaluate(&[("x", view(""))], &data).time_dependent());
}

#[test]
fn previews_evaluate_unsaved_views() {
    let data = sample();
    let mut v = view("host.name == \"web-1\"");
    v.problems_only = true;
    let preview = preview(&v, &data).unwrap();
    assert_eq!(names(&preview), ["web-1!http"]);
    assert_eq!(preview.summary.ok, 1);
    let error = super::preview(&view("(("), &data).unwrap_err();
    assert!(error.contains("line 1"), "{error}");
    let error = super::preview(&view("nope(1)"), &data).unwrap_err();
    assert!(error.contains("nope"), "{error}");
}

#[test]
fn duplicate_ids_and_cancellation() {
    let data = sample();
    let mut dashboards = Dashboards::default();
    dashboards.configure(&environment(&[
        ("d", view("")),
        ("d", view("service.state == 2")),
    ]));
    dashboards.update(&data, &all(), false, &AtomicBool::new(false));
    assert_eq!(dashboards.results().len(), 1);
    assert_eq!(result(&dashboards, "d").rows.len(), 8, "the first one wins");

    let mut cancelled = Dashboards::default();
    cancelled.configure(&environment(&[("d", view(""))]));
    let results = cancelled.update(&data, &all(), false, &AtomicBool::new(true));
    assert!(results.is_empty(), "stopped before evaluating");
}
