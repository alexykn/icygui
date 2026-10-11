//! Tests of the state and its outbound half, with a recording core link
//! and a real persistence thread on a temporary directory.

use std::time::{Duration, Instant};

use ic_config::{Appearance, GroupBy, Paths, Sort, SortKey, StateStore};
use ic_core::{LoadPhase, NotificationRecord};
use ic_rules::{NotificationIntent, Tone as IntentTone};

use super::testing::Recorder;
use super::*;
use crate::lists::ListKind;
use crate::persist::Persistence;

fn now() -> Timestamp {
    Timestamp::from_unix_seconds(1_790_000_000.)
}

fn reference(group: &str, dashboard: &str) -> DashboardRef {
    DashboardRef {
        group_id: format!("demo-{group}"),
        dashboard_id: format!("demo-{group}-{dashboard}"),
    }
}

/// The fixture with a recording core, connected.
fn connected_fixture() -> (AppState, Recorder) {
    let mut state = AppState::fixture(now());
    let recorder = Recorder::default();
    state.set_core(Box::new(recorder.clone()));
    (state, recorder)
}

fn api_user(permissions: &[&str]) -> ApiInfo {
    ApiInfo {
        user: "viewer".to_owned(),
        permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
        version: "v2.15.6".to_owned(),
    }
}

/// A live state over the fixture's settings, saving into `dir`.
fn live_in(dir: &std::path::Path, ui: UiState) -> (AppState, Paths) {
    let paths = Paths::in_dir(dir);
    let mut config = fixture::build(now()).config;
    config.active_environment = None;
    let mut state = AppState::live(config, ui, now());
    let persistence =
        Persistence::start(paths.config_store(), paths.state_store(), Box::new(|_| {})).unwrap();
    state.set_persistence(persistence);
    (state, paths)
}

#[test]
fn the_fixture_starts_on_production() {
    let state = AppState::fixture(now());
    assert!(state.is_demo());
    let (group, dashboard) = state.selected_dashboard().unwrap();
    assert_eq!(
        (group.name.as_str(), dashboard.name.as_str()),
        ("overview", "production")
    );
    assert!(state.result(state.selected().unwrap()).is_some());
    assert_eq!(state.environment().unwrap().name, "prod-cluster");
    assert!(state.connection().is_connected());
}

#[test]
fn selecting_validates_the_dashboard() {
    let mut state = AppState::fixture(now());
    assert!(state.select(reference("platform", "network")));
    assert!(!state.select(reference("platform", "network")), "unchanged");
    assert!(!state.select(reference("platform", "no-such-dashboard")));
    assert_eq!(state.selected(), Some(&reference("platform", "network")));
}

#[test]
fn toggling_a_group_flips_its_collapsed_flag() {
    let (mut state, recorder) = connected_fixture();
    assert!(state.toggle_group("demo-lab"));
    let lab = |state: &AppState| {
        state
            .environment()
            .unwrap()
            .groups
            .iter()
            .find(|group| group.id == "demo-lab")
            .unwrap()
            .collapsed
    };
    assert!(lab(&state));
    assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
    assert!(state.toggle_group("demo-lab"));
    assert!(!lab(&state));
    assert!(!state.toggle_group("demo-nope"));
}

#[test]
fn dashboards_are_numbered_in_sidebar_order() {
    let mut state = AppState::fixture(now());
    let names = |state: &AppState| -> Vec<String> {
        (1..=12)
            .map_while(|number| state.dashboard_at(number))
            .map(|reference| state.dashboard(&reference).unwrap().1.name.clone())
            .collect()
    };
    assert_eq!(
        names(&state),
        [
            "overview",
            "production",
            "databases",
            "network",
            "kubernetes",
            "certificates",
            "deploy-checks",
            "sandbox"
        ]
    );
    assert_eq!(state.dashboard_at(0), None, "numbers start at 1");
    state.toggle_group("demo-platform");
    assert_eq!(
        names(&state),
        ["overview", "production", "databases", "sandbox"]
    );
    assert!(AppState::empty().dashboard_at(1).is_none());
}

#[test]
fn tabs_cycle_through_the_dashboard() {
    let mut state = AppState::fixture(now());
    assert!(!state.cycle_tab(true), "no tabs: nothing to cycle");
    let a = ObjectKey::host("db-prod-03");
    let b = ObjectKey::service("db-prod-03", "postgres-replication");
    state.open_tab(a.clone());
    state.open_tab(b.clone());
    assert_eq!(state.active_tab(), Some(&b));
    assert!(state.cycle_tab(true));
    assert_eq!(
        state.active_tab(),
        None,
        "after the last tab: the dashboard"
    );
    assert!(state.cycle_tab(true));
    assert_eq!(state.active_tab(), Some(&a));
    assert!(state.cycle_tab(false));
    assert_eq!(state.active_tab(), None);
    assert!(state.cycle_tab(false));
    assert_eq!(
        state.active_tab(),
        Some(&b),
        "before the dashboard: the last tab"
    );

    assert!(state.show_dashboard());
    assert!(!state.show_dashboard(), "already shown");
    assert_eq!(state.tabs(), [a, b], "the tabs stay open");
}

#[test]
fn the_cluster_section_shows_instead_of_the_dashboard_or_a_tab() {
    use crate::cluster::ClusterEntry;
    let mut state = AppState::fixture(now());
    let host = ObjectKey::host("db-prod-03");
    state.open_tab(host.clone());
    assert!(state.open_list(ListKind::Downtimes));
    assert!(state.open_list(ListKind::Handling));
    assert!(!state.open_list(ListKind::Handling), "shown already");
    assert_eq!(state.active_cluster(), Some(ClusterEntry::Handling));
    assert_eq!(state.active_tab(), None, "one thing is shown");
    // ctrl-tab: the tabs, then back to the dashboard.
    assert!(state.cycle_tab(true));
    assert_eq!(state.active_tab(), Some(&host));
    assert!(state.cycle_tab(true));
    assert_eq!((state.active_tab(), state.active_cluster()), (None, None));
    // The events and health entries are views of the section too.
    assert!(state.show_cluster(ClusterEntry::Events));
    assert_eq!(state.active_cluster(), Some(ClusterEntry::Events));
    assert!(state.show_dashboard());
    assert_eq!(state.active_cluster(), None);
    // The palette's *acknowledged*: handling on its chip.
    assert!(state.open_list_on(ListKind::Handling, crate::lists::model::Chip::Acknowledged));
    assert_eq!(
        state.list_options(ListKind::Handling).chip.as_deref(),
        Some("acknowledged")
    );
    // Selecting a dashboard shows it instead.
    let selected = state.selected().unwrap().clone();
    assert!(state.select(selected));
    assert_eq!(state.active_cluster(), None);
    assert!(state.close_all_tabs());
    assert!(state.tabs().is_empty());
}

#[test]
fn an_empty_state_has_nothing_selected() {
    let state = AppState::empty();
    assert!(!state.is_demo());
    assert!(state.environment().is_none());
    assert!(state.selected_dashboard().is_none());
    assert_eq!(state.connection().health(now()), Health::Idle);
    assert_eq!(state.connection().label(now()), "no environment");
    assert!(state.has_no_objects());
}

#[test]
fn core_events_update_the_state() {
    let mut state = AppState::live(fixture::build(now()).config, UiState::default(), now());
    assert!(
        state.connection().is_starting(),
        "connecting from the start"
    );
    assert_eq!(state.connection().endpoint, "master-01.example.com");
    state.apply(CoreEvent::Connection(ConnectionState::Loading {
        phase: LoadPhase::Hosts,
        done: 2,
        total: Some(8),
    }));
    assert!(state.connection().progress().is_some());
    let snapshot = Arc::new(Snapshot {
        revision: 7,
        last_event_at: Some(now()),
        ..fixture::build(now()).snapshot
    });
    state.apply(CoreEvent::Snapshot(snapshot));
    assert_eq!(state.snapshot().revision, 7);
    assert!(!state.has_no_objects());
    state.apply(CoreEvent::Connection(ConnectionState::Connected {
        node: connection::full_node("master-01"),
        version: "r2.15.6-1".to_owned(),
        since: now(),
    }));
    assert!(state.connection().is_connected());
    assert_eq!(state.connection().label(now()), "master-01 · 0s");
    state.apply(CoreEvent::Permissions(api_user(&["objects/query/*"])));
    assert_eq!(state.permissions().unwrap().user, "viewer");

    let intent = NotificationIntent {
        id: "x".to_owned(),
        object: None,
        title: "14 new problems".to_owned(),
        subtitle: String::new(),
        body: String::new(),
        tone: IntentTone::Info,
        sound: false,
        silent: true,
        silenced: Some(ic_rules::Silence::Paused),
        at: now(),
    };
    state.apply(CoreEvent::Notification(NotificationRecord {
        intent,
        read: false,
    }));
    assert_eq!(state.unread_notifications(), 1);
    assert_eq!(state.notification_records().count(), 1);
    // The pauses are the app's: an engine saying its pause ended lets the
    // app forget the ones that are over, and only those.
    let real_now = Timestamp::now().as_unix_seconds();
    let later = Timestamp::from_unix_seconds(real_now + 3600.);
    state.pause_notifications(Some(later));
    state.apply(CoreEvent::NotificationsPaused(None));
    assert_eq!(state.paused_until(), Some(later));
    state.pause_notifications(Some(Timestamp::from_unix_seconds(real_now - 1.)));
    state.apply(CoreEvent::NotificationsPaused(None));
    assert_eq!(state.paused_until(), None);
}

#[test]
fn view_changes_update_the_core_and_are_saved() {
    let dir = tempfile::tempdir().unwrap();
    let (mut state, paths) = live_in(dir.path(), UiState::default());
    let recorder = Recorder::default();
    state.set_core(Box::new(recorder.clone()));
    let production = reference("overview", "production");
    assert!(state.update_primary_view(&production, |view| {
        view.sort = Sort {
            key: SortKey::Host,
            descending: false,
        };
        view.set_grouping(GroupBy::Host);
        view.handled = ic_config::HandledSetting::SETTINGS;
    }));
    assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
    assert!(
        !state.update_primary_view(&production, |view| view.handled =
            ic_config::HandledSetting::SETTINGS),
        "unchanged: nothing sent"
    );
    assert_eq!(recorder.sent().len(), 1);
    assert!(state.flush_persistence(Duration::from_secs(10)));
    let saved = paths.config_store().load().unwrap();
    let view = &saved
        .environment(fixture::ENVIRONMENT_ID)
        .unwrap()
        .dashboard(&production.group_id, &production.dashboard_id)
        .unwrap()
        .views[0];
    assert_eq!(view.sort.key, SortKey::Host);
    assert_eq!(view.list_grouping(), GroupBy::Host);
    assert_eq!(view.handled, ic_config::HandledSetting::SETTINGS);
    assert_eq!(
        saved.active_environment.as_deref(),
        Some(fixture::ENVIRONMENT_ID),
        "the active environment picked at start was saved too"
    );
}

#[test]
fn tabs_and_selection_persist_and_come_back() {
    let dir = tempfile::tempdir().unwrap();
    let (mut state, paths) = live_in(dir.path(), UiState::default());
    let replication = ObjectKey::service("db-prod-03", "postgres-replication");
    let host = ObjectKey::host("db-prod-03");
    assert!(state.select(reference("platform", "network")));
    assert!(state.open_tab(replication.clone()));
    assert!(state.open_tab(host.clone()));
    assert!(state.close_tab(&host));
    assert!(state.open_tab(host.clone()));
    state.set_window_state(WindowState {
        x: 40.,
        y: 30.,
        width: 1300.,
        height: 820.,
        maximized: false,
    });
    assert!(state.flush_persistence(Duration::from_secs(10)));

    let ui = StateStore::new(paths.state_file()).load().unwrap();
    let saved = ui.environment(fixture::ENVIRONMENT_ID);
    assert_eq!(
        saved.tabs,
        ["db-prod-03!postgres-replication", "db-prod-03"]
    );
    assert_eq!(saved.selected, Some(reference("platform", "network")));
    assert!((ui.window.unwrap().width - 1300.).abs() < f32::EPSILON);

    // The next start restores them (on the dashboard, not a tab).
    let (restored, _) = live_in(dir.path(), ui);
    assert_eq!(restored.tabs(), [replication, host]);
    assert_eq!(restored.active_tab(), None);
    assert_eq!(restored.selected(), Some(&reference("platform", "network")));
    assert!((restored.window_state().unwrap().height - 820.).abs() < f32::EPSILON);
}

#[test]
fn a_saved_selection_that_no_longer_exists_falls_back_to_the_first_dashboard() {
    let mut ui = UiState::default();
    ui.set_environment(
        fixture::ENVIRONMENT_ID,
        EnvironmentUiState {
            tabs: vec!["host!svc".to_owned(), "bad!".to_owned()],
            lists: vec!["acknowledged".to_owned(), "nonsense".to_owned()],
            selected: Some(reference("gone", "gone")),
            ..EnvironmentUiState::default()
        },
    );
    ui.set_environment(
        "deleted-environment",
        EnvironmentUiState {
            tabs: vec!["x".to_owned()],
            lists: Vec::new(),
            selected: None,
            ..EnvironmentUiState::default()
        },
    );
    let state = AppState::live(fixture::build(now()).config, ui, now());
    assert_eq!(state.selected(), Some(&reference("overview", "overview")));
    assert_eq!(state.tabs(), [ObjectKey::service("host", "svc")]);
    assert_eq!(
        state.active_cluster(),
        None,
        "it starts on the dashboard (stage 2's list tabs aren't kept)"
    );
    assert!(
        !state
            .ui_state()
            .environments
            .contains_key("deleted-environment"),
        "state of deleted environments is dropped"
    );
}

#[test]
fn the_demo_and_the_fixture_save_nothing() {
    let state = AppState::demo(crate::live::demo::config(), now());
    assert!(state.is_demo());
    assert_eq!(state.connection().endpoint, crate::live::demo::ENDPOINT);
    assert!(
        state.flush_persistence(Duration::from_millis(10)),
        "nothing to write"
    );
}

#[test]
fn only_the_demos_own_environments_are_labelled_demo() {
    let mut state = AppState::demo(crate::live::demo::config(), now());
    assert!(state.is_demo_environment());
    assert!(state.switch_environment(crate::live::demo::STAGING_ID));
    assert!(state.is_demo_environment());
    // An environment added while the demo runs talks to a real Icinga.
    let real = Environment::new(
        "docker",
        "https://127.0.0.1:5665",
        AuthConfig::Basic {
            username: "icygui".to_owned(),
        },
    );
    let id = real.id.clone();
    state.save_environment(real, false);
    assert!(state.switch_environment(&id));
    assert!(state.is_demo(), "still nothing saved");
    assert!(!state.is_demo_environment(), "but not simulated");
    let live = AppState::live(crate::live::demo::config(), UiState::default(), now());
    assert!(!live.is_demo_environment());
}

#[test]
fn refresh_is_gentle() {
    let (mut state, recorder) = connected_fixture();
    let start = Instant::now();
    assert!(state.refresh(start));
    assert!(
        !state.refresh(start + Duration::from_millis(500)),
        "too soon"
    );
    assert!(state.refresh(start + Duration::from_secs(3)));
    assert_eq!(recorder.sent(), ["Refresh", "Refresh"]);
    recorder.clear();
    state.apply(CoreEvent::Connection(ConnectionState::Connecting {
        attempt: 2,
    }));
    assert!(
        !state.refresh(start + Duration::from_secs(10)),
        "already connecting"
    );
    let mut idle = AppState::fixture(now());
    assert!(!idle.refresh(start), "no core");
}

#[test]
fn hydration_asks_once_and_only_while_connected() {
    let (mut state, recorder) = connected_fixture();
    let start = Instant::now();
    let a = ObjectKey::service("db-prod-03", "postgres-replication");
    let b = ObjectKey::service("mq-prod-01", "rabbitmq-queue");
    assert_eq!(
        state.hydrate(vec![a.clone(), b.clone()], start),
        Hydrated::Sent(vec![a.clone(), b.clone()])
    );
    assert_eq!(state.hydrate(vec![a.clone()], start), Hydrated::Nothing);
    assert_eq!(
        recorder.sent(),
        ["Hydrate(db-prod-03!postgres-replication,mq-prod-01!rabbitmq-queue)"]
    );
    state.apply(CoreEvent::Connection(ConnectionState::Reconnecting {
        error: "gone".to_owned(),
        attempt: 1,
        retry_at: now(),
        untrusted: None,
    }));
    assert_eq!(
        state.hydrate(vec![ObjectKey::service("h", "new")], start),
        Hydrated::NotNow
    );
    assert_eq!(recorder.sent().len(), 1);
}

#[test]
fn actions_the_user_may_not_run_are_refused() {
    let mut state = AppState::fixture(now());
    let target = ObjectKey::service("db-prod-03", "postgres-replication");
    let request = |action| ActionRequest {
        action,
        targets: vec![target.clone()],
        review: false,
    };
    assert!(
        state.request(request(ObjectAction::Acknowledge)).is_ok(),
        "unknown: allowed"
    );
    state.set_permissions(Some(api_user(&[
        "objects/query/*",
        "actions/reschedule-check",
    ])));
    let refused = state
        .request(request(ObjectAction::Acknowledge))
        .unwrap_err();
    assert!(refused.contains("actions/acknowledge-problem"), "{refused}");
    assert_eq!(state.last_denial(), Some(refused.as_str()));
    assert!(state.request(request(ObjectAction::CheckNow)).is_ok());
    assert_eq!(state.last_request().unwrap().action, ObjectAction::CheckNow);
    assert!(state.query_denial(ObjectKind::Services).is_none());
    state.set_permissions(Some(api_user(&["objects/query/Host"])));
    assert!(state.query_denial(ObjectKind::Services).is_some());
    assert_eq!(state.can_read_notifications(), Some(false));
}

#[test]
fn save_errors_show_until_dismissed_or_fixed() {
    let mut state = AppState::fixture(now());
    state.on_saved(SaveReport::Config(Err("read-only file system".to_owned())));
    assert_eq!(state.save_error(), Some("read-only file system"));
    state.dismiss_save_error();
    assert_eq!(state.save_error(), None);
    state.on_saved(SaveReport::Config(Err("disk full".to_owned())));
    assert_eq!(
        state.save_error(),
        Some("disk full"),
        "a new error shows again"
    );
    state.on_saved(SaveReport::Ui(Err("ignored".to_owned())));
    assert_eq!(state.save_error(), Some("disk full"));
    state.on_saved(SaveReport::Config(Ok(())));
    assert_eq!(state.save_error(), None);
}

#[test]
fn recovery_adopts_the_settings_and_connects() {
    let problem = ConfigProblem {
        message: "TOML parse error".to_owned(),
        path: PathBuf::from("/tmp/config.toml"),
        backup: Ok(None),
        newer: false,
        busy: false,
        failure: None,
    };
    let mut state = AppState::recovery(problem, UiState::default(), now());
    assert!(state.config_problem().is_some());
    assert!(state.environment().is_none());
    state.update_config_problem(|problem| problem.busy = true);
    assert!(state.config_problem().unwrap().busy);
    state.adopt_config(fixture::build(now()).config);
    assert!(state.config_problem().is_none());
    assert_eq!(state.environment().unwrap().name, "prod-cluster");
    assert!(state.connection().is_starting());
}

#[test]
fn the_demo_server_address_goes_into_its_environment() {
    let mut state = AppState::demo(crate::live::demo::config(), now());
    state.set_demo_servers(
        crate::live::demo::STAGING_ID,
        &[
            (
                "https://127.0.0.1:41234".to_owned(),
                Some("AB:CD".to_owned()),
            ),
            ("https://127.0.0.1:41235".to_owned(), None),
        ],
    );
    let staging = state
        .environment_by_id(crate::live::demo::STAGING_ID)
        .unwrap();
    assert_eq!(staging.urls.len(), 2);
    assert_eq!(staging.urls[0].url, "https://127.0.0.1:41234");
    assert_eq!(staging.urls[0].pinned_sha256.as_deref(), Some("AB:CD"));
    assert_eq!(staging.urls[1].url, "https://127.0.0.1:41235");
    assert!(!staging.tls.use_system_roots);
    assert_ne!(
        state.environment().unwrap().primary_url(),
        "https://127.0.0.1:41234",
        "the other environments keep theirs"
    );
}

#[test]
fn objects_are_found_in_dashboards() {
    let mut state = AppState::fixture(now());
    let replication = ObjectKey::service("db-prod-03", "postgres-replication");
    assert_eq!(
        state.dashboard_showing(&replication),
        Some(reference("overview", "production")),
        "the selected dashboard first"
    );
    state.select(reference("platform", "network"));
    assert_eq!(
        state.dashboard_showing(&replication),
        Some(reference("overview", "overview")),
        "else the first in sidebar order"
    );
    assert_eq!(state.dashboard_showing(&ObjectKey::host("nowhere")), None);
}

/// Quiet mode's rows (PERF-09) are stale about what changed while quiet:
/// a service that turned critical then is found where its views will
/// list it (the selected `production`, problems only), judged from the
/// object as it is now, not from rows that don't have it yet (which
/// would open it in a dashboard listing everything, or as a tab).
#[test]
fn objects_that_changed_while_quiet_are_found_by_the_views() {
    let mut state = AppState::fixture(now());
    let snapshot = state.snapshot().clone();
    let (key, service) = snapshot
        .services
        .iter()
        .find(|(key, service)| {
            !service.is_problem()
                && snapshot.hosts.get(&key.host).is_some_and(|host| {
                    !host.is_problem()
                        && host.vars.get("env").and_then(|env| env.as_str()) == Some("prod")
                })
        })
        .map(|(key, service)| (key.clone(), service.clone()))
        .expect("an OK service on a prod host");
    let object = ObjectKey::Service { key: key.clone() };
    let production = reference("overview", "production");
    let listed = |state: &AppState, reference: &DashboardRef| {
        state.result(reference).is_some_and(|result| {
            result.views[0]
                .rows()
                .contains(&ic_core::snapshot::DashboardRow::Object(object.clone()))
        })
    };
    assert!(!listed(&state, &production), "OK: not a problem yet");

    // Critical in the store; the rows are still the ones from before.
    let mut critical = (*service).clone();
    critical.state = ic_model::ServiceState::Critical;
    let mut services = (*snapshot.services).clone();
    services.insert(key, Arc::new(critical));
    let changed = |quiet: bool| {
        Arc::new(Snapshot {
            revision: snapshot.revision + 1,
            services: Arc::new(services.clone()),
            quiet,
            ..(*snapshot).clone()
        })
    };
    state.set_snapshot(changed(true));
    assert!(state.rows_settling());
    assert!(!listed(&state, &production), "the rows are stale");
    assert_eq!(state.dashboard_showing(&object), Some(production.clone()));

    // A live snapshot's rows are what the list shows.
    state.set_snapshot(changed(false));
    assert!(!state.rows_settling());
    assert_ne!(state.dashboard_showing(&object), Some(production));
}

#[test]
fn object_names_parse() {
    assert_eq!(
        parse_object("db-prod-03"),
        Some(ObjectKey::host("db-prod-03"))
    );
    assert_eq!(
        parse_object(" db-prod-03!disk / "),
        Some(ObjectKey::service("db-prod-03", "disk /"))
    );
    assert_eq!(parse_object("!broken"), None);
    assert_eq!(parse_object(""), None);
}

#[test]
fn shutting_down_takes_the_core() {
    let (mut state, recorder) = connected_fixture();
    let core = state.take_core().unwrap();
    drop(core.shutdown_in_background());
    assert!(*recorder.stopped.borrow());
    assert!(state.take_core().is_none());
    // At quit every engine's link goes.
    let (mut state, _) = connected_fixture();
    assert_eq!(state.take_all_cores().len(), 1);
    assert!(state.take_all_cores().is_empty());
}

#[test]
fn view_changes_wait_while_the_engine_waits_to_reconnect() {
    let (mut state, recorder) = connected_fixture();
    let production = reference("overview", "production");
    state.apply(CoreEvent::Connection(ConnectionState::Reconnecting {
        error: "refused".to_owned(),
        attempt: 3,
        retry_at: now(),
        untrusted: None,
    }));
    assert!(state.update_primary_view(&production, |view| view.handled =
        ic_config::HandledSetting::SETTINGS));
    assert!(state.update_primary_view(&production, |view| view.set_grouping(GroupBy::Host)));
    assert!(state.toggle_group("demo-lab"));
    assert!(
        recorder.sent().is_empty(),
        "an update would skip the backoff: {:?}",
        recorder.sent()
    );
    // The engine connects by itself: one update with all of them.
    state.apply(CoreEvent::Connection(ConnectionState::Connecting {
        attempt: 4,
    }));
    assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);
    state.apply(CoreEvent::Connection(ConnectionState::Connected {
        node: connection::full_node("master-01"),
        version: String::new(),
        since: now(),
    }));
    assert_eq!(recorder.sent().len(), 1, "sent once");
    // After a refused login too.
    state.apply(CoreEvent::Connection(ConnectionState::AuthFailed {
        message: "401".to_owned(),
    }));
    assert!(state.update_primary_view(&production, |view| view.handled =
        ic_config::HandledSetting::SHOW));
    assert_eq!(recorder.sent().len(), 1);
}

#[test]
fn views_change_by_id_and_the_handled_button_toggles() {
    let (mut state, recorder) = connected_fixture();
    let production = reference("overview", "production");
    let view_id = state.dashboard(&production).unwrap().1.views[0].id.clone();
    let rows = |state: &AppState| -> Vec<String> {
        state
            .view_result(&production, &view_id)
            .unwrap()
            .rows()
            .iter()
            .filter_map(|row| match row {
                ic_core::snapshot::DashboardRow::Object(key) => Some(key.full_name()),
                ic_core::snapshot::DashboardRow::Group { .. } => None,
            })
            .collect()
    };
    // rc1's production showed its handled problems.
    let acknowledged = "web-edge-02!http-tls".to_owned();
    assert!(rows(&state).contains(&acknowledged));
    let shown = state.view_result(&production, &view_id).unwrap().clone();
    assert_eq!(shown.hidden, 0);
    assert!(shown.handled > 0);

    // `N handled · hide`: back to the settings, which hide them.
    assert!(state.toggle_handled(&production, &view_id));
    let view = &state.dashboard(&production).unwrap().1.views[0];
    assert_eq!(view.handled, ic_config::HandledSetting::SETTINGS);
    assert!(!rows(&state).contains(&acknowledged));
    let hidden = state.view_result(&production, &view_id).unwrap().clone();
    assert_eq!(hidden.hidden, shown.handled, "every handled one is hidden");
    assert_eq!(hidden.counts, shown.counts, "the counts don't change");
    assert_eq!(recorder.sent(), ["UpdateEnvironment(prod-cluster)"]);

    // `N hidden · show`.
    assert!(state.toggle_handled(&production, &view_id));
    assert!(rows(&state).contains(&acknowledged));
    // Unknown views and dashboards change nothing.
    assert!(!state.update_view(&production, "no such view", |view| view.collapsed = true));
    assert!(!state.toggle_handled(&reference("overview", "gone"), &view_id));
    assert_eq!(recorder.sent().len(), 2);
}

#[test]
fn handled_defaults_reach_every_engine_and_the_views_that_follow_them() {
    let (mut state, recorder) = connected_fixture();
    let overview = reference("overview", "overview");
    let view_id = state.dashboard(&overview).unwrap().1.views[0].id.clone();
    let hidden_before = state.view_result(&overview, &view_id).unwrap().hidden;
    assert!(hidden_before > 0, "the overview hides handled problems");
    let appearance = Appearance {
        hide_handled: HideHandled::NONE,
        ..*state.appearance()
    };
    assert!(state.set_appearance(appearance));
    assert_eq!(state.handled_defaults(), HideHandled::NONE);
    let sent = recorder.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].starts_with("SetHandledDefaults("), "{sent:?}");
    // The fixture evaluates again: the overview follows the settings.
    assert_eq!(state.view_result(&overview, &view_id).unwrap().hidden, 0);
    assert!(!state.set_appearance(appearance), "unchanged");
    assert_eq!(recorder.sent().len(), 1);
}

#[test]
fn would_list_follows_the_handled_kinds() {
    let state = AppState::fixture(now());
    let snapshot = state.snapshot().clone();
    let key = ObjectKey::Service {
        key: ServiceKey::new("web-edge-02", "http-tls"),
    };
    let service = &snapshot.services[key.as_service().unwrap()];
    assert!(service.check.acknowledgement.is_acknowledged());
    assert!(!service.check.in_downtime());
    let view = View {
        handled: ic_config::HandledSetting::SETTINGS,
        ..View::default()
    };
    let all = HideHandled::ALL;
    let at = Timestamp::now();
    assert!(!would_list(&snapshot, &view, all, &key, at));
    let downtimes_only = HideHandled {
        acknowledged: false,
        ..all
    };
    assert!(would_list(&snapshot, &view, downtimes_only, &key, at));
    let shown = View {
        handled: ic_config::HandledSetting::SHOW,
        ..view.clone()
    };
    assert!(would_list(&snapshot, &shown, all, &key, at));
    // Only lists list anything.
    let grid = View {
        display: ic_config::ViewDisplay::HostGroupGrid,
        ..shown
    };
    assert!(!would_list(&snapshot, &grid, all, &key, at));
}

#[test]
fn the_handled_button_hides_even_when_the_settings_hide_nothing() {
    use ic_config::{HandledMode, HandledSetting};
    let none = HideHandled::NONE;
    // `2 handled · hide` on a view that follows settings hiding nothing.
    let hidden = handled_after_click(HandledSetting::SETTINGS, none, 0);
    assert_eq!(hidden.mode, HandledMode::Hide);
    assert_eq!(hidden.hidden(none), HideHandled::ALL);
    // `2 hidden · show` shows them again.
    let shown = handled_after_click(hidden, none, 2);
    assert_eq!(shown.hidden(none), none);
    assert_eq!(
        handled_after_click(shown, none, 0).hidden(none),
        HideHandled::ALL
    );
}
