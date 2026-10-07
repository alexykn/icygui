//! The dashboard editor's views (topic 04, 4c–4e; topic 05, 5e) on the
//! real window: the views list (add a view of any kind, rename, reorder by
//! dragging or alt-↑/↓, duplicate, remove), the selected view's settings,
//! the live preview of every view with the selected one marked on its
//! header, and the checks before a save.

use std::time::Duration;

use futures::FutureExt as _;
use gpui::{
    App, AppContext as _, Entity, Modifiers, MouseButton, MouseMoveEvent, Pixels, PlatformInput,
    Point, point, px,
};
use ic_config::{GridCells, GroupSource, HandledMode, View, ViewDisplay};
use ic_model::Timestamp;
use ic_rules::ScopeSetting;

use super::{Body, Harness, production, run, run_app, wait_for};
use crate::app_state::AppState;
use crate::app_state::editing::DashboardDraft;
use crate::dashboard::DashboardEvent;
use crate::dashboard::page::ItemKind;
use crate::editor::DashboardEditor;
use crate::fixture::FixtureOptions;
use crate::workspace::{Confirmed, ModalKind};

/// The middle of row `index` of the views list (the inspector's rows are
/// 34px field boxes, 4px apart, under the name and the sidebar mark and
/// group).
fn view_row(index: u8) -> Point<Pixels> {
    point(px(1160.), px(229. + 38. * f32::from(index)))
}

/// The `···` of row `index` of the views list.
fn view_more(index: u8) -> Point<Pixels> {
    point(px(1405.), view_row(index).y)
}

/// An item of the `···` menu of row `index`: move up (0), move down,
/// duplicate, collapse by default, remove (4, past the separator).
fn view_menu_item(index: u8, item: u8) -> Point<Pixels> {
    let offset = if item == 4 {
        154.
    } else {
        34. + 28. * f32::from(item)
    };
    point(px(1280.), view_row(index).y + px(offset))
}

/// *add view* under a list of `count` views.
fn add_view(count: u8) -> Point<Pixels> {
    point(px(1137.), px(221. + 38. * f32::from(count)))
}

/// Item `index` of the *add view* menu (list, grouped list, host-group
/// grid, summary tiles, event stream, handling, downtimes: in sections,
/// each under its label) under a list of `count` views.
fn add_view_item(count: u8, index: u8) -> Point<Pixels> {
    const OFFSETS: [f32; 7] = [60., 88., 148., 176., 236., 264., 292.];
    point(
        px(1150.),
        add_view(count).y + px(OFFSETS[usize::from(index)]),
    )
}

/// The handled field's `show` (or, `hide`, its `hide`) of a list view
/// under a list of `count` views.
fn handled_choice(count: u8, hide: bool) -> Point<Pixels> {
    point(
        px(if hide { 1365. } else { 1255. }),
        add_view(count).y + px(450.),
    )
}

fn editor(app: &Harness, cx: &App) -> Entity<DashboardEditor> {
    app.workspace.read(cx).editor().expect("the editor").clone()
}

/// Opens the editor for the production dashboard.
fn edit_production(app: &Harness, cx: &mut App) {
    app.dashboard(cx)
        .update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
    app.draw(cx);
}

/// The draft's views, by name.
fn names(app: &Harness, cx: &App) -> Vec<String> {
    editor(app, cx)
        .read(cx)
        .draft()
        .views
        .iter()
        .map(crate::editor::model::view_name)
        .collect()
}

/// The selected view.
fn selected(app: &Harness, cx: &App) -> View {
    let editor = editor(app, cx);
    let editor = editor.read(cx);
    editor
        .draft()
        .views
        .iter()
        .find(|view| view.id == editor.selected())
        .expect("a selected view")
        .clone()
}

/// The view the preview marks on its header.
fn picked(app: &Harness, cx: &App) -> Option<String> {
    editor(app, cx)
        .read(cx)
        .preview_view()
        .read(cx)
        .picked_view()
        .map(ToOwned::to_owned)
}

/// Drags with the left button from `from` to `to`, as a hand would.
fn drag(app: &Harness, cx: &mut App, from: Point<Pixels>, to: Point<Pixels>) {
    app.press(cx, from, Modifiers::default());
    for step in 1..=4_u8 {
        let part = f32::from(step) / 4.;
        let position = point(
            from.x + (to.x - from.x) * part,
            from.y + (to.y - from.y) * part,
        );
        app.in_window(cx, |window, cx| {
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position,
                    pressed_button: Some(MouseButton::Left),
                    modifiers: Modifiers::default(),
                }),
                cx,
            );
        });
        app.draw(cx);
    }
    app.release(cx, to, Modifiers::default());
    app.draw(cx);
}

/// Makes production a dashboard of `views`.
fn production_with(app: &Harness, cx: &mut App, views: Vec<View>) {
    app.state.update(cx, |state, cx| {
        state
            .update_dashboard(
                &production(),
                DashboardDraft {
                    name: "production".to_owned(),
                    views,
                    notifications: ScopeSetting::Inherit,
                    group_id: "demo-overview".to_owned(),
                    mark: ic_config::SidebarMark::Auto,
                },
            )
            .unwrap();
        state.select(production());
        cx.notify();
    });
    app.draw(cx);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: the views list from adding to discarding"
)]
fn views_are_added_renamed_reordered_and_removed() {
    run(FixtureOptions::default(), |app, cx| {
        edit_production(app, cx);
        assert_eq!(names(app, cx), ["service problems"], "rc1's one list");
        let first = selected(app, cx).id;

        // *add view* asks for the display first; the view goes under the
        // selected one, starts from its filter, and is selected.
        app.click(cx, add_view(1), Modifiers::default());
        assert_eq!(
            editor(app, cx).read(cx).open_menu().as_deref(),
            Some("AddView")
        );
        app.click(cx, add_view_item(1, 2), Modifiers::default());
        assert_eq!(names(app, cx), ["service problems", "host-group grid"]);
        let grid = selected(app, cx);
        assert_eq!(grid.display, ViewDisplay::HostGroupGrid);
        assert_eq!(
            grid.filter,
            editor(app, cx).read(cx).draft().views[0].filter,
            "it starts from the selected view's filter"
        );
        assert_eq!(
            picked(app, cx),
            Some(grid.id.clone()),
            "marked in the preview"
        );
        // The preview shows both views, each under its header.
        let page = editor(app, cx)
            .read(cx)
            .preview_view()
            .read(cx)
            .page(cx)
            .expect("the preview's page");
        assert_eq!(page.views.len(), 2);
        assert!(page.items.iter().any(|item| item.kind == ItemKind::Header));

        // Renamed in its field.
        let field = editor(app, cx).read(cx).view_name_input().clone();
        app.in_window(cx, |window, cx| {
            field.update(cx, |input, cx| input.replace_all("fleet", window, cx));
        });
        app.draw(cx);
        assert_eq!(names(app, cx), ["service problems", "fleet"]);

        // An event stream under the list: select the list first.
        app.click(cx, view_row(0), Modifiers::default());
        assert_eq!(selected(app, cx).id, first);
        app.click(cx, add_view(2), Modifiers::default());
        app.click(cx, add_view_item(2, 4), Modifiers::default());
        assert_eq!(
            names(app, cx),
            ["service problems", "event stream", "fleet"]
        );

        // alt-↑ / alt-↓ move the selected view (the list has the keys
        // after a click on a row).
        app.click(cx, view_row(1), Modifiers::default());
        app.keys(cx, "alt-up");
        assert_eq!(
            names(app, cx),
            ["event stream", "service problems", "fleet"]
        );
        assert_eq!(selected(app, cx).name, "event stream", "it stays selected");
        app.keys(cx, "alt-down alt-down");
        assert_eq!(
            names(app, cx),
            ["service problems", "fleet", "event stream"]
        );
        app.keys(cx, "alt-down");
        assert_eq!(names(app, cx).len(), 3, "the last one stays last");
        // ↑ / ↓ select.
        app.keys(cx, "up");
        assert_eq!(selected(app, cx).name, "fleet");

        // Dragging a row onto another moves it there.
        drag(app, cx, view_row(0), view_row(2));
        assert_eq!(
            names(app, cx),
            ["fleet", "event stream", "service problems"]
        );
        drag(app, cx, view_row(2), view_row(0));
        assert_eq!(
            names(app, cx),
            ["service problems", "fleet", "event stream"]
        );

        // `···`: duplicate, then remove without a question.
        app.click(cx, view_more(1), Modifiers::default());
        assert_eq!(selected(app, cx).name, "fleet", "its row is selected");
        app.click(cx, view_menu_item(1, 2), Modifiers::default());
        assert_eq!(
            names(app, cx),
            ["service problems", "fleet", "fleet copy", "event stream"]
        );
        app.click(cx, view_more(2), Modifiers::default());
        app.click(cx, view_menu_item(2, 4), Modifiers::default());
        assert_eq!(
            names(app, cx),
            ["service problems", "fleet", "event stream"]
        );
        assert_eq!(app.workspace.read(cx).modal(cx), None, "no question");
        assert_eq!(selected(app, cx).name, "event stream", "the next one");

        // ctrl-backspace removes the selected view, but not while a field
        // has the keyboard.
        let filter = editor(app, cx).read(cx).filter_input().clone();
        app.in_window(cx, |window, cx| {
            filter.update(cx, |input, cx| input.focus(window, cx));
        });
        app.keys(cx, "ctrl-backspace");
        assert_eq!(names(app, cx).len(), 3, "typing never removes a view");
        app.click(cx, view_row(2), Modifiers::default());
        app.keys(cx, "ctrl-backspace ctrl-backspace");
        assert_eq!(names(app, cx), ["service problems"]);
        app.keys(cx, "ctrl-backspace");
        assert_eq!(names(app, cx).len(), 1, "the last view stays");
        assert!(
            editor(app, cx).read(cx).changes().is_none(),
            "back to the dashboard as saved"
        );

        // Discard brings everything back, after asking.
        app.click(cx, add_view(1), Modifiers::default());
        app.click(cx, add_view_item(1, 3), Modifiers::default());
        assert_eq!(names(app, cx), ["service problems", "summary tiles"]);
        app.keys(cx, "escape");
        assert!(matches!(
            app.workspace.read(cx).modal(cx),
            Some(ModalKind::Confirm(confirmation)) if confirmation.action == Confirmed::DiscardEdits
        ));
        app.keys(cx, "enter");
        assert!(app.workspace.read(cx).editor().is_none());
        let (_, saved) = app.state.read(cx).selected_dashboard().unwrap();
        assert_eq!(saved.views.len(), 1);
    });
}

#[test]
fn the_selected_view_is_marked_on_its_header_and_a_click_picks_a_view() {
    run(FixtureOptions::default(), |app, cx| {
        let views = vec![
            View {
                id: "problems".to_owned(),
                name: "problems".to_owned(),
                ..View::default()
            },
            View {
                id: "grid".to_owned(),
                name: "fleet".to_owned(),
                display: ViewDisplay::HostGroupGrid,
                ..View::default()
            },
            View {
                id: "hosts".to_owned(),
                name: "hosts".to_owned(),
                object_kind: ic_config::ObjectKind::Hosts,
                problems_only: false,
                ..View::default()
            },
        ];
        production_with(app, cx, views);
        edit_production(app, cx);
        assert_eq!(picked(app, cx).as_deref(), Some("problems"), "the first");
        assert_eq!(
            editor(app, cx).read(cx).draft().views.len(),
            3,
            "the editor has every view"
        );

        // A click anywhere in a view of the preview selects it there.
        let preview = editor(app, cx).read(cx).preview_view().clone();
        let page = preview.read(cx).page(cx).expect("a page");
        let grid_line = (0..page.items.len())
            .find(|&index| matches!(page.items[index].kind, ItemKind::Grid { .. }))
            .expect("the grid's squares");
        let bounds = preview.read(cx).item_bounds(grid_line, cx).unwrap();
        assert!(
            bounds.top() > px(41.) && bounds.bottom() < px(900.),
            "{bounds:?}"
        );
        app.click(
            cx,
            point(bounds.right() - px(20.), bounds.center().y),
            Modifiers::default(),
        );
        assert_eq!(selected(app, cx).id, "grid");
        assert_eq!(picked(app, cx).as_deref(), Some("grid"));
        assert_eq!(app.pane_object(cx), None, "the preview opens no pane");
        // A view's header too.
        let header = (0..page.items.len())
            .find(|&index| {
                page.items[index].kind == ItemKind::Header && page.items[index].view == 0
            })
            .unwrap();
        let bounds = preview.read(cx).item_bounds(header, cx).unwrap();
        app.click(
            cx,
            point(bounds.left() + px(200.), bounds.center().y),
            Modifiers::default(),
        );
        assert_eq!(selected(app, cx).id, "problems");

        // A row of the inspector selects its view; the preview marks it
        // and scrolls its header into view.
        app.click(cx, view_row(2), Modifiers::default());
        assert_eq!(selected(app, cx).id, "hosts");
        assert_eq!(picked(app, cx).as_deref(), Some("hosts"));
        let page = preview.read(cx).page(cx).unwrap();
        let header = (0..page.items.len())
            .find(|&index| {
                page.items[index].kind == ItemKind::Header && page.items[index].view == 2
            })
            .unwrap();
        let bounds = preview.read(cx).item_bounds(header, cx).unwrap();
        assert!(
            bounds.top() >= px(41.) && bounds.bottom() <= px(900.),
            "{bounds:?}"
        );
        // The selection is no change to save.
        assert!(editor(app, cx).read(cx).changes().is_none());
    });
}

#[test]
fn a_views_settings_follow_its_display() {
    run(FixtureOptions::default(), |app, cx| {
        let views = vec![
            View {
                id: "list".to_owned(),
                name: "problems".to_owned(),
                ..View::default()
            },
            View {
                id: "grid".to_owned(),
                name: "fleet".to_owned(),
                display: ViewDisplay::HostGroupGrid,
                ..View::default()
            },
            View {
                id: "stream".to_owned(),
                name: "events".to_owned(),
                display: ViewDisplay::EventStream,
                ..View::default()
            },
        ];
        production_with(app, cx, views);
        edit_production(app, cx);
        let editor = editor(app, cx);

        // The list's handled field: as in settings, show, or hide kinds
        // (its `hide`, under three views).
        app.click(cx, view_row(0), Modifiers::default());
        app.click(cx, handled_choice(3, true), Modifiers::default());
        let list = selected(app, cx);
        assert_eq!(list.handled.mode, HandledMode::Hide);
        assert_eq!(
            list.handled.hide,
            app.state.read(cx).handled_defaults(),
            "hide starts from what the settings hide"
        );
        // `show`.
        app.click(cx, handled_choice(3, false), Modifiers::default());
        assert_eq!(selected(app, cx).handled.mode, HandledMode::Show);

        // The grid's: squares or labelled cells, its groups.
        app.click(cx, view_row(1), Modifiers::default());
        editor.update(cx, |editor, cx| {
            editor.change_selected_for_test(cx, |view| view.grid.cells = GridCells::LabelledCells);
        });
        app.draw(cx);
        assert_eq!(selected(app, cx).grid.cells, GridCells::LabelledCells);
        let preview = editor.read(cx).preview_view().clone();
        let page = preview.read(cx).page(cx).unwrap();
        assert!(
            page.views[1].grid.as_ref().is_some_and(|grid| grid.cells),
            "the preview shows labelled cells at once"
        );
    });
}

/// DASH-04 with several views: a save checks every view's settings and
/// filter, and selects the view in question.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: each check that stops a save, then the save"
)]
fn a_save_checks_every_view() {
    run_app(
        crate::WINDOW_SIZE,
        |cx| cx.new(|_| AppState::fixture(Timestamp::now())),
        Body::Async(Box::new(|app, cx| {
            async move {
                cx.update(|cx| {
                    let views = vec![
                        View {
                            id: "list".to_owned(),
                            name: "problems".to_owned(),
                            ..View::default()
                        },
                        View {
                            id: "tiles".to_owned(),
                            name: "sites".to_owned(),
                            display: ViewDisplay::SummaryTiles,
                            ..View::default()
                        },
                    ];
                    production_with(&app, cx, views);
                });
                cx.update(|cx| edit_production(&app, cx));
                cx.update(|cx| {
                    app.draw(cx);
                    // The tiles group by a custom variable without a name.
                    let editor = editor(&app, cx);
                    app.click(cx, view_row(1), Modifiers::default());
                    editor.update(cx, |editor, cx| {
                        editor.change_selected_for_test(cx, |view| {
                            view.groups.by = GroupSource::CustomVar;
                        });
                    });
                    app.click(cx, view_row(0), Modifiers::default());
                    app.keys(cx, "ctrl-s");
                });
                cx.update(|cx| {
                    let editor = editor(&app, cx);
                    let error = editor.read(cx).save_error().unwrap().to_owned();
                    assert_eq!(error, "Name the custom variable sites groups by.");
                    assert_eq!(editor.read(cx).selected(), "tiles", "it shows the view");
                    // Named, it goes on to the filters.
                    let field = editor.read(cx).custom_var_input().clone();
                    app.in_window(cx, |window, cx| {
                        field.update(cx, |input, cx| input.replace_all("site", window, cx));
                    });
                });
                // (In an async test, a field's change reaches the editor when
                // the update ends: each step is an update of its own.)
                cx.update(|cx| {
                    // A filter that doesn't parse, in the list.
                    app.click(cx, view_row(0), Modifiers::default());
                });
                cx.update(|cx| {
                    let filter = editor(&app, cx).read(cx).filter_input().clone();
                    app.in_window(cx, |window, cx| {
                        filter.update(cx, |input, cx| {
                            input.replace_all("service.state != ", window, cx);
                        });
                    });
                });
                cx.update(|cx| {
                    app.click(cx, view_row(1), Modifiers::default());
                    app.keys(cx, "ctrl-s");
                });
                cx.update(|cx| {
                    let editor = editor(&app, cx);
                    let error = editor.read(cx).save_error().unwrap().to_owned();
                    assert!(
                        error.starts_with("Fix the filter of problems first:"),
                        "{error}"
                    );
                    assert!(error.contains("(line 1, column 18)"), "{error}");
                    assert_eq!(editor.read(cx).selected(), "list");
                    let (_, saved) = app.state.read(cx).selected_dashboard().unwrap();
                    assert_eq!(saved.views.len(), 2);
                    assert_eq!(
                        saved.views[1].groups.by,
                        GroupSource::HostGroup,
                        "not saved"
                    );
                    // Fixed, the save waits for the check and goes through.
                    let filter = editor.read(cx).filter_input().clone();
                    app.in_window(cx, |window, cx| {
                        filter.update(cx, |input, cx| {
                            input.replace_all("service.state != 0", window, cx);
                        });
                    });
                });
                cx.update(|cx| app.keys(cx, "ctrl-s"));
                cx.update(|cx| {
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
                    assert_eq!(saved.views[1].groups.by, GroupSource::CustomVar);
                    assert_eq!(saved.views[1].groups.custom_var, "site");
                });
            }
            .boxed_local()
        })),
    );
}

#[test]
fn a_new_dashboard_starts_with_one_list_view() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-n");
        let editor = editor(app, cx);
        let draft = editor.read(cx).draft();
        assert_eq!(draft.views.len(), 1);
        assert_eq!(draft.views[0].display, ViewDisplay::List);
        assert_eq!(editor.read(cx).selected(), draft.views[0].id);
        // The preview shows it as the dashboard will: a single list,
        // without a view header.
        let page = editor
            .read(cx)
            .preview_view()
            .read(cx)
            .page(cx)
            .expect("the preview's page");
        assert!(page.items.iter().all(|item| item.kind != ItemKind::Header));
    });
}

/// The preview's own controls (a list's handled button, a view header's
/// menus) change the draft, not the saved dashboard.
#[test]
fn the_previews_handled_button_changes_the_draft() {
    run(FixtureOptions::default(), |app, cx| {
        let overview = ic_rules::DashboardRef {
            group_id: "demo-overview".to_owned(),
            dashboard_id: "demo-overview-overview".to_owned(),
        };
        app.state.update(cx, |state, cx| {
            state.select(overview.clone());
            cx.notify();
        });
        app.draw(cx);
        app.dashboard(cx)
            .update(cx, |_, cx| cx.emit(DashboardEvent::Edit(overview.clone())));
        app.draw(cx);
        let editor = editor(app, cx);
        let hidden = editor
            .read(cx)
            .preview_result()
            .and_then(Result::ok)
            .and_then(|result| result.views.first().map(|view| view.hidden))
            .unwrap();
        assert!(hidden > 0, "the overview hides handled problems");
        // A single list: the summary bar's slot, at the preview's right end.
        app.click(
            cx,
            point(px(1068. - 18. - 10.), px(41. + 18.)),
            Modifiers::default(),
        );
        assert_eq!(selected(app, cx).handled.mode, HandledMode::Show);
        let result = editor
            .read(cx)
            .preview_result()
            .and_then(Result::ok)
            .unwrap();
        assert_eq!(result.views[0].hidden, 0, "the preview shows them, hollow");
        let (_, saved) = app.state.read(cx).selected_dashboard().unwrap();
        assert_ne!(
            saved.views[0].handled.mode,
            HandledMode::Show,
            "only the draft"
        );
        assert!(editor.read(cx).changes().is_some());
    });
}
