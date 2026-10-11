//! The one dropdown system (README *Dropdowns: one system*, PLAN 4.2
//! *Dropdown pass*) on the real window: a select's list opens from its
//! field, exactly as wide, joined to its edge, upward when there is no room
//! below; an action menu hangs 4px from its trigger, right-aligned for a
//! trigger on the right, 180 to 280px wide; both take the arrows, Enter,
//! typed letters and Escape, and close on a press outside. Also the kit
//! parts the editor's popovers use: the icon picker's cursor, *copy filter
//! from…*'s cursor and preview, and a new dashboard's provisional sidebar
//! row.

use gpui::{App, Entity, Modifiers, Pixels, Point, point, px};
use ic_config::SidebarMark;
use ic_ui_kit::{FloatKind, last_placed};

use super::{Harness, production, run};
use crate::dashboard::{DashboardEvent, HeaderMenu};
use crate::editor::DashboardEditor;
use crate::fixture::FixtureOptions;

/// The editor's *group* select (the dashboard fields' second row, right
/// column).
const GROUP_SELECT: Point<Pixels> = Point {
    x: px(1340.),
    y: px(159.),
};
/// The editor's *sidebar mark* select (left of the group).
const MARK_SELECT: Point<Pixels> = Point {
    x: px(1188.),
    y: px(159.),
};
/// The selected list view's *sort* select, near the window's bottom.
const SORT_SELECT: Point<Pixels> = Point {
    x: px(1166.),
    y: px(807.),
};
/// The filter field's *copy filter from…* button.
const COPY_FILTER: Point<Pixels> = Point {
    x: px(1404.),
    y: px(546.),
};
/// The dashboard header's `···`.
const HEADER_OPTIONS: Point<Pixels> = Point {
    x: px(1411.),
    y: px(20.),
};

fn editor(app: &Harness, cx: &App) -> Entity<DashboardEditor> {
    app.workspace.read(cx).editor().expect("the editor").clone()
}

/// Opens the editor for the production dashboard.
fn edit_production(app: &Harness, cx: &mut App) {
    app.dashboard(cx)
        .update(cx, |_, cx| cx.emit(DashboardEvent::Edit(production())));
    app.draw(cx);
}

fn open_menu(app: &Harness, cx: &App) -> Option<String> {
    editor(app, cx).read(cx).open_menu()
}

#[test]
fn a_select_opens_from_its_field_and_takes_the_keyboard() {
    run(FixtureOptions::default(), |app, cx| {
        edit_production(app, cx);
        let group = editor(app, cx).read(cx).draft().group_id.clone();
        app.click(cx, GROUP_SELECT, Modifiers::default());
        assert_eq!(open_menu(app, cx).as_deref(), Some("Group"));
        let placed = last_placed(FloatKind::SelectList, cx).expect("the list is drawn");
        // One shape: exactly as wide as the field, joined to its bottom
        // edge, no gap.
        assert_eq!(placed.content.left(), placed.trigger.left());
        assert_eq!(placed.content.size.width, placed.trigger.size.width);
        assert_eq!(placed.content.top(), placed.trigger.bottom());

        // The first arrow lands on the current value, the next moves on,
        // Enter chooses.
        app.keys(cx, "down down enter");
        assert_eq!(open_menu(app, cx), None, "choosing closes it");
        let chosen = editor(app, cx).read(cx).draft().group_id.clone();
        assert_ne!(chosen, group, "the next group was chosen");

        // Typing jumps to a name: the fixture's groups are overview, dba,
        // platform, lab.
        app.click(cx, GROUP_SELECT, Modifiers::default());
        app.keys(cx, "p enter");
        let typed = editor(app, cx).read(cx).draft().group_id.clone();
        let platform = app
            .state
            .read(cx)
            .groups()
            .iter()
            .find(|group| group.name == "platform")
            .map(|group| group.id.clone());
        assert_eq!(Some(typed), platform);

        // Escape closes the list, not the editor.
        app.click(cx, GROUP_SELECT, Modifiers::default());
        app.keys(cx, "escape");
        assert_eq!(open_menu(app, cx), None);
        assert!(app.workspace.read(cx).editor().is_some(), "still editing");

        // A press outside closes it too.
        app.click(cx, GROUP_SELECT, Modifiers::default());
        app.click(cx, point(px(700.), px(500.)), Modifiers::default());
        assert_eq!(open_menu(app, cx), None);
    });
}

#[test]
fn a_select_near_the_window_bottom_opens_upward() {
    run(FixtureOptions::default(), |app, cx| {
        edit_production(app, cx);
        app.click(cx, SORT_SELECT, Modifiers::default());
        assert_eq!(open_menu(app, cx).as_deref(), Some("Sort"));
        let placed = last_placed(FloatKind::SelectList, cx).expect("the list is drawn");
        // Joined to the field's top edge, as wide as it.
        assert_eq!(placed.content.bottom(), placed.trigger.top());
        assert_eq!(placed.content.size.width, placed.trigger.size.width);
        assert!(placed.content.top() >= px(8.), "inside the window");
        app.keys(cx, "escape");
        assert_eq!(open_menu(app, cx), None);
    });
}

#[test]
fn action_menus_hang_from_their_trigger() {
    run(FixtureOptions::default(), |app, cx| {
        let dashboard = app.dashboard(cx);
        app.click(cx, HEADER_OPTIONS, Modifiers::default());
        assert_eq!(dashboard.read(cx).open_menu(), Some(HeaderMenu::Options));
        let placed = last_placed(FloatKind::Popover, cx).expect("the menu is drawn");
        // 4px under the trigger's pressed background, right-aligned with
        // it (a trigger on the right of its header).
        assert_eq!(placed.content.top(), placed.trigger.bottom() + px(4.));
        assert_eq!(placed.content.right(), placed.trigger.right());
        // Sized to its longest entry, within the one width rule.
        let width = placed.content.size.width;
        assert!(width >= px(180.) && width <= px(280.), "{width:?}");

        // The arrows move, Escape closes before anything behind sees it.
        app.keys(cx, "down down");
        assert_eq!(dashboard.read(cx).open_menu(), Some(HeaderMenu::Options));
        app.keys(cx, "escape");
        assert_eq!(dashboard.read(cx).open_menu(), None);
        // Enter on the first item (edit dashboard) runs it.
        app.click(cx, HEADER_OPTIONS, Modifiers::default());
        app.keys(cx, "down enter");
        assert!(
            app.workspace.read(cx).editor().is_some(),
            "edit dashboard ran"
        );
    });
}

#[test]
fn the_icon_picker_has_a_keyboard_cursor() {
    run(FixtureOptions::default(), |app, cx| {
        edit_production(app, cx);
        // The mark select: state (current), icon; *icon* goes on to the
        // picker, its search taking the keys.
        app.click(cx, MARK_SELECT, Modifiers::default());
        assert_eq!(open_menu(app, cx).as_deref(), Some("Mark"));
        app.keys(cx, "down down enter");
        assert_eq!(open_menu(app, cx).as_deref(), Some("IconPicker"));
        app.keys(cx, "s e r");
        let found = crate::editor::model::pickable_icons("ser");
        assert!(found.len() > 2, "{found:?}");
        assert_eq!(editor(app, cx).read(cx).icon_cursor(), Some(0));
        app.keys(cx, "right right left right");
        assert_eq!(editor(app, cx).read(cx).icon_cursor(), Some(2));
        app.keys(cx, "enter");
        assert_eq!(open_menu(app, cx), None, "choosing closes the picker");
        assert_eq!(
            editor(app, cx).read(cx).draft().mark,
            SidebarMark::Icon(found[2].lucide_name().to_owned())
        );
    });
}

#[test]
fn copy_filter_from_has_a_cursor_and_a_preview() {
    run(FixtureOptions::default(), |app, cx| {
        edit_production(app, cx);
        app.click(cx, COPY_FILTER, Modifiers::default());
        assert_eq!(open_menu(app, cx).as_deref(), Some("CopyFilter"));
        let sources = editor(app, cx).read(cx).copy_sources_for_test("", cx);
        assert!(sources.len() > 1);
        app.keys(cx, "down down");
        let (cursor, preview) = {
            let editor = editor(app, cx);
            let editor = editor.read(cx);
            let (cursor, preview) = editor.copy_cursor();
            (cursor, preview.cloned())
        };
        assert_eq!(cursor, Some(1));
        // The preview counts what the filter matches here, on the
        // snapshot (no request).
        let (filter, count) = preview.expect("the preview's count");
        assert_eq!(filter, sources[1].filter);
        let snapshot = app.state.read(cx).snapshot().clone();
        assert_eq!(
            Some(count),
            crate::editor::model::count_matches(
                &snapshot,
                &filter,
                sources[1].display,
                sources[1].object_kind,
                ic_model::Timestamp::now(),
            )
        );
        app.keys(cx, "enter");
        assert_eq!(open_menu(app, cx), None);
        let copied = editor(app, cx)
            .read(cx)
            .filter_input()
            .read(cx)
            .value()
            .to_string();
        assert_eq!(copied, sources[1].filter);
    });
}

#[test]
fn a_new_dashboard_shows_a_provisional_row_while_edited() {
    run(FixtureOptions::default(), |app, cx| {
        let sidebar = app.workspace.read(cx).sidebar().clone();
        assert!(sidebar.read(cx).provisional().is_none());
        app.keys(cx, "ctrl-n");
        let draft_group = editor(app, cx).read(cx).draft().group_id.clone();
        let provisional = sidebar.read(cx).provisional().cloned().expect("the row");
        assert_eq!(provisional.group_id, draft_group);
        assert_eq!(provisional.name, "new dashboard");
        // It follows the draft's name.
        app.keys(cx, "w e b");
        app.draw(cx);
        assert_eq!(
            sidebar.read(cx).provisional().map(|row| row.name.clone()),
            Some("web".to_owned())
        );
        // Leaving the editor takes it away.
        app.keys(cx, "escape");
        if app.workspace.read(cx).modal(cx).is_some() {
            app.keys(cx, "enter");
        }
        app.draw(cx);
        assert!(app.workspace.read(cx).editor().is_none());
        assert!(sidebar.read(cx).provisional().is_none());
    });
}
