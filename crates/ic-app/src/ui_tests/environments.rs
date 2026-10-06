//! Environments through the real core: the first run's onboarding form
//! with "Test connection" (ENV-08, ENV-02, ENV-04), switching from the
//! footer and the palette (ENV-01), trusting a certificate (ENV-05) and
//! deleting an environment with its password and event log (ENV-03).
//! The Icinga is the demo's in-process mock; passwords live in memory.

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AppContext as _, Entity, Modifiers, point, px};
use ic_config::{AuthConfig, Config, Environment, Paths};
use ic_core::ports::{SecretError, SecretStore};
use ic_core::snapshot::DashboardRow;
use ic_model::Timestamp;
use secrecy::{ExposeSecret as _, SecretString};

use super::{Body, Harness, run, run_app, wait_for};
use crate::app_state::AppState;
use crate::environments::form::FormField;
use crate::environments::{EnvironmentEditor, EnvironmentEditorEvent};
use crate::fixture::FixtureOptions;
use crate::live::demo::{self, DemoEndpoint, DemoFault, DemoOptions, DemoSecrets, DemoServer};
use crate::live::{self, Launch, Session};
use crate::workspace::{Confirmed, ModalKind};

/// Generous: the debug build connects to the mock in about a second.
const CONNECT: Duration = Duration::from_secs(40);

/// A running mock Icinga (the demo's `lab` scenario) and where it is.
fn mock_icinga() -> (DemoServer, DemoEndpoint, SecretString) {
    let (mut server, ready) = demo::start(&DemoOptions {
        scenario: "lab".to_owned(),
        seed: 5,
        fault: None,
        storm_every: None,
    })
    .unwrap();
    let (endpoint, _control) = futures::executor::block_on(ready).unwrap().unwrap();
    server.set_endpoint(endpoint.clone());
    let password = server.core_password().unwrap();
    (server, endpoint, password)
}

/// A live app over `paths` with `secrets` as its keychain.
fn live_app(
    paths: Paths,
    secrets: Arc<dyn SecretStore>,
) -> impl FnOnce(&mut App) -> Entity<AppState> + 'static {
    move |cx| {
        let ui = paths.state_store().load().unwrap_or_default();
        let config = live::load_settings(&paths.config_store()).unwrap();
        let state = cx.new(|_| AppState::live(config, ui, Timestamp::now()));
        let session = Session::install(state.clone(), Launch::Live { paths, secrets }, None, cx);
        session.update(cx, Session::start);
        state
    }
}

/// The demo, started (`fault` shows a connection failure).
fn demo_app(fault: Option<DemoFault>) -> impl FnOnce(&mut App) -> Entity<AppState> + 'static {
    move |cx| {
        let state = cx.new(|_| AppState::demo(demo::config(), Timestamp::now()));
        let session = Session::install(
            state.clone(),
            Launch::Demo {
                options: DemoOptions {
                    scenario: "prod-cluster".to_owned(),
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

/// Sets a form field's text, as typing would.
fn fill(
    app: &Harness,
    cx: &mut App,
    editor: &Entity<EnvironmentEditor>,
    field: FormField,
    text: &str,
) {
    let input = editor.read(cx).input(field).clone();
    app.in_window(cx, |window, cx| {
        input.update(cx, |input, cx| input.replace_all(text, window, cx));
    });
}

fn connected_to(state: &AppState, endpoint: &str) -> bool {
    state.connection().is_connected() && state.connection().endpoint == endpoint
}

fn has_rows(state: &AppState) -> bool {
    state
        .selected()
        .and_then(|reference| state.result(reference))
        .is_some_and(|result| {
            result
                .rows
                .iter()
                .any(|row| matches!(row, DashboardRow::Object(_)))
        })
}

#[test]
fn the_first_environment_is_added_from_the_onboarding_form() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let secrets = Arc::new(DemoSecrets::default());
    let (_server, endpoint, password) = mock_icinga();
    let keychain = secrets.clone();
    let config_file = paths.config_store().path().to_path_buf();
    run_app(
        crate::WINDOW_SIZE,
        live_app(paths, secrets.clone()),
        Body::Async(Box::new(move |app, cx| {
            async move {
                let editor = cx.update(|cx| {
                    let workspace = app.workspace.read(cx);
                    assert!(workspace.modal(cx).is_none());
                    workspace.onboarding().expect("the onboarding form").clone()
                });
                cx.update(|cx| {
                    fill(&app, cx, &editor, FormField::Name, "lab");
                    fill(&app, cx, &editor, FormField::Url, &endpoint.url);
                    fill(&app, cx, &editor, FormField::Username, "icygui");
                    fill(
                        &app,
                        cx,
                        &editor,
                        FormField::Password,
                        password.expose_secret(),
                    );
                });
                // Without a pin the self-signed certificate isn't trusted:
                // the test says so and offers it.
                cx.update(|cx| editor.update(cx, EnvironmentEditor::test));
                wait_for(&app, &cx, "the test's answer", CONNECT, |_, cx| {
                    editor.read(cx).test_result().is_some()
                })
                .await;
                cx.update(|cx| {
                    let result = editor.read(cx).test_result().unwrap().clone();
                    match result {
                        Err(ic_core::ConnectionFailure::Tls { certificate, .. }) => {
                            let certificate = certificate.expect("the presented certificate");
                            assert_eq!(certificate.fingerprint(), endpoint.fingerprint);
                        }
                        other => panic!("expected a TLS failure, got {other:?}"),
                    }
                    // "Trust this certificate" pins it and shows the pin.
                    let fingerprint = endpoint.fingerprint.clone();
                    app.in_window(cx, |window, cx| {
                        editor.update(cx, |editor, cx| {
                            editor.trust_presented(fingerprint, window, cx);
                        });
                    });
                    assert_eq!(editor.read(cx).form().pinned, endpoint.fingerprint);
                });
                cx.update(|cx| editor.update(cx, EnvironmentEditor::test));
                wait_for(&app, &cx, "a successful test", CONNECT, |_, cx| {
                    editor.read(cx).test_result().is_some_and(Result::is_ok)
                })
                .await;
                let id = cx.update(|cx| {
                    let Some(Ok(report)) = editor.read(cx).test_result().cloned() else {
                        panic!("no report");
                    };
                    assert_eq!(report.info.user, "icygui");
                    assert!(report.missing_permissions.is_empty());
                    editor.read(cx).form().id.clone()
                });
                cx.update(|cx| editor.update(cx, EnvironmentEditor::save));
                wait_for(&app, &cx, "the connection", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected() && has_rows(state)
                })
                .await;
                cx.update(|cx| {
                    assert!(app.workspace.read(cx).onboarding().is_none());
                    let state = app.state.read(cx);
                    let environment = state.environment().unwrap();
                    assert_eq!(environment.id, id);
                    assert_eq!(environment.name, "lab");
                    assert_eq!(environment.groups.len(), 1, "the default dashboards");
                    assert_eq!(app.workspace.read(cx).title(), "lab — icygui");
                });
                // The password is in the keychain, never in the file.
                let stored = keychain.get(&id).unwrap().unwrap();
                assert_eq!(stored.expose_secret(), password.expose_secret());
                cx.update(|cx| {
                    assert!(app.state.read(cx).flush_persistence(Duration::from_secs(5)));
                });
                let text = std::fs::read_to_string(&config_file).unwrap();
                assert!(text.contains("lab"));
                assert!(!text.contains(password.expose_secret()));
            }
            .boxed_local()
        })),
    );
}

#[test]
fn environments_are_switched_from_the_footer_and_the_palette() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "prod-cluster", CONNECT, |app, cx| {
                    connected_to(app.state.read(cx), demo::ENDPOINT)
                })
                .await;
                let prod_selection = cx.update(|cx| {
                    // Select another dashboard to see it come back.
                    let state = app.state.read(cx);
                    let network = state.dashboard_named("network").unwrap();
                    app.state.update(cx, |state, cx| {
                        state.select(network.clone());
                        cx.notify();
                    });
                    network
                });
                // The footer status opens the switcher.
                cx.update(|cx| app.click(cx, point(px(150.), px(880.)), Modifiers::default()));
                cx.update(|cx| {
                    let sidebar = app.workspace.read(cx).sidebar().clone();
                    assert!(sidebar.read(cx).details_open());
                    // `staging`, third from the bottom of the menu.
                    app.click(cx, point(px(130.), px(750.)), Modifiers::default());
                });
                wait_for(&app, &cx, "staging", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    connected_to(state, "stg-master-01") && has_rows(state)
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert_eq!(state.active_environment_id(), Some(demo::STAGING_ID));
                    assert_eq!(state.environment().unwrap().groups.len(), 1);
                    assert!(
                        state
                            .snapshot()
                            .hosts
                            .keys()
                            .all(|host| !host.as_str().starts_with("db-prod")),
                        "prod-cluster's objects are gone"
                    );
                    assert_eq!(app.workspace.read(cx).title(), "staging (demo) — icygui");
                });
                // And back with the palette.
                cx.update(|cx| app.keys(cx, "ctrl-k"));
                cx.update(|cx| {
                    let palette = app.workspace.read(cx).palette().unwrap().clone();
                    let input = palette.read(cx).input().clone();
                    app.in_window(cx, |window, cx| {
                        input.update(cx, |input, cx| {
                            input.replace_all("switch to prod", window, cx);
                        });
                    });
                });
                cx.update(|cx| app.keys(cx, "enter"));
                wait_for(&app, &cx, "prod-cluster again", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    connected_to(state, demo::ENDPOINT) && has_rows(state)
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert_eq!(
                        state.selected(),
                        Some(&prod_selection),
                        "its selection is back"
                    );
                    assert!(
                        state
                            .snapshot()
                            .services
                            .contains_key(&ic_model::ServiceKey::new(
                                "db-prod-03",
                                "postgres-replication"
                            ))
                    );
                });
            }
            .boxed_local()
        })),
    );
}

#[test]
fn an_untrusted_certificate_is_trusted_after_review() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(Some(DemoFault::Tls)),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the certificate failure", CONNECT, |app, cx| {
                    matches!(
                        app.state.read(cx).connection().state,
                        Some(ic_core::ConnectionState::TlsFailed {
                            certificate: Some(_),
                            ..
                        })
                    )
                })
                .await;
                // The body's "Review certificate" button.
                cx.update(|cx| app.click(cx, point(px(820.), px(529.)), Modifiers::default()));
                let fingerprint = cx.update(|cx| {
                    assert_eq!(
                        app.workspace.read(cx).modal(cx),
                        Some(ModalKind::Certificate)
                    );
                    let Some(ic_core::ConnectionState::TlsFailed {
                        certificate: Some(certificate),
                        ..
                    }) = app.state.read(cx).connection().state.clone()
                    else {
                        panic!("the failure went away");
                    };
                    certificate.fingerprint()
                });
                // "trust this certificate" (once the dialog is drawn).
                cx.update(|cx| {
                    app.draw(cx);
                    app.click(cx, point(px(885.), px(615.)), Modifiers::default());
                });
                wait_for(&app, &cx, "the trusted connection", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected() && has_rows(state)
                })
                .await;
                cx.update(|cx| {
                    assert_eq!(app.workspace.read(cx).modal(cx), None);
                    let environment = app.state.read(cx).environment().unwrap().clone();
                    assert_eq!(
                        environment.tls.pinned_sha256.as_deref(),
                        Some(fingerprint.as_str())
                    );
                });
            }
            .boxed_local()
        })),
    );
}

/// An action dialog or the settings opened for one environment close when
/// another becomes active (the tray switches without touching the
/// window): nothing meant for prod-cluster reaches staging, whose hosts
/// may have the same names.
#[test]
fn dialogs_for_one_environment_close_when_another_becomes_active() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "prod-cluster", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    connected_to(state, demo::ENDPOINT) && has_rows(state)
                })
                .await;
                let object = ic_model::ObjectKey::service("db-prod-03", "postgres-replication");
                cx.update(|cx| {
                    app.state.update(cx, |state, cx| {
                        let _ = state.request(crate::actions::ActionRequest {
                            action: crate::actions::ObjectAction::AddComment,
                            targets: vec![object.clone()],
                        });
                        cx.notify();
                    });
                });
                cx.update(|cx| {
                    app.draw(cx);
                    assert_eq!(
                        app.workspace.read(cx).modal(cx),
                        Some(ModalKind::Action(
                            crate::operate::dialog::DialogKind::Comment
                        ))
                    );
                });
                // What the tray's "switch environment" does.
                cx.update(|cx| {
                    let session = live::session(cx).unwrap();
                    session.update(cx, |session, cx| {
                        session.switch_environment(demo::STAGING_ID, cx);
                    });
                });
                cx.update(|cx| {
                    app.draw(cx);
                    assert_eq!(
                        app.workspace.read(cx).modal(cx),
                        None,
                        "the comment for prod-cluster can't be sent to staging"
                    );
                });
                // The settings, opened in staging, close on the way back.
                cx.update(|cx| {
                    app.in_window(cx, |window, cx| {
                        window.dispatch_action(Box::new(crate::actions::OpenSettings), cx);
                    });
                });
                cx.update(|cx| {
                    app.draw(cx);
                    assert_eq!(app.workspace.read(cx).modal(cx), Some(ModalKind::Settings));
                    let session = live::session(cx).unwrap();
                    session.update(cx, |session, cx| {
                        session.switch_environment(demo::ENVIRONMENT_ID, cx);
                    });
                });
                cx.update(|cx| {
                    app.draw(cx);
                    assert_eq!(app.workspace.read(cx).modal(cx), None);
                });
                wait_for(&app, &cx, "prod-cluster again", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    connected_to(state, demo::ENDPOINT) && has_rows(state)
                })
                .await;
            }
            .boxed_local()
        })),
    );
}

/// ENV-05's pin mismatch: the environment pins another certificate than
/// the one the server presents. The editor's test and the banner's review
/// both show the two fingerprints, and trusting the new one replaces the
/// pin and connects.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: the failure, the editor's test, the review, the trust"
)]
fn a_pin_mismatch_shows_both_fingerprints_and_trusts_the_new_certificate() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(Some(DemoFault::PinMismatch)),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the certificate failure", CONNECT, |app, cx| {
                    matches!(
                        app.state.read(cx).connection().state,
                        Some(ic_core::ConnectionState::TlsFailed {
                            certificate: Some(_),
                            ..
                        })
                    )
                })
                .await;
                let (presented, id) = cx.update(|cx| {
                    let state = app.state.read(cx);
                    let Some(ic_core::ConnectionState::TlsFailed {
                        certificate: Some(certificate),
                        ..
                    }) = state.connection().state.clone()
                    else {
                        panic!("the failure went away");
                    };
                    let environment = state.environment().unwrap();
                    assert_eq!(
                        environment.tls.pinned_sha256.as_deref(),
                        Some(demo::other_pin().as_str())
                    );
                    (certificate.fingerprint(), environment.id.clone())
                });
                assert_ne!(presented, demo::other_pin());

                // The editor's "Test connection" reports the mismatch.
                cx.update(|cx| {
                    app.in_window(cx, |window, cx| {
                        app.workspace.update(cx, |workspace, cx| {
                            workspace.open_environment_editor(Some(&id), window, cx);
                        });
                    });
                });
                let editor = cx.update(|cx| {
                    app.draw(cx);
                    let editor = app.workspace.read(cx).environment_editor().unwrap().clone();
                    editor.update(cx, EnvironmentEditor::test);
                    editor
                });
                wait_for(&app, &cx, "the test's answer", CONNECT, |_, cx| {
                    editor.read(cx).test_result().is_some()
                })
                .await;
                cx.update(|cx| {
                    match editor.read(cx).test_result().unwrap() {
                        Err(ic_core::ConnectionFailure::CertificateMismatch {
                            expected,
                            actual,
                        }) => {
                            assert!(crate::environments::certificate::same_fingerprint(
                                expected,
                                &ic_config::parse_fingerprint(&demo::other_pin()).unwrap()
                            ));
                            assert_eq!(
                                ic_config::parse_fingerprint(actual).unwrap(),
                                ic_config::parse_fingerprint(&presented).unwrap()
                            );
                        }
                        other => panic!("expected a pin mismatch, got {other:?}"),
                    }
                    app.in_window(cx, |window, cx| {
                        app.workspace
                            .update(cx, |workspace, cx| workspace.close_modal(window, cx));
                    });
                });

                // The banner's review: both fingerprints, the danger button.
                cx.update(|cx| {
                    app.in_window(cx, |window, cx| {
                        window.dispatch_action(Box::new(crate::actions::ReviewCertificate), cx);
                    });
                    app.draw(cx);
                });
                let review = cx.update(|cx| {
                    assert_eq!(
                        app.workspace.read(cx).modal(cx),
                        Some(ModalKind::Certificate)
                    );
                    let review = app.workspace.read(cx).certificate_review().unwrap().clone();
                    let (environment, offer) = review.read(cx).offer(cx).unwrap();
                    assert_eq!(environment, id);
                    assert_eq!(offer.fingerprint, presented);
                    assert_eq!(offer.replaces, Some(demo::other_pin()));
                    review
                });
                cx.update(|cx| {
                    review.update(
                        cx,
                        crate::environments::certificate::CertificateReview::trust,
                    );
                });
                wait_for(&app, &cx, "the trusted connection", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    state.connection().is_connected() && has_rows(state)
                })
                .await;
                cx.update(|cx| {
                    assert_eq!(app.workspace.read(cx).modal(cx), None);
                    let environment = app.state.read(cx).environment().unwrap().clone();
                    assert_eq!(
                        environment.tls.pinned_sha256.as_deref(),
                        Some(presented.as_str()),
                        "the new certificate replaced the pin"
                    );
                });
            }
            .boxed_local()
        })),
    );
}

#[test]
fn a_deleted_environment_takes_its_password_and_event_log_along() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    paths.create_dirs().unwrap();
    let (_server, endpoint, password) = mock_icinga();
    let environment = |name: &str| {
        let mut environment = Environment::new(
            name,
            &endpoint.url,
            AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        environment.tls.pinned_sha256 = Some(endpoint.fingerprint.clone());
        environment.tls.use_system_roots = false;
        environment
    };
    let (first, second) = (environment("lab-a"), environment("lab-b"));
    let (first_id, second_id) = (first.id.clone(), second.id.clone());
    let config = Config {
        active_environment: Some(first_id.clone()),
        environments: vec![first, second],
        ..Config::default()
    };
    paths.config_store().save(&config).unwrap();
    let secrets = Arc::new(DemoSecrets::default());
    secrets.put(&first_id, Some(password.clone()));
    secrets.put(&second_id, Some(password));
    let keychain = secrets.clone();
    let log = ic_core::event_log_path(&paths.data_dir, &first_id);
    run_app(
        crate::WINDOW_SIZE,
        live_app(paths, secrets.clone()),
        Body::Async(Box::new(move |app, cx| {
            async move {
                wait_for(&app, &cx, "lab-a", CONNECT, |app, cx| {
                    app.state.read(cx).connection().is_connected()
                })
                .await;
                assert!(log.exists(), "the engine keeps an event log");
                // edit lab-a… → delete environment… → confirm.
                cx.update(|cx| {
                    app.in_window(cx, |window, cx| {
                        window.dispatch_action(Box::new(crate::actions::EditEnvironment), cx);
                    });
                });
                cx.update(|cx| {
                    let editor = app.workspace.read(cx).environment_editor().unwrap().clone();
                    let id = first_id.clone();
                    editor.update(cx, |_, cx| cx.emit(EnvironmentEditorEvent::Delete(id)));
                });
                cx.update(|cx| {
                    let Some(ModalKind::Confirm(confirmation)) = app.workspace.read(cx).modal(cx)
                    else {
                        panic!("no confirmation");
                    };
                    assert_eq!(
                        confirmation.action,
                        Confirmed::Environment(first_id.clone())
                    );
                    app.keys(cx, "enter");
                });
                wait_for(&app, &cx, "lab-b", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    state.active_environment_id() == Some(second_id.as_str())
                        && state.connection().is_connected()
                })
                .await;
                wait_for(
                    &app,
                    &cx,
                    "the clean-up",
                    Duration::from_secs(10),
                    |_, _| !log.exists(),
                )
                .await;
                assert!(
                    keychain.get(&first_id).unwrap().is_none(),
                    "the password is gone"
                );
                assert!(keychain.get(&second_id).unwrap().is_some());
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert_eq!(state.environments().len(), 1);
                    assert!(state.environment_by_id(&first_id).is_none());
                });
            }
            .boxed_local()
        })),
    );
}

#[test]
fn the_environment_editor_names_what_is_missing() {
    run(FixtureOptions::default(), |app, cx| {
        let workspace = app.workspace.clone();
        app.in_window(cx, |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_environment_editor(None, window, cx);
            });
        });
        app.draw(cx);
        let editor = app.workspace.read(cx).environment_editor().unwrap().clone();
        assert!(
            editor.read(cx).shown_issues().is_empty(),
            "nothing shown before saving"
        );
        // Enter in a field saves: the problems show, nothing is saved.
        app.keys(cx, "enter");
        let issues = editor.read(cx).shown_issues();
        for field in [
            FormField::Name,
            FormField::Url,
            FormField::Username,
            FormField::Password,
        ] {
            assert!(issues.contains_key(&field), "{field:?}: {issues:?}");
        }
        assert_eq!(
            app.workspace.read(cx).modal(cx),
            Some(ModalKind::Environment)
        );
        assert_eq!(app.state.read(cx).environments().len(), 1);
        // Editing the existing one starts from its settings.
        app.keys(cx, "escape");
        let id = app
            .state
            .read(cx)
            .active_environment_id()
            .unwrap()
            .to_owned();
        app.in_window(cx, |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_environment_editor(Some(&id), window, cx);
            });
        });
        app.draw(cx);
        let editor = app.workspace.read(cx).environment_editor().unwrap().clone();
        let form = editor.read(cx).form().clone();
        assert_eq!(form.name, "prod-cluster");
        assert!(!form.is_new());
        assert!(form.issues().is_empty(), "{:?}", form.issues());
    });
}

/// A keychain that refuses to store anything (locked, or no Secret
/// Service running).
struct LockedKeychain;

impl SecretStore for LockedKeychain {
    fn get(&self, _account: &str) -> Result<Option<SecretString>, SecretError> {
        Ok(None)
    }

    fn set(&self, _account: &str, _secret: &SecretString) -> Result<(), SecretError> {
        Err(SecretError::new("the keychain is locked"))
    }

    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Ok(())
    }
}

#[test]
fn a_password_the_keychain_refuses_keeps_the_form_open() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    run_app(
        crate::WINDOW_SIZE,
        live_app(paths, Arc::new(LockedKeychain)),
        Body::Async(Box::new(|app, cx| {
            async move {
                let editor = cx.update(|cx| app.workspace.read(cx).onboarding().unwrap().clone());
                cx.update(|cx| {
                    fill(&app, cx, &editor, FormField::Name, "prod");
                    fill(&app, cx, &editor, FormField::Url, "https://127.0.0.1:1");
                    fill(&app, cx, &editor, FormField::Username, "icygui");
                    fill(&app, cx, &editor, FormField::Password, "secret");
                });
                cx.update(|cx| editor.update(cx, EnvironmentEditor::save));
                wait_for(&app, &cx, "the refusal", CONNECT, |_, cx| {
                    editor.read(cx).error().is_some()
                })
                .await;
                cx.update(|cx| {
                    let error = editor.read(cx).error().unwrap().to_owned();
                    assert!(error.contains("couldn't be stored"), "{error}");
                    assert!(error.contains("locked"), "{error}");
                    assert!(app.workspace.read(cx).onboarding().is_some(), "still shown");
                    assert!(
                        app.state.read(cx).environments().is_empty(),
                        "nothing saved"
                    );
                });
            }
            .boxed_local()
        })),
    );
}

/// A keychain that is there but can't be used (locked, or the Secret
/// Service fails): reading and deleting fail too.
struct BrokenKeychain;

impl SecretStore for BrokenKeychain {
    fn get(&self, _account: &str) -> Result<Option<SecretString>, SecretError> {
        Err(SecretError::new("the keychain is locked"))
    }

    fn set(&self, _account: &str, _secret: &SecretString) -> Result<(), SecretError> {
        Err(SecretError::new("the keychain is locked"))
    }

    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Err(SecretError::new("the keychain is locked"))
    }
}

/// An environment that can't connect (nothing listens on port 1).
fn unreachable_environment(name: &str) -> Environment {
    let mut environment = Environment::new(
        name,
        "https://127.0.0.1:1",
        AuthConfig::Basic {
            username: "icygui".to_owned(),
        },
    );
    environment.tls.use_system_roots = false;
    environment
}

fn settings_with(environment: &Environment) -> Config {
    Config {
        active_environment: Some(environment.id.clone()),
        environments: vec![environment.clone()],
        ..Config::default()
    }
}

/// ENV-03: when an environment changes from a password to a client
/// certificate, its password leaves the keychain.
#[test]
fn switching_to_a_client_certificate_removes_the_password() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    paths.create_dirs().unwrap();
    let environment = unreachable_environment("lab");
    let id = environment.id.clone();
    paths
        .config_store()
        .save(&settings_with(&environment))
        .unwrap();
    let secrets = Arc::new(DemoSecrets::default());
    secrets.put(&id, Some(SecretString::from("old-password")));
    let keychain = secrets.clone();
    run_app(
        crate::WINDOW_SIZE,
        live_app(paths, secrets),
        Body::Async(Box::new(move |app, cx| {
            async move {
                let mut changed = environment.clone();
                changed.auth = AuthConfig::ClientCertificate {
                    cert_path: "/etc/icygui/client.crt".into(),
                    key_path: "/etc/icygui/client.key".into(),
                };
                changed.author = Some("m.keller".to_owned());
                let task = cx.update(|cx| {
                    let session = live::session(cx).unwrap();
                    session.update(cx, |session, cx| {
                        session.save_environment(changed, None, cx)
                    })
                });
                task.await.unwrap();
                assert!(keychain.get(&id).unwrap().is_none(), "the password is gone");
                cx.update(|cx| {
                    assert!(matches!(
                        app.state.read(cx).environment().unwrap().auth,
                        AuthConfig::ClientCertificate { .. }
                    ));
                    assert!(app.state.read(cx).notice().is_none(), "nothing left over");
                });
            }
            .boxed_local()
        })),
    );
}

/// ENV-03, ENV-04: a keychain that can't be read or written says so: the
/// test names the keychain (not a missing password), and a password that
/// couldn't be deleted with its environment is reported with the entry to
/// remove by hand.
#[test]
fn keychain_failures_are_named_not_swallowed() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    paths.create_dirs().unwrap();
    let environment = unreachable_environment("lab");
    let id = environment.id.clone();
    paths
        .config_store()
        .save(&settings_with(&environment))
        .unwrap();
    run_app(
        crate::WINDOW_SIZE,
        live_app(paths, Arc::new(BrokenKeychain)),
        Body::Async(Box::new(move |app, cx| {
            async move {
                let test = cx.update(|cx| {
                    let session = live::session(cx).unwrap();
                    session.update(cx, |session, cx| {
                        session.test_environment(environment.clone(), None, cx)
                    })
                });
                match test.await {
                    Err(ic_core::ConnectionFailure::Other(message)) => {
                        assert!(message.contains("keychain couldn't be read"), "{message}");
                        assert!(message.contains("locked"), "{message}");
                    }
                    other => panic!("expected the keychain's error, got {other:?}"),
                }
                cx.update(|cx| {
                    let session = live::session(cx).unwrap();
                    assert!(session.update(cx, |session, cx| session.delete_environment(&id, cx)));
                });
                wait_for(&app, &cx, "the leftover's report", CONNECT, |app, cx| {
                    app.state
                        .read(cx)
                        .notice()
                        .is_some_and(|notice| notice.problem)
                })
                .await;
                cx.update(|cx| {
                    let notice = app.state.read(cx).notice().unwrap().clone();
                    let detail = notice.detail.unwrap_or_default();
                    assert!(detail.contains("still in the keychain"), "{detail}");
                    assert!(detail.contains(&id), "names the entry: {detail}");
                    assert!(app.state.read(cx).environments().is_empty());
                });
            }
            .boxed_local()
        })),
    );
}
