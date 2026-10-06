//! Several API URLs per environment (ENV-12) through the real core: the
//! demo's `prod-cluster` lists its master and its satellite. The editor
//! lists, tests, reorders, adds and removes them; with the master down
//! (`ICYGUI_DEMO_FAULT=partial`) the satellite's partial view is labelled
//! in the connection status, its details and the snapshot.

use futures::FutureExt as _;
use ic_core::ClusterView;
use ic_model::Timestamp;

use super::environments::{CONNECT, connected_to, demo_app};
use super::{Body, run_app, wait_for};
use crate::environments::form::FormField;
use crate::live::demo::{self, DemoFault};

#[test]
fn the_editor_tests_reorders_adds_and_removes_the_clusters_urls() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "prod-cluster", CONNECT, |app, cx| {
                    connected_to(app.state.read(cx), demo::ENDPOINT)
                })
                .await;
                // The master sees the whole cluster: nothing to label.
                cx.update(|cx| {
                    let connection = app.state.read(cx).connection();
                    let node = connection.node.clone().unwrap();
                    assert_eq!(node.view, ClusterView::Full);
                    assert_eq!(node.zone.as_deref(), Some("master"));
                    assert_eq!(node.url_index, 0);
                    assert!(connection.view_marker().is_none());
                });
                let editor = cx.update(|cx| {
                    let id = app
                        .state
                        .read(cx)
                        .active_environment_id()
                        .unwrap()
                        .to_owned();
                    let workspace = app.workspace.clone();
                    app.in_window(cx, |window, cx| {
                        workspace.update(cx, |workspace, cx| {
                            workspace.open_environment_editor(Some(&id), window, cx);
                        });
                    });
                    app.draw(cx);
                    let editor = app.workspace.read(cx).environment_editor().unwrap().clone();
                    let form = editor.read(cx).form().clone();
                    assert_eq!(form.urls.len(), 2, "the master and the satellite");
                    assert!(form.urls.iter().all(|url| !url.pinned.is_empty()));
                    editor
                });
                // "test all URLs": one after the other, each names its node.
                cx.update(|cx| editor.update(cx, crate::environments::EnvironmentEditor::test));
                wait_for(&app, &cx, "both tests", CONNECT, |_, cx| {
                    let editor = editor.read(cx);
                    editor.test_result_of(0).is_some() && editor.test_result_of(1).is_some()
                })
                .await;
                let satellite_url = cx.update(|cx| {
                    let editor = editor.read(cx);
                    let master = editor.test_result_of(0).unwrap().clone().unwrap();
                    assert_eq!(master.node.name, "master-01");
                    assert_eq!(master.node.view, ClusterView::Full);
                    let satellite = editor.test_result_of(1).unwrap().clone().unwrap();
                    assert_eq!(satellite.node.name, "sat-ams-01");
                    assert_eq!(
                        satellite.node.view,
                        ClusterView::Partial {
                            zone: "ams".to_owned()
                        }
                    );
                    editor.form().urls[1].url.clone()
                });
                // Prefer the satellite: its row and its answer move up.
                cx.update(|cx| {
                    editor.update(cx, |editor, cx| editor.move_url_row(1, true, cx));
                    let editor = editor.read(cx);
                    assert_eq!(editor.form().urls[0].url, satellite_url);
                    let moved = editor.test_result_of(0).unwrap().as_ref().unwrap();
                    assert_eq!(moved.node.name, "sat-ams-01");
                    assert_eq!(
                        editor.form().to_environment().urls[0].url,
                        satellite_url,
                        "the order of preference is the list's"
                    );
                });
                // Remove it; a new row needs a URL before saving.
                cx.update(|cx| {
                    editor.update(cx, |editor, cx| editor.remove_url_row(0, cx));
                    assert_eq!(editor.read(cx).form().urls.len(), 1);
                    app.in_window(cx, |window, cx| {
                        editor.update(cx, |editor, cx| editor.add_url_row(window, cx));
                    });
                    assert_eq!(editor.read(cx).form().urls.len(), 2);
                    editor.update(cx, crate::environments::EnvironmentEditor::save);
                    let issues = editor.read(cx).shown_issues();
                    assert!(issues[&FormField::Url(1)].contains("host"), "{issues:?}");
                    // The last URL can't be removed.
                    editor.update(cx, |editor, cx| {
                        editor.remove_url_row(1, cx);
                        editor.remove_url_row(0, cx);
                    });
                    assert_eq!(editor.read(cx).form().urls.len(), 1);
                    assert!(editor.read(cx).shown_issues().is_empty());
                });
                // A single URL is tested as before.
                cx.update(|cx| {
                    editor.update(cx, |editor, cx| editor.test_url(0, cx));
                });
                wait_for(&app, &cx, "the test", CONNECT, |_, cx| {
                    editor.read(cx).test_result().is_some_and(Result::is_ok)
                })
                .await;
            }
            .boxed_local()
        })),
    );
}

#[test]
fn a_satellite_is_labelled_as_a_partial_view() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(Some(DemoFault::Partial)),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "the satellite", CONNECT, |app, cx| {
                    let state = app.state.read(cx);
                    connected_to(state, "sat-ams-01")
                        && state
                            .snapshot()
                            .node
                            .as_ref()
                            .is_some_and(|node| node.name == "sat-ams-01")
                })
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    let connection = state.connection();
                    let marker = connection.view_marker().expect("labelled");
                    assert_eq!(marker.label, "partial view: zone ams");
                    assert!(marker.partial);
                    let node = connection.node.as_ref().unwrap();
                    assert_eq!(node.url_index, 1);
                    assert_eq!(node.passed_over.len(), 1, "the master");
                    assert!(
                        node.passed_over[0].1.contains("503"),
                        "{:?}",
                        node.passed_over
                    );
                    // The objects are the zone's only.
                    let snapshot = state.snapshot();
                    assert!(!snapshot.node.as_ref().unwrap().view.is_full());
                    assert!(!snapshot.hosts.is_empty());
                    assert!(
                        snapshot
                            .hosts
                            .values()
                            .all(|host| host.check.zone.as_deref() == Some("ams"))
                    );
                    // The footer's text keeps the node and the age (the
                    // node coloured; the tooltip and the summary bar name
                    // the view).
                    let (endpoint, _) = connection.label_parts(Timestamp::now());
                    assert_eq!(endpoint, "sat-ams-01");
                });
            }
            .boxed_local()
        })),
    );
}

/// ENV-12: "+ add URL" selects the new row's `https://`, so a URL typed (or
/// pasted) replaces it instead of ending up after it.
#[test]
fn a_url_typed_into_an_added_row_replaces_its_https() {
    super::run(crate::fixture::FixtureOptions::default(), |app, cx| {
        let id = app
            .state
            .read(cx)
            .active_environment_id()
            .unwrap()
            .to_owned();
        let workspace = app.workspace.clone();
        app.in_window(cx, |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_environment_editor(Some(&id), window, cx);
            });
        });
        app.draw(cx);
        let editor = app.workspace.read(cx).environment_editor().unwrap().clone();
        let before = editor.read(cx).form().urls.len();
        app.in_window(cx, |window, cx| {
            editor.update(cx, |editor, cx| editor.add_url_row(window, cx));
        });
        app.draw(cx);
        assert_eq!(editor.read(cx).form().urls[before].url, "https://");
        app.keys(cx, "h t t p s : / / m 2 : 5 6 6 5");
        assert_eq!(editor.read(cx).form().urls[before].url, "https://m2:5665");
    });
}
