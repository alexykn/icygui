//! The whole app against a real core: `--demo`'s in-process mock Icinga
//! through `ic-core`, its events pumped into the window, the outbound half
//! (view changes, hydration) answered by the core, the demo's faults as
//! banners, and the recovery of an unreadable settings file.

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AppContext as _, Entity};
use ic_config::{AuthConfig, Config, Environment, GroupBy, Paths, Sort, SortKey};
use ic_core::ports::{SecretError, SecretStore};
use ic_core::snapshot::DashboardRow;
use ic_model::{ObjectKey, Timestamp};
use secrecy::SecretString;

use super::{Body, run_app, wait_for};
use crate::app_state::hydration::row_needs_details;
use crate::app_state::{AppState, NoticeKind};
use crate::live::demo::{self, DemoFault, DemoOptions};
use crate::live::{self, Launch, RecoveryChoice, Session};

/// Generous: the debug build loads the demo in about a second.
pub(super) const LOAD: Duration = Duration::from_secs(40);

/// The demo (`scenario`, with `fault`), started.
pub(super) fn demo_app(
    scenario: &'static str,
    fault: Option<DemoFault>,
) -> impl FnOnce(&mut App) -> Entity<AppState> + 'static {
    move |cx| {
        let state = cx.new(|_| AppState::demo(demo::config(), Timestamp::now()));
        let session = Session::install(
            state.clone(),
            Launch::Demo {
                options: DemoOptions {
                    scenario: scenario.to_owned(),
                    seed: 3,
                    fault,
                    storm_every: None,
                },
            },
            None,
            cx,
        );
        session.update(cx, Session::start);
        state
    }
}

fn dashboard(state: &AppState, name: &str) -> ic_rules::DashboardRef {
    state.dashboard_named(name).unwrap()
}

/// The object keys of the selected dashboard's rows.
pub(super) fn row_keys(state: &AppState) -> Vec<ObjectKey> {
    let Some(result) = state
        .selected()
        .and_then(|reference| state.result(reference))
    else {
        return Vec::new();
    };
    result
        .rows
        .iter()
        .filter_map(|row| match row {
            DashboardRow::Object(key) => Some(key.clone()),
            DashboardRow::Group { .. } => None,
        })
        .collect()
}

#[test]
fn the_demo_connects_through_the_core_and_fills_the_window() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app("prod-cluster", None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the first load", LOAD, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected() && !row_keys(state).is_empty()
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert!(state.is_demo());
                    assert!(
                        state
                            .connection()
                            .label(Timestamp::now())
                            .starts_with("master-01 · "),
                        "{}",
                        state.connection().label(Timestamp::now())
                    );
                    assert_eq!(state.connection_notice(Timestamp::now()), None);
                    assert!(state.permissions().is_some(), "the core reported the user");
                    assert!(
                        row_keys(state)
                            .contains(&ObjectKey::service("db-prod-03", "postgres-replication")),
                        "the design's problem is listed"
                    );
                });
                // Opening an object from outside (a notification's click)
                // shows it in a dashboard that lists it.
                cx.update(|cx| {
                    assert!(live::open_object(
                        &ObjectKey::service("mq-prod-01", "rabbitmq-queue"),
                        cx
                    ));
                    app.draw(cx);
                    assert_eq!(
                        app.pane_object(cx),
                        Some(ObjectKey::service("mq-prod-01", "rabbitmq-queue"))
                    );
                });
            }
            .boxed_local()
        })),
    );
}

#[test]
fn view_changes_are_evaluated_by_the_core_and_rows_get_their_details() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app("prod-cluster", None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the first load", LOAD, |app, cx| {
                    app.state.read(cx).connection().is_connected()
                })
                .await;
                // Sort the overview by host: the core re-evaluates it.
                cx.update(|cx| {
                    app.state.update(cx, |state, cx| {
                        let overview = dashboard(state, "overview");
                        state.select(overview.clone());
                        assert!(state.update_view(&overview, |view| {
                            view.sort = Sort {
                                key: SortKey::Host,
                                descending: false,
                            };
                        }));
                        cx.notify();
                    });
                });
                wait_for(&app, &cx, "rows sorted by host", LOAD, |app, cx| {
                    let hosts: Vec<String> = row_keys(app.state.read(cx))
                        .iter()
                        .map(|key| key.host_name().to_string())
                        .collect();
                    !hosts.is_empty() && hosts.windows(2).all(|pair| pair[0] <= pair[1])
                })
                .await;

                // All services, scrolled to the end: the OK services there
                // are lean until the rows on screen ask for their details.
                cx.update(|cx| {
                    app.state.update(cx, |state, cx| {
                        let all = dashboard(state, "all services");
                        state.select(all);
                        cx.notify();
                    });
                    app.draw(cx);
                    app.keys(cx, "end");
                });
                let lean_on_screen = |app: &super::Harness, cx: &mut App| {
                    let dashboard = app.dashboard(cx);
                    let visible = dashboard.read(cx).visible_rows();
                    let state = app.state.read(cx);
                    let rows = state
                        .selected()
                        .and_then(|reference| state.result(reference))
                        .map(|result| result.rows.clone())
                        .unwrap_or_default();
                    visible
                        .filter_map(|index| match rows.get(index) {
                            Some(DashboardRow::Object(key)) => Some(key.clone()),
                            _ => None,
                        })
                        .filter(|key| row_needs_details(state.snapshot(), key))
                        .count()
                };
                let before = cx.update(|cx| lean_on_screen(&app, cx));
                assert!(before > 0, "the OK services at the end start lean");
                wait_for(&app, &cx, "the rows on screen hydrated", LOAD, |app, cx| {
                    lean_on_screen(app, cx) == 0
                })
                .await;
            }
            .boxed_local()
        })),
    );
}

/// Whether `rows` are well-formed group sections: each header first, with
/// the number of object rows under it. Returns the headers' labels.
fn group_sections(rows: &[DashboardRow]) -> Option<Vec<String>> {
    let mut labels = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        let DashboardRow::Group { label, count } = &rows[index] else {
            return None;
        };
        let objects = rows[index + 1..]
            .iter()
            .take_while(|row| matches!(row, DashboardRow::Object(_)))
            .count();
        if objects != *count || objects == 0 {
            return None;
        }
        labels.push(label.clone());
        index += 1 + objects;
    }
    Some(labels)
}

#[test]
fn group_by_shows_the_cores_group_headers() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app("prod-cluster", None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the first load", LOAD, |app, cx| {
                    app.state.read(cx).connection().is_connected()
                })
                .await;
                let mut previous: Vec<String> = Vec::new();
                for group_by in [GroupBy::HostGroup, GroupBy::ServiceGroup, GroupBy::Host] {
                    cx.update(|cx| {
                        app.state.update(cx, |state, cx| {
                            let all = dashboard(state, "all services");
                            state.select(all.clone());
                            assert!(state.update_view(&all, |view| view.group_by = group_by));
                            cx.notify();
                        });
                    });
                    let before = previous.clone();
                    wait_for(&app, &cx, "new group headers", LOAD, move |app, cx| {
                        let state = app.state.read(cx);
                        state
                            .selected()
                            .and_then(|reference| state.result(reference))
                            .and_then(|result| group_sections(&result.rows))
                            .is_some_and(|labels| labels.len() > 1 && labels != before)
                    })
                    .await;
                    cx.update(|cx| {
                        app.draw(cx);
                        let state = app.state.read(cx);
                        let result = state
                            .selected()
                            .and_then(|reference| state.result(reference))
                            .unwrap();
                        let labels = group_sections(&result.rows).unwrap();
                        // A group shows once, under its display name.
                        let mut unique = labels.clone();
                        unique.sort();
                        unique.dedup();
                        assert_eq!(unique.len(), labels.len(), "{labels:?}");
                        let expected = match group_by {
                            GroupBy::HostGroup => "linux-servers",
                            GroupBy::Host => "db-prod-03",
                            _ => "",
                        };
                        assert!(
                            expected.is_empty() || labels.iter().any(|label| label == expected),
                            "{labels:?}"
                        );
                        previous = labels;
                    });
                }
            }
            .boxed_local()
        })),
    );
}

/// The demo server's control, once the demo runs.
pub(super) fn control(cx: &mut App) -> ic_mock::MockControl {
    live::session(cx)
        .and_then(|session| session.read(cx).demo_control())
        .expect("the demo server runs")
}

#[test]
fn changes_in_icinga_show_within_seconds_and_lost_connections_come_back() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app("prod-cluster", None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the first load", LOAD, |app, cx| {
                    app.state.read(cx).connection().is_connected()
                })
                .await;
                // A check goes critical in Icinga: the overview lists it
                // (LIVE-01: the event stream, no polling).
                let ssh = ObjectKey::service("db-prod-01", "ssh");
                cx.update(|cx| {
                    assert!(!row_keys(app.state.read(cx)).contains(&ssh));
                    control(cx)
                        .set_service_state(
                            "db-prod-01",
                            "ssh",
                            ic_model::ServiceState::Critical,
                            "CRITICAL - connection refused",
                            true,
                        )
                        .unwrap();
                });
                let changed = std::time::Instant::now();
                wait_for(&app, &cx, "the new problem listed", LOAD, |app, cx| {
                    row_keys(app.state.read(cx)).contains(&ssh)
                })
                .await;
                let delay = changed.elapsed();
                assert!(delay < Duration::from_secs(10), "took {delay:?}");

                // The connection drops and Icinga answers 503: the banner
                // counts down; once Icinga is back, "Retry now" connects.
                cx.update(|cx| {
                    let control = control(cx);
                    control.fail_next(u32::MAX, 503);
                    control.drop_connections();
                });
                wait_for(&app, &cx, "the reconnecting banner", LOAD, |app, cx| {
                    app.state
                        .read(cx)
                        .connection_notice(Timestamp::now())
                        .is_some_and(|notice| {
                            notice.kind == NoticeKind::Reconnecting
                                && notice.title.starts_with("Connection to master-01 lost.")
                        })
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert!(!state.has_no_objects(), "the last known objects stay");
                    assert!(row_keys(state).contains(&ssh));
                    control(cx).fail_next(0, 503);
                    app.state.update(cx, |state, _| {
                        assert!(state.refresh(std::time::Instant::now()), "Retry now");
                    });
                });
                wait_for(&app, &cx, "connected again", LOAD, |app, cx| {
                    app.state.read(cx).connection().is_connected()
                })
                .await;
            }
            .boxed_local()
        })),
    );
}

#[test]
fn the_demos_faults_show_their_banners() {
    let cases = [
        (DemoFault::Auth, NoticeKind::AuthFailed),
        (DemoFault::Tls, NoticeKind::TlsFailed),
        (DemoFault::MissingSecret, NoticeKind::MissingSecret),
        (DemoFault::Misconfigured, NoticeKind::Misconfigured),
        (DemoFault::Offline, NoticeKind::Reconnecting),
    ];
    for (fault, kind) in cases {
        run_app(
            crate::WINDOW_SIZE,
            demo_app("lab", Some(fault)),
            Body::Async(Box::new(move |app, cx| {
                async move {
                    wait_for(
                        &app,
                        &cx,
                        &format!("the {kind:?} notice"),
                        LOAD,
                        |app, cx| {
                            app.state
                                .read(cx)
                                .connection_notice(Timestamp::now())
                                .is_some_and(|notice| notice.kind == kind)
                        },
                    )
                    .await;
                    cx.update(|cx| {
                        let state = app.state.read(cx);
                        assert!(state.has_no_objects(), "nothing loaded: the body says it");
                        if kind == NoticeKind::AuthFailed {
                            let title = state
                                .connection_notice(Timestamp::now())
                                .map(|notice| notice.title)
                                .unwrap_or_default();
                            assert_eq!(title, "Icinga refused the login of icygui.");
                        }
                        if kind == NoticeKind::TlsFailed {
                            let detail = state
                                .connection_notice(Timestamp::now())
                                .and_then(|notice| notice.detail)
                                .unwrap_or_default();
                            assert!(detail.contains("SHA-256"), "{detail}");
                        }
                    });
                }
                .boxed_local()
            })),
        );
    }
}

/// A password store in memory (the keychain isn't there in tests).
struct MemorySecrets(Option<String>);

impl SecretStore for MemorySecrets {
    fn get(&self, _account: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(self.0.clone().map(SecretString::from))
    }

    fn set(&self, _account: &str, _secret: &SecretString) -> Result<(), SecretError> {
        Err(SecretError::new("read-only"))
    }

    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Ok(())
    }
}

/// A live app over `paths` with the password `secret`, recovering if its
/// settings can't be read.
fn live_app(
    paths: Paths,
    secret: Option<String>,
) -> impl FnOnce(&mut App) -> Entity<AppState> + 'static {
    move |cx| {
        let ui = paths.state_store().load().unwrap_or_default();
        let state = match live::load_settings(&paths.config_store()) {
            Ok(config) => AppState::live(config, ui, Timestamp::now()),
            Err(problem) => AppState::recovery(*problem, ui, Timestamp::now()),
        };
        let recovering = state.config_problem().is_some();
        let state = cx.new(|_| state);
        let secrets = Arc::new(MemorySecrets(secret));
        let session = Session::install(state.clone(), Launch::Live { paths, secrets }, None, cx);
        if !recovering {
            session.update(cx, Session::start);
        }
        state
    }
}

/// Settings with one environment that can't connect (its certificate files
/// don't exist), so no keychain and no network are needed.
fn settings_with(name: &str) -> Config {
    let mut environment = Environment::new(
        name,
        "https://127.0.0.1:1",
        AuthConfig::ClientCertificate {
            cert_path: "/nonexistent/client.crt".into(),
            key_path: "/nonexistent/client.key".into(),
        },
    );
    environment.author = Some("test".to_owned());
    Config {
        active_environment: Some(environment.id.clone()),
        environments: vec![environment],
        ..Config::default()
    }
}

/// ENV-07: an engine that dies (its event stream ends while the session
/// didn't stop it) shows a banner and the footer's "stopped" instead of
/// stale data as live; "Restart" brings a new engine.
#[test]
fn an_engine_that_dies_says_so_and_restarts() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app("prod-cluster", None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the first load", LOAD, |app, cx| {
                    app.state.read(cx).connection().is_connected()
                })
                .await;
                cx.update(|cx| {
                    let session = live::session(cx).unwrap();
                    session.update(cx, Session::end_event_stream);
                });
                wait_for(&app, &cx, "the stopped engine", LOAD, |app, cx| {
                    app.state
                        .read(cx)
                        .connection_notice(Timestamp::now())
                        .is_some_and(|notice| {
                            notice.kind == NoticeKind::EngineFailed
                                && notice.title.contains("stopped")
                        })
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert!(
                        state
                            .connection()
                            .label(Timestamp::now())
                            .ends_with("· stopped"),
                        "{}",
                        state.connection().label(Timestamp::now())
                    );
                    // The banner's Restart.
                    app.in_window(cx, |window, cx| {
                        window.dispatch_action(Box::new(crate::actions::RestartEngine), cx);
                    });
                });
                wait_for(&app, &cx, "the new engine's load", LOAD, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected()
                        && state.connection_notice(Timestamp::now()).is_none()
                })
                .await;
            }
            .boxed_local()
        })),
    );
}

#[test]
fn an_unreadable_settings_file_can_be_restored_from_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let store = paths.config_store();
    store.save(&settings_with("restored")).unwrap();
    // The next save makes that the backup; then the file breaks.
    store.save(&settings_with("current")).unwrap();
    std::fs::write(store.path(), "environments = [ broken").unwrap();
    let check = paths.clone();
    run_app(
        crate::WINDOW_SIZE,
        live_app(paths, None),
        Body::Async(Box::new(move |app, cx| {
            async move {
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    let problem = state.config_problem().expect("the recovery screen");
                    assert!(matches!(&problem.backup, Ok(Some(_))));
                    assert!(state.environment().is_none());
                });
                cx.update(|cx| {
                    let session = live::session(cx).unwrap();
                    session.update(cx, |session, cx| {
                        session.resolve_config(RecoveryChoice::RestoreBackup, cx);
                    });
                });
                wait_for(&app, &cx, "the restored settings", LOAD, |app, cx| {
                    app.state.read(cx).config_problem().is_none()
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert_eq!(state.environment().unwrap().name, "restored");
                });
                // It connects: the engine reports the missing certificate.
                wait_for(&app, &cx, "the engine's verdict", LOAD, |app, cx| {
                    app.state
                        .read(cx)
                        .connection_notice(Timestamp::now())
                        .is_some_and(|notice| notice.kind == NoticeKind::Misconfigured)
                })
                .await;
            }
            .boxed_local()
        })),
    );
    let saved = check.config_store().load().unwrap();
    assert_eq!(saved.environments[0].name, "restored");
    let kept = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().contains(".unreadable-"));
    assert!(kept, "the broken file is kept as a copy");
}

#[test]
fn an_unreadable_settings_file_can_be_started_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let store = paths.config_store();
    // A good backup, then a file from a newer icygui.
    store.save(&settings_with("good")).unwrap();
    store.save(&settings_with("current")).unwrap();
    std::fs::write(paths.config_file.clone(), "version = 99\n").unwrap();
    let check = paths.clone();
    run_app(
        crate::WINDOW_SIZE,
        live_app(paths, None),
        Body::Async(Box::new(move |app, cx| {
            async move {
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    let problem = state.config_problem().expect("the recovery screen");
                    assert!(problem.newer, "written by a newer icygui");
                    // Retrying an unchanged file keeps the screen up.
                    let session = live::session(cx).unwrap();
                    session.update(cx, |session, cx| {
                        session.resolve_config(RecoveryChoice::Retry, cx);
                    });
                });
                wait_for(&app, &cx, "the retry", LOAD, |app, cx| {
                    app.state
                        .read(cx)
                        .config_problem()
                        .is_some_and(|problem| !problem.busy && problem.failure.is_some())
                })
                .await;
                cx.update(|cx| {
                    let session = live::session(cx).unwrap();
                    session.update(cx, |session, cx| {
                        session.resolve_config(RecoveryChoice::StartFresh, cx);
                    });
                });
                wait_for(&app, &cx, "fresh settings", LOAD, |app, cx| {
                    app.state.read(cx).config_problem().is_none()
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert!(state.environment().is_none(), "no environment yet");
                    assert!(state.has_no_objects());
                });
            }
            .boxed_local()
        })),
    );
    assert_eq!(check.config_store().load().unwrap(), Config::default());
    let backup = check.config_store().load_backup().unwrap().unwrap();
    assert_eq!(
        backup.environments[0].name, "good",
        "starting fresh leaves the last good backup to restore later"
    );
}

/// The disposable Icinga 2.15.6 from `contract/run-icinga.sh`, read-only:
/// the app connects as the `icygui` user, loads, and lists its problems.
/// Runs only when `ICYGUI_CONTRACT_URL` is set, and refuses any Icinga but
/// that fixture (on this machine, with its fixture-only `viewer` user)
/// before connecting; it sends only queries.
#[test]
fn a_real_icinga_loads_read_only() {
    let Ok(url) = std::env::var("ICYGUI_CONTRACT_URL") else {
        return;
    };
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"));
    let environment = |user: &str| {
        let mut environment = Environment::new(
            "contract",
            &url,
            AuthConfig::Basic {
                username: user.to_owned(),
            },
        );
        environment.tls.use_system_roots = false;
        environment.tls.ca_file = Some(var("ICYGUI_CONTRACT_CA_FILE").into());
        environment.tls.server_name = Some(var("ICYGUI_CONTRACT_SERVER_NAME"));
        environment
    };
    let api_url = environment("viewer").api_url().unwrap();
    let host = api_url.host_str().unwrap_or_default();
    assert!(
        host == "localhost" || host == "127.0.0.1" || host == "[::1]",
        "ICYGUI_CONTRACT_URL must be the disposable Icinga on this machine"
    );
    let viewer = futures::executor::block_on(ic_core::test_connection(
        environment("viewer"),
        Some(SecretString::from("viewer-test")),
    ));
    assert!(
        matches!(&viewer, Ok(Ok(report)) if report.info.user == "viewer"),
        "not the contract fixture: its viewer user can't log in"
    );
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let user = environment(&var("ICYGUI_CONTRACT_USER"));
    paths
        .config_store()
        .save(&Config {
            active_environment: Some(user.id.clone()),
            environments: vec![user],
            ..Config::default()
        })
        .unwrap();
    run_app(
        crate::WINDOW_SIZE,
        live_app(paths, Some(var("ICYGUI_CONTRACT_PASSWORD"))),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the real Icinga loaded", LOAD, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected() && !state.has_no_objects()
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert!(state.permissions().is_some());
                    assert_eq!(state.connection_notice(Timestamp::now()), None);
                    assert!(
                        state
                            .selected()
                            .and_then(|reference| state.result(reference))
                            .is_some()
                    );
                });
            }
            .boxed_local()
        })),
    );
}
