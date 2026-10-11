//! The sidebar's group and dashboard menus (DASH-02, DASH-03), the
//! dashboard editor with its live preview (DASH-04), export and import
//! (DASH-06), and the command palette (UI-03), on the fixture: real
//! clicks and keystrokes in the headless window.

use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, AppContext as _, Entity, Modifiers, Pixels, Point, point, px};
use ic_config::ObjectKind;
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{DashboardRef, ScopeSetting};

use super::{
    Body, Harness, network, production, replication, run, run_app, sidebar_item, wait_for,
};
use crate::actions::ObjectAction;
use crate::app_state::AppState;
use crate::app_state::editing::NEW_DASHBOARD_NAME as NEW;
use crate::dashboard::DashboardEvent;
use crate::editor::{DashboardEditor, EditorTarget};
use crate::fixture::FixtureOptions;
use crate::operate::tracker::Toast;
use crate::palette::{CommandPalette, PaletteCommand};
use crate::sidebar::{RenameTarget, SidebarMenu};
use crate::workspace::{Confirmed, ModalKind};

/// The `overview` group's `···` (shown: it holds the selected dashboard).
const OVERVIEW_MENU: Point<Pixels> = Point {
    x: px(274.),
    y: px(59. + super::CLUSTER_SECTION),
};
/// The `overview` group's `+`.
const OVERVIEW_ADD: Point<Pixels> = Point {
    x: px(250.),
    y: px(59. + super::CLUSTER_SECTION),
};

/// An item of the group menu opened from [`OVERVIEW_MENU`], `offset`
/// pixels below the group row.
fn group_menu_item(offset: f32) -> Point<Pixels> {
    point(px(180.), px(59. + super::CLUSTER_SECTION + offset))
}

/// The middle of the `network` row (under the `platform` group, after
/// the three `overview` dashboards).
fn network_row() -> Point<Pixels> {
    point(
        px(150.),
        px(41. + super::CLUSTER_SECTION + 36. + 3. * 30. + 6. + 36. + 15.),
    )
}

/// An item of a dashboard's menu, `offset` pixels below its row's middle.
fn dashboard_menu_item(row: Point<Pixels>, offset: f32) -> Point<Pixels> {
    point(px(180.), row.y + px(offset))
}

fn right_click(app: &Harness, cx: &mut App, position: Point<Pixels>) {
    app.in_window(cx, |window, cx| {
        window.dispatch_event(
            gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                button: gpui::MouseButton::Right,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                button: gpui::MouseButton::Right,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
    });
    app.draw(cx);
}

fn palette(app: &Harness, cx: &App) -> Entity<CommandPalette> {
    app.workspace.read(cx).palette().unwrap().clone()
}

fn editor(app: &Harness, cx: &App) -> Entity<DashboardEditor> {
    app.workspace.read(cx).editor().unwrap().clone()
}

fn group_names(state: &AppState) -> Vec<String> {
    state
        .groups()
        .iter()
        .map(|group| group.name.clone())
        .collect()
}

/// Replaces the palette's query, as typing would.
fn type_query(app: &Harness, cx: &mut App, query: &str) {
    let input = palette(app, cx).read(cx).input().clone();
    app.in_window(cx, |window, cx| {
        input.update(cx, |input, cx| input.replace_all(query, window, cx));
    });
    app.draw(cx);
}

#[test]
fn the_palette_finds_and_runs_by_keyboard() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-k");
        assert_eq!(app.workspace.read(cx).modal(cx), Some(ModalKind::Palette));
        // Typing goes to its field (the keys of the list don't fire).
        app.keys(cx, "n e t w");
        let first = palette(app, cx).read(cx).items()[0].clone();
        assert_eq!(first.label, "network");
        app.keys(cx, "enter");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        assert_eq!(app.state.read(cx).selected(), Some(&network()));

        // Escape and ctrl-k close it again.
        app.keys(cx, "ctrl-k escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        app.keys(cx, "ctrl-k ctrl-k");
        assert_eq!(app.workspace.read(cx).modal(cx), None);

        // Up and down move over the results, wrapping; Tab opens an
        // object as a tab.
        app.keys(cx, "ctrl-k");
        type_query(app, cx, "postgres-replication");
        let items = palette(app, cx).read(cx).items().to_vec();
        let service = items
            .iter()
            .position(|item| item.command == PaletteCommand::OpenObject(replication()))
            .unwrap();
        assert_eq!(palette(app, cx).read(cx).selected(), 0);
        app.keys(cx, "up");
        assert_eq!(
            palette(app, cx).read(cx).selected(),
            items.len() - 1,
            "wraps"
        );
        app.keys(cx, "down");
        for _ in 0..service {
            app.keys(cx, "down");
        }
        assert_eq!(palette(app, cx).read(cx).selected(), service);
        app.keys(cx, "tab");
        assert_eq!(app.state.read(cx).active_tab(), Some(&replication()));

        // A verb acts on the objects it names; the one on screen (the
        // tab) first, as its key would.
        app.keys(cx, "ctrl-k");
        type_query(app, cx, "ack db-prod");
        let items = palette(app, cx).read(cx).items().to_vec();
        assert_eq!(items[0].label, "Acknowledge · postgres-replication");
        assert_eq!(
            items[0].command,
            PaletteCommand::Act(ObjectAction::Acknowledge, Vec::new())
        );
        app.keys(cx, "enter");
        let request = app.state.read(cx).last_request().unwrap().clone();
        assert_eq!(request.action, ObjectAction::Acknowledge);
        assert_eq!(request.targets, [replication()]);

        // ctrl/cmd-enter acts on all the objects it names, together.
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        app.keys(cx, "ctrl-k");
        type_query(app, cx, "ack db-prod");
        app.keys(cx, "ctrl-enter");
        let request = app.state.read(cx).last_request().unwrap().clone();
        assert_eq!(request.action, ObjectAction::Acknowledge);
        assert!(request.targets.len() > 1, "{:?}", request.targets);
        assert!(request.targets.contains(&replication()));
        assert!(
            request
                .targets
                .iter()
                .all(|target| target.to_string().contains("db-prod"))
        );
    });
}

#[test]
fn the_palette_acts_on_the_cursors_object_and_switches_views() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "j j");
        let cursor = app.cursor(cx).unwrap().1;
        app.keys(cx, "ctrl-k");
        let items = palette(app, cx).read(cx).items().to_vec();
        let ObjectKey::Service { key } = &cursor else {
            panic!("{cursor}");
        };
        let name = app.state.read(cx).snapshot().services[key]
            .display_name
            .clone();
        assert_eq!(items[0].label, format!("Acknowledge · {name}"));
        assert!(
            items[0].detail.starts_with(&format!("on {}", key.host)),
            "{}",
            items[0].detail
        );
        assert_eq!(items[0].key_hint, Some("a"));
        type_query(app, cx, "check now");
        app.keys(cx, "enter");
        let request = app.state.read(cx).last_request().unwrap().clone();
        assert_eq!(request.action, ObjectAction::CheckNow);
        assert_eq!(request.targets, [cursor]);

        // Settings and commands open what they say.
        app.keys(cx, "ctrl-k");
        type_query(app, cx, "add environment");
        app.keys(cx, "enter");
        assert_eq!(
            app.workspace.read(cx).modal(cx),
            Some(ModalKind::Environment)
        );
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        app.keys(cx, "ctrl-k");
        type_query(app, cx, "toggle sidebar");
        app.keys(cx, "enter");
        assert!(!app.workspace.read(cx).is_sidebar_open());
    });
}

#[test]
fn groups_are_renamed_reordered_and_deleted_from_their_menu() {
    run(FixtureOptions::default(), |app, cx| {
        let sidebar = app.workspace.read(cx).sidebar().clone();
        app.click(cx, OVERVIEW_MENU, Modifiers::default());
        assert_eq!(
            sidebar.read(cx).open_menu(),
            Some(&SidebarMenu::Group("demo-overview".to_owned()))
        );
        // Clicking the trigger again closes it.
        app.click(cx, OVERVIEW_MENU, Modifiers::default());
        assert_eq!(sidebar.read(cx).open_menu(), None);

        // rename: a field in place of the name; Enter saves.
        app.click(cx, OVERVIEW_MENU, Modifiers::default());
        app.click(cx, group_menu_item(61.), Modifiers::default());
        let (target, input) = {
            let (target, input) = sidebar.read(cx).renaming().unwrap();
            (target.clone(), input.clone())
        };
        assert_eq!(target, RenameTarget::Group("demo-overview".to_owned()));
        app.in_window(cx, |window, cx| {
            input.update(cx, |input, cx| input.replace_all("core", window, cx));
        });
        app.keys(cx, "enter");
        assert!(sidebar.read(cx).renaming().is_none());
        assert_eq!(group_names(app.state.read(cx)), ["core", "platform", "lab"]);
        // The keyboard is back in the list.
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(0));

        // move down.
        app.click(cx, OVERVIEW_MENU, Modifiers::default());
        app.click(cx, group_menu_item(154.), Modifiers::default());
        assert_eq!(group_names(app.state.read(cx)), ["platform", "core", "lab"]);

        // A rename that's escaped keeps the name.
        app.state.update(cx, |state, cx| {
            state.move_group("demo-overview", -1);
            cx.notify();
        });
        app.draw(cx);
        app.click(cx, OVERVIEW_MENU, Modifiers::default());
        app.click(cx, group_menu_item(61.), Modifiers::default());
        app.keys(cx, "x y escape");
        assert_eq!(group_names(app.state.read(cx))[0], "core");

        // delete: asks first; Escape keeps it, Enter deletes.
        app.click(cx, OVERVIEW_MENU, Modifiers::default());
        app.click(cx, group_menu_item(386.), Modifiers::default());
        let Some(ModalKind::Confirm(confirmation)) = app.workspace.read(cx).modal(cx) else {
            panic!("no confirmation");
        };
        assert_eq!(
            confirmation.action,
            Confirmed::Group("demo-overview".to_owned())
        );
        assert!(
            confirmation.detail.contains("3 dashboards"),
            "{}",
            confirmation.detail
        );
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        assert_eq!(group_names(app.state.read(cx)).len(), 3);
        app.click(cx, OVERVIEW_MENU, Modifiers::default());
        app.click(cx, group_menu_item(386.), Modifiers::default());
        app.keys(cx, "enter");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        assert_eq!(group_names(app.state.read(cx)), ["platform", "lab"]);
        assert_eq!(app.state.read(cx).selected(), Some(&network()));
    });
}

#[test]
fn dashboards_are_duplicated_moved_muted_and_deleted_from_their_menu() {
    run(FixtureOptions::default(), |app, cx| {
        let sidebar = app.workspace.read(cx).sidebar().clone();
        right_click(app, cx, network_row());
        assert_eq!(
            sidebar.read(cx).open_menu(),
            Some(&SidebarMenu::Dashboard(network()))
        );
        // duplicate: the copy follows it and is shown.
        app.click(
            cx,
            dashboard_menu_item(network_row(), 87.),
            Modifiers::default(),
        );
        let copy = app.state.read(cx).selected().unwrap().clone();
        assert_ne!(copy, network());
        let platform = app.state.read(cx).groups()[1].clone();
        assert_eq!(platform.dashboards[1].name, "network copy");

        // The copy's menu: mute it, then move it to `lab`.
        let copy_row = network_row() + point(px(0.), px(30.));
        right_click(app, cx, copy_row);
        app.click(
            cx,
            dashboard_menu_item(copy_row, 347.),
            Modifiers::default(),
        );
        let (_, dashboard) = app.state.read(cx).dashboard(&copy).unwrap();
        assert_eq!(dashboard.notifications, ScopeSetting::Off);
        right_click(app, cx, copy_row);
        app.click(
            cx,
            dashboard_menu_item(copy_row, 231.),
            Modifiers::default(),
        );
        let moved = DashboardRef {
            group_id: "demo-lab".to_owned(),
            dashboard_id: copy.dashboard_id.clone(),
        };
        assert!(app.state.read(cx).dashboard(&moved).is_some(), "now in lab");
        assert_eq!(app.state.read(cx).selected(), Some(&moved));

        // delete (asks first).
        let lab_row = point(
            px(150.),
            px(super::CLUSTER_SECTION
                + 41.
                + 36.
                + 3. * 30.
                + 6.
                + 36.
                + 4. * 30.
                + 6.
                + 36.
                + 30.
                + 15.),
        );
        right_click(app, cx, lab_row);
        assert_eq!(
            sidebar.read(cx).open_menu(),
            Some(&SidebarMenu::Dashboard(moved.clone()))
        );
        // Low in the sidebar the menu opens upward; the keyboard reaches its
        // last item (End) and chooses it (Enter).
        app.keys(cx, "end enter");
        assert!(matches!(
            app.workspace.read(cx).modal(cx),
            Some(ModalKind::Confirm(confirmation)) if confirmation.action == Confirmed::Dashboard(moved.clone())
        ));
        app.keys(cx, "enter");
        assert!(app.state.read(cx).dashboard(&moved).is_none());
    });
}

#[test]
fn a_new_dashboard_is_made_in_the_editor_with_a_live_preview() {
    run_app(
        crate::WINDOW_SIZE,
        |cx| cx.new(|_| AppState::fixture(Timestamp::now())),
        Body::Async(Box::new(|app, cx| {
            async move {
                // In an async test, events reach their subscribers when an
                // update ends: each step is an update of its own.
                cx.update(|cx| app.click(cx, OVERVIEW_ADD, Modifiers::default()));
                cx.update(|cx| {
                    let editor = editor(&app, cx);
                    assert_eq!(*editor.read(cx).target(), EditorTarget::New);
                    assert_eq!(editor.read(cx).draft().group_id, "demo-overview");
                    // The name is selected for typing.
                    app.keys(cx, "w e b");
                });
                cx.update(|cx| assert_eq!(editor(&app, cx).read(cx).draft().name, "web"));
                // A filter that doesn't parse: the preview says where.
                cx.update(|cx| {
                    let filter = editor(&app, cx).read(cx).filter_input().clone();
                    app.in_window(cx, |window, cx| {
                        filter.update(cx, |input, cx| {
                            input.replace_all("host.name ==", window, cx);
                        });
                    });
                });
                wait_for(
                    &app,
                    &cx,
                    "the preview's error",
                    Duration::from_secs(5),
                    |app, cx| {
                        editor(app, cx)
                            .read(cx)
                            .preview_result()
                            .is_some_and(|result| result.is_err())
                    },
                )
                .await;
                cx.update(|cx| {
                    let editor = editor(&app, cx);
                    let error = editor
                        .read(cx)
                        .preview_result()
                        .unwrap()
                        .clone()
                        .unwrap_err();
                    assert!(error.contains("(line 1, column 13)"), "{error}");
                    // It can't be saved like that.
                    app.keys(cx, "ctrl-s");
                });
                cx.update(|cx| {
                    let editor = editor(&app, cx);
                    assert!(editor.read(cx).save_error().is_some());
                    // A working one counts its matches.
                    let filter = editor.read(cx).filter_input().clone();
                    app.in_window(cx, |window, cx| {
                        filter.update(cx, |input, cx| {
                            input.replace_all("match(\"db-*\", host.name)", window, cx);
                        });
                    });
                });
                wait_for(
                    &app,
                    &cx,
                    "the preview's rows",
                    Duration::from_secs(5),
                    |app, cx| {
                        editor(app, cx)
                            .read(cx)
                            .preview_result()
                            .is_some_and(|result| {
                                result
                                    .as_ref()
                                    .is_ok_and(|result| !result.views[0].rows().is_empty())
                            })
                    },
                )
                .await;
                cx.update(|cx| app.keys(cx, "ctrl-s"));
                cx.update(|cx| {
                    assert!(
                        app.workspace.read(cx).editor().is_none(),
                        "saved and closed"
                    );
                    let state = app.state.read(cx);
                    let (group, dashboard) = state.selected_dashboard().unwrap();
                    assert_eq!(group.name, "overview");
                    assert_eq!(dashboard.name, "web");
                    assert_eq!(dashboard.views[0].filter, "match(\"db-*\", host.name)");
                    assert_eq!(dashboard.views[0].object_kind, ObjectKind::Services);
                    let result = state.result(state.selected().unwrap()).unwrap();
                    assert!(result.views[0].error.is_none());
                    let kept = |toast: &Toast| toast.title.contains("are kept");
                    assert!(!state.toasts().any(kept), "nothing left to keep");
                });
                // The next new dashboard starts afresh.
                cx.update(|cx| app.click(cx, OVERVIEW_ADD, Modifiers::default()));
                cx.update(|cx| assert_eq!(editor(&app, cx).read(cx).draft().name, NEW));
            }
            .boxed_local()
        })),
    );
}

/// DASH-04: a save right after typing (inside the preview's debounce)
/// never stores a filter that doesn't parse, and waits for the check of
/// one that does.
#[test]
fn a_save_right_after_typing_checks_the_filter_first() {
    run_app(
        crate::WINDOW_SIZE,
        |cx| cx.new(|_| AppState::fixture(Timestamp::now())),
        Body::Async(Box::new(|app, cx| {
            async move {
                cx.update(|cx| {
                    let dashboard = app.dashboard(cx);
                    dashboard.update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
                });
                // A broken filter, then ctrl-s before the preview's
                // debounce is over (no await in between: no timer runs).
                cx.update(|cx| {
                    app.draw(cx);
                    let filter = editor(&app, cx).read(cx).filter_input().clone();
                    app.in_window(cx, |window, cx| {
                        filter.update(cx, |input, cx| {
                            input.replace_all("service.state != ", window, cx);
                        });
                    });
                });
                cx.update(|cx| app.keys(cx, "ctrl-s"));
                cx.update(|cx| {
                    let editor = editor(&app, cx);
                    let error = editor.read(cx).save_error().unwrap().to_owned();
                    assert!(error.contains("(line 1, column 18)"), "{error}");
                    let (_, saved) = app.state.read(cx).selected_dashboard().unwrap();
                    assert_ne!(saved.views[0].filter, "service.state != ", "not saved");
                });
                // A working one, saved at once: once checked.
                cx.update(|cx| {
                    let filter = editor(&app, cx).read(cx).filter_input().clone();
                    app.in_window(cx, |window, cx| {
                        filter.update(cx, |input, cx| {
                            input.replace_all("service.state != 0", window, cx);
                        });
                    });
                });
                cx.update(|cx| {
                    app.keys(cx, "ctrl-s");
                    assert!(
                        app.workspace.read(cx).editor().is_some(),
                        "waits for the check"
                    );
                });
                wait_for(
                    &app,
                    &cx,
                    "the checked save",
                    Duration::from_secs(5),
                    |app, cx| app.workspace.read(cx).editor().is_none(),
                )
                .await;
                cx.update(|cx| {
                    let (_, saved) = app.state.read(cx).selected_dashboard().unwrap();
                    assert_eq!(saved.views[0].filter, "service.state != 0");
                });
            }
            .boxed_local()
        })),
    );
}

#[test]
fn the_header_menu_edits_a_dashboard_and_escape_discards() {
    run(FixtureOptions::default(), |app, cx| {
        let dashboard = app.dashboard(cx);
        dashboard.update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
        app.draw(cx);
        let open = editor(app, cx);
        assert_eq!(
            *open.read(cx).target(),
            EditorTarget::Existing(production())
        );
        assert_eq!(open.read(cx).draft().name, "production");
        let name = open.read(cx).name_input().clone();
        app.in_window(cx, |window, cx| {
            name.update(cx, |input, cx| input.replace_all("prod", window, cx));
        });
        // Escape asks before dropping the changes; Escape again keeps
        // editing, Enter discards.
        app.keys(cx, "escape");
        assert!(
            matches!(
                app.workspace.read(cx).modal(cx),
                Some(ModalKind::Confirm(confirmation))
                    if confirmation.action == Confirmed::DiscardEdits
            ),
            "asks first"
        );
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        assert_eq!(
            editor(app, cx).read(cx).draft().name,
            "prod",
            "still editing"
        );
        app.keys(cx, "escape enter");
        assert!(app.workspace.read(cx).editor().is_none(), "discarded");
        let (_, saved) = app.state.read(cx).selected_dashboard().unwrap();
        assert_eq!(saved.name, "production", "nothing saved");
        // Without changes, Escape leaves at once.
        dashboard.update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
        app.draw(cx);
        app.keys(cx, "escape");
        assert!(app.workspace.read(cx).editor().is_none());
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        // Selecting another dashboard leaves the editor too, keeping its
        // changes for the next edit of the same dashboard.
        dashboard.update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
        app.draw(cx);
        let name = editor(app, cx).read(cx).name_input().clone();
        app.in_window(cx, |window, cx| {
            name.update(cx, |input, cx| input.replace_all("kept", window, cx));
        });
        app.click(cx, sidebar_item(0), Modifiers::default());
        assert!(app.workspace.read(cx).editor().is_none());
        dashboard.update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
        app.draw(cx);
        assert_eq!(editor(app, cx).read(cx).draft().name, "kept");
        app.keys(cx, "escape enter");
        assert!(app.workspace.read(cx).editor().is_none());
        dashboard.update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
        app.draw(cx);
        assert_eq!(
            editor(app, cx).read(cx).draft().name,
            "production",
            "discarded for good"
        );
        app.keys(cx, "escape");
        // ctrl-n opens it for a new dashboard in the selected group.
        app.keys(cx, "ctrl-n");
        assert_eq!(*editor(app, cx).read(cx).target(), EditorTarget::New);
    });
}

#[test]
fn groups_are_exported_and_imported_as_files() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("platform.icygui-dashboards.toml");
    run_app(
        crate::WINDOW_SIZE,
        |cx| cx.new(|_| AppState::fixture(Timestamp::now())),
        Body::Async(Box::new(move |app, cx| {
            async move {
                // No file chooser here: the path is typed.
                cx.update(|cx| {
                    let text = app
                        .state
                        .read(cx)
                        .export_groups(&["demo-platform".to_owned()])
                        .unwrap();
                    let workspace = app.workspace.clone();
                    app.in_window(cx, |window, cx| {
                        workspace.update(cx, |workspace, cx| {
                            workspace.ask_for_path(Some(text), &file, window, cx);
                        });
                    });
                    app.draw(cx);
                    assert_eq!(app.workspace.read(cx).modal(cx), Some(ModalKind::Path));
                    app.keys(cx, "enter");
                });
                cx.update(|cx| assert_eq!(app.workspace.read(cx).modal(cx), None));
                wait_for(
                    &app,
                    &cx,
                    "the export",
                    Duration::from_secs(10),
                    |app, cx| app.state.read(cx).notice().is_some(),
                )
                .await;
                let written = std::fs::read_to_string(&file).unwrap();
                assert!(written.contains("icygui-dashboards"));
                assert!(written.contains("kubernetes"));
                cx.update(|cx| {
                    let workspace = app.workspace.clone();
                    app.in_window(cx, |window, cx| {
                        workspace.update(cx, |workspace, cx| {
                            workspace.ask_for_path(None, &file, window, cx);
                        });
                    });
                    app.keys(cx, "enter");
                });
                wait_for(
                    &app,
                    &cx,
                    "the import",
                    Duration::from_secs(10),
                    |app, cx| app.state.read(cx).groups().len() == 4,
                )
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    assert_eq!(
                        group_names(state),
                        ["overview", "platform", "lab", "platform"]
                    );
                    assert_ne!(state.groups()[3].id, "demo-platform", "fresh ids");
                    assert!(state.notice().is_some_and(|notice| !notice.problem));
                    assert_eq!(
                        state.selected().map(|reference| reference.group_id.clone()),
                        Some(state.groups()[3].id.clone()),
                        "the imported dashboards are shown"
                    );
                });
            }
            .boxed_local()
        })),
    );
}

#[test]
fn objects_open_from_the_palette() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-k");
        type_query(app, cx, "rabbitmq-queue mq-prod-01");
        let items = palette(app, cx).read(cx).items().to_vec();
        let index = items
            .iter()
            .position(|item| {
                item.command
                    == PaletteCommand::OpenObject(ObjectKey::service(
                        "mq-prod-01",
                        "rabbitmq-queue",
                    ))
            })
            .unwrap();
        for _ in 0..index {
            app.keys(cx, "down");
        }
        app.keys(cx, "enter");
        assert_eq!(
            app.pane_object(cx),
            Some(ObjectKey::service("mq-prod-01", "rabbitmq-queue")),
            "shown in a dashboard listing it, with its pane open"
        );
    });
}
