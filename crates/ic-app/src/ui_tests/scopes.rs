//! The notification centre and the footer's switcher across environments
//! (A2, A3, B) on the demo's three environments, without engines: each
//! environment's notifications arrive as its engine would send them. The
//! centre opens on the environment on screen; *all* merges them with the
//! environment's name in front of each label; a label filters, again
//! clears; *mark all read* marks what the list shows; a storm collapses
//! into its summary and expands; an entry of another environment switches
//! there and opens its object; opening an object marks its notifications
//! read; the clock's badge counts the environment on screen and the
//! switcher lists every environment with its mute.

use gpui::{App, AppContext as _, Modifiers, point, px};
use ic_core::{CoreEvent, NotificationRecord};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{NotificationIntent, Silence, Tone};

use super::{Body, Harness, replication, run, run_app};
use crate::app_state::AppState;
use crate::fixture::FixtureOptions;
use crate::live::demo;
use crate::notifications::entry::{CentreItem, Place, Scope};
use crate::sidebar::switcher_rows;

/// The footer's clock (the notification centre).
const CLOCK: gpui::Point<gpui::Pixels> = gpui::Point {
    x: px(50.),
    y: px(880.),
};
/// The footer's environment switcher.
const SWITCHER: gpui::Point<gpui::Pixels> = gpui::Point {
    x: px(150.),
    y: px(880.),
};
/// The centre's `all` chip, first in its bottom bar (the list is 440 px
/// high: the card's height doesn't change).
const ALL_TAB: gpui::Point<gpui::Pixels> = gpui::Point {
    x: px(72.),
    y: px(842.),
};
/// The first entry's label, list full.
const FIRST_LABEL: gpui::Point<gpui::Pixels> = gpui::Point {
    x: px(140.),
    y: px(457.),
};
/// The first entry's title, list full.
const FIRST_ENTRY: gpui::Point<gpui::Pixels> = gpui::Point {
    x: px(300.),
    y: px(424.),
};
/// *mark all read* in the centre's heading, list full.
const MARK_ALL_READ: gpui::Point<gpui::Pixels> = gpui::Point {
    x: px(400.),
    y: px(323.),
};

fn record(
    id: &str,
    object: Option<ObjectKey>,
    subtitle: &str,
    silenced: Option<Silence>,
    ago: f64,
) -> NotificationRecord {
    NotificationRecord {
        intent: NotificationIntent {
            id: id.to_owned(),
            title: match &object {
                Some(object) => format!("CRITICAL · {object}"),
                None => "3 new problems in prod-cluster".to_owned(),
            },
            object,
            subtitle: subtitle.to_owned(),
            body: "CRITICAL - standby lag 412s (> 300s)".to_owned(),
            tone: Tone::Critical,
            sound: false,
            silent: silenced.is_some(),
            silenced,
            at: Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() - ago),
        },
        read: false,
    }
}

/// What the demo's environments' engines would have sent: nine problems
/// and a storm of three in prod-cluster (on screen), eight problems in
/// staging (the newest), none in lab. Either fills the centre's list, so
/// the centre keeps its height.
fn notify_everywhere(app: &Harness, cx: &mut App) {
    app.state.update(cx, |state, cx| {
        for index in 0..9 {
            let object = ObjectKey::service("db-prod-03", &format!("check-{index}"));
            state.apply_from(
                demo::ENVIRONMENT_ID,
                CoreEvent::Notification(record(
                    &format!("prod-{index}"),
                    Some(object),
                    "overview / overview",
                    None,
                    60. + f64::from(index),
                )),
            );
        }
        for index in 0..3 {
            let object = ObjectKey::service("db-prod-01", &format!("disk-{index}"));
            state.apply_from(
                demo::ENVIRONMENT_ID,
                CoreEvent::Notification(record(
                    &format!("held-{index}"),
                    Some(object),
                    "databases / production",
                    Some(Silence::Storm {
                        summary: "storm:1".to_owned(),
                    }),
                    40. + f64::from(index),
                )),
            );
        }
        state.apply_from(
            demo::ENVIRONMENT_ID,
            CoreEvent::Notification(record("storm:1", None, "prod-cluster", None, 30.)),
        );
        for index in 0..8 {
            state.apply_from(
                demo::STAGING_ID,
                CoreEvent::Notification(record(
                    &format!("staging-{index}"),
                    Some(ObjectKey::service("stg-db-01", &format!("disk-{index}"))),
                    "overview / overview",
                    None,
                    10. + f64::from(index),
                )),
            );
        }
        cx.notify();
    });
    app.draw(cx);
}

/// The demo's environments, no engines.
fn demo_state(cx: &mut App) -> gpui::Entity<AppState> {
    cx.new(|_| AppState::demo(demo::config(), Timestamp::now()))
}

fn sidebar(app: &Harness, cx: &App) -> gpui::Entity<crate::sidebar::Sidebar> {
    app.workspace.read(cx).sidebar().clone()
}

#[test]
fn the_centre_shows_the_environment_on_screen_then_all_and_marks_the_view_read() {
    run_app(
        crate::WINDOW_SIZE,
        demo_state,
        Body::Sync(Box::new(|app, cx| {
            notify_everywhere(app, cx);
            let state = app.state.read(cx);
            // The badge counts the environment on screen (A3).
            assert_eq!(state.unread_notifications(), 13);
            assert_eq!(state.unread_in(demo::STAGING_ID), 8);

            // It opens on the environment on screen.
            app.click(cx, CLOCK, Modifiers::default());
            let sidebar = sidebar(app, cx);
            assert!(sidebar.read(cx).notifications_open());
            assert_eq!(
                sidebar.read(cx).centre_scope(cx),
                Some(Scope::Environment(demo::ENVIRONMENT_ID.to_owned()))
            );
            let view = sidebar.read(cx).centre_view(cx);
            assert_eq!((view.unread, view.total), (13, 13), "its own");
            assert!(
                view.entries()
                    .all(|entry| entry.environment == demo::ENVIRONMENT_ID)
            );
            assert!(
                view.entries()
                    .filter_map(|entry| entry.label.as_ref())
                    .all(|label| label.text == "overview" || label.text == "databases / production"),
                "labels without repetition and without the environment"
            );

            // `all` merges every environment's, names the environment.
            app.click(cx, ALL_TAB, Modifiers::default());
            assert_eq!(sidebar.read(cx).centre_scope(cx), Some(Scope::All));
            let view = sidebar.read(cx).centre_view(cx);
            assert_eq!((view.unread, view.total), (21, 21));
            let first = view.entries().next().unwrap().clone();
            assert_eq!(first.environment, demo::STAGING_ID, "newest first");
            assert_eq!(first.label.as_ref().unwrap().text, "staging · overview");

            // Its label shows that place's only; mark all read marks those.
            app.click(cx, FIRST_LABEL, Modifiers::default());
            let view = sidebar.read(cx).centre_view(cx);
            assert_eq!(view.entries().count(), 8, "staging's overview");
            assert!(
                view.entries()
                    .all(|entry| entry.label.as_ref().unwrap().place
                        == Place {
                            environment: demo::STAGING_ID.to_owned(),
                            label: "overview".to_owned(),
                        })
            );
            assert_eq!(view.unread, 21, "the heading counts the scope");
            app.click(cx, MARK_ALL_READ, Modifiers::default());
            let state = app.state.read(cx);
            assert_eq!(state.unread_in(demo::STAGING_ID), 0, "what was shown");
            assert_eq!(state.unread_in(demo::ENVIRONMENT_ID), 13, "not the rest");

            // Again: everything again; mark all read marks every one.
            app.click(cx, FIRST_LABEL, Modifiers::default());
            assert_eq!(sidebar.read(cx).centre_view(cx).entries().count(), 21);
            app.click(cx, MARK_ALL_READ, Modifiers::default());
            let state = app.state.read(cx);
            assert_eq!(state.unread_in(demo::ENVIRONMENT_ID), 0);
            assert_eq!(state.unread_notifications(), 0);
            let view = sidebar.read(cx).centre_view(cx);
            assert_eq!(view.unread, 0);
            assert!(!view.has_unread());

            // Opening it again starts on the environment on screen.
            app.click(cx, CLOCK, Modifiers::default());
            app.click(cx, CLOCK, Modifiers::default());
            assert_eq!(
                sidebar.read(cx).centre_scope(cx),
                Some(Scope::Environment(demo::ENVIRONMENT_ID.to_owned()))
            );
        })),
    );
}

#[test]
fn a_storm_collapses_into_its_summary_and_expands() {
    run_app(
        crate::WINDOW_SIZE,
        demo_state,
        Body::Sync(Box::new(|app, cx| {
            notify_everywhere(app, cx);
            app.click(cx, CLOCK, Modifiers::default());
            let sidebar = sidebar(app, cx);
            let view = sidebar.read(cx).centre_view(cx);
            // prod-cluster's newest: the storm, with what it held back.
            let Some(CentreItem::Storm(storm)) = view.sections[0].items.first().cloned() else {
                panic!("the storm first: {:?}", view.sections[0].items.first());
            };
            assert_eq!(storm.header.id, "storm:1");
            assert_eq!(storm.members.len(), 3);
            assert!(
                storm
                    .members
                    .iter()
                    .all(|member| member.silence.as_deref() == Some("silent · storm"))
            );
            assert!(sidebar.read(cx).centre_expanded().is_empty(), "collapsed");
            app.click(cx, FIRST_ENTRY, Modifiers::default());
            assert!(sidebar.read(cx).notifications_open(), "still open");
            assert_eq!(
                sidebar.read(cx).centre_expanded(),
                std::slice::from_ref(&storm.key)
            );
            assert_eq!(
                app.state.read(cx).unread_notifications(),
                13,
                "expanding reads nothing"
            );
            app.click(cx, FIRST_ENTRY, Modifiers::default());
            assert!(
                sidebar.read(cx).centre_expanded().is_empty(),
                "collapsed again"
            );
        })),
    );
}

#[test]
fn an_entry_of_another_environment_switches_there_and_opens_it() {
    run_app(
        crate::WINDOW_SIZE,
        demo_state,
        Body::Sync(Box::new(|app, cx| {
            notify_everywhere(app, cx);
            app.click(cx, CLOCK, Modifiers::default());
            app.click(cx, ALL_TAB, Modifiers::default());
            // The newest: staging's disk-0.
            app.click(cx, FIRST_ENTRY, Modifiers::default());
            app.draw(cx);
            let sidebar = sidebar(app, cx);
            assert!(!sidebar.read(cx).notifications_open(), "closed");
            let state = app.state.read(cx);
            assert_eq!(state.active_environment_id(), Some(demo::STAGING_ID));
            let object = ObjectKey::service("stg-db-01", "disk-0");
            assert_eq!(state.active_tab(), Some(&object), "shown (as a tab here)");
            assert_eq!(state.unread_notifications(), 7, "it counts as read");
        })),
    );
}

#[test]
fn opening_an_object_marks_its_notifications_read() {
    run(FixtureOptions::default(), |app, cx| {
        app.state.update(cx, |state, cx| {
            state.apply(CoreEvent::Notification(record(
                "replication",
                Some(replication()),
                "overview / production",
                None,
                120.,
            )));
            state.apply(CoreEvent::Notification(record(
                "other",
                Some(ObjectKey::host("db-prod-03")),
                "overview / production",
                None,
                60.,
            )));
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(app.state.read(cx).unread_notifications(), 2);
        app.in_window(cx, |window, cx| {
            app.workspace.update(cx, |workspace, cx| {
                workspace.reveal(&replication(), window, cx);
            });
        });
        app.draw(cx);
        assert_eq!(app.pane_object(cx), Some(replication()));
        let state = app.state.read(cx);
        assert_eq!(state.unread_notifications(), 1, "its own only");
        assert!(
            state
                .notification_records()
                .any(|record| record.intent.id == "replication" && record.read)
        );
        // With one environment the centre has no scopes: it is that one.
        app.click(cx, CLOCK, Modifiers::default());
        let sidebar = sidebar(app, cx);
        let id = app
            .state
            .read(cx)
            .active_environment_id()
            .unwrap()
            .to_owned();
        assert_eq!(
            sidebar.read(cx).centre_scope(cx),
            Some(Scope::Environment(id))
        );
    });
}

#[test]
fn the_switcher_lists_every_environment_with_its_mute() {
    run_app(
        crate::WINDOW_SIZE,
        demo_state,
        Body::Sync(Box::new(|app, cx| {
            notify_everywhere(app, cx);
            app.state.update(cx, |state, cx| {
                let until =
                    Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() + 3600.);
                assert!(state.pause_environment(demo::STAGING_ID, Some(until)));
                cx.notify();
            });
            let rows = switcher_rows(app.state.read(cx), Timestamp::now());
            let summary: Vec<(&str, bool, bool)> = rows
                .iter()
                .map(|row| (row.name.as_str(), row.active, row.muted))
                .collect();
            assert_eq!(
                summary,
                [
                    ("prod-cluster", true, false),
                    ("staging", false, true),
                    ("lab", false, false),
                ]
            );
            // The footer opens it; a row switches (ENV-01).
            app.click(cx, SWITCHER, Modifiers::default());
            assert!(sidebar(app, cx).read(cx).details_open());
            app.click(cx, point(px(130.), px(748.)), Modifiers::default());
            let state = app.state.read(cx);
            assert_eq!(state.active_environment_id(), Some(demo::STAGING_ID));
            // The badge follows the environment on screen (A3).
            assert_eq!(state.unread_notifications(), 8);
        })),
    );
}

/// A2 and the design rule: the centre keeps its size and place while it is
/// open, whatever it shows: another scope, a label filter, a storm
/// expanded, notifications arriving. Nothing moves under the pointer.
#[test]
fn the_centre_keeps_its_place_whatever_it_shows() {
    run_app(
        crate::WINDOW_SIZE,
        demo_state,
        Body::Sync(Box::new(|app, cx| {
            // A single notification: the list is far from full.
            app.state.update(cx, |state, cx| {
                state.apply_from(
                    demo::ENVIRONMENT_ID,
                    CoreEvent::Notification(record(
                        "first",
                        Some(ObjectKey::service("db-prod-03", "first")),
                        "overview / overview",
                        None,
                        60.,
                    )),
                );
                cx.notify();
            });
            app.draw(cx);
            app.click(cx, CLOCK, Modifiers::default());
            let sidebar = sidebar(app, cx);
            let bounds = sidebar.read(cx).centre_list_bounds();
            assert!(f32::from(bounds.size.height) > 400., "{bounds:?}");
            let unchanged = |app: &Harness, cx: &mut App, what: &str| {
                app.draw(cx);
                assert_eq!(
                    sidebar.read(cx).centre_list_bounds(),
                    bounds,
                    "{what} moved the centre"
                );
            };
            // `all` (the tab is where it was), then back.
            app.click(cx, ALL_TAB, Modifiers::default());
            assert_eq!(sidebar.read(cx).centre_scope(cx), Some(Scope::All));
            unchanged(app, cx, "a scope");
            // Everything arrives at once: the list fills, the card stays.
            notify_everywhere(app, cx);
            unchanged(app, cx, "notifications arriving");
            app.click(cx, FIRST_LABEL, Modifiers::default());
            assert_eq!(sidebar.read(cx).centre_view(cx).entries().count(), 8);
            unchanged(app, cx, "a label filter");
            app.click(cx, FIRST_LABEL, Modifiers::default());
            unchanged(app, cx, "clearing the filter");
        })),
    );
}

/// A5: any environment is muted from the switcher, the one on screen
/// stays as it is: a row's bell (on hover, left of its gear) points the
/// mute row at that environment; the row's click still switches.
#[test]
fn any_environment_is_muted_from_the_switcher() {
    /// staging's row and its bell.
    const STAGING_BELL: gpui::Point<gpui::Pixels> = gpui::Point {
        x: px(384.),
        y: px(748.),
    };
    /// The mute row's `1h`.
    const ONE_HOUR: gpui::Point<gpui::Pixels> = gpui::Point {
        x: px(300.),
        y: px(804.),
    };
    run_app(
        crate::WINDOW_SIZE,
        demo_state,
        Body::Sync(Box::new(|app, cx| {
            app.click(cx, SWITCHER, Modifiers::default());
            let sidebar = sidebar(app, cx);
            assert!(sidebar.read(cx).details_open());
            app.hover(cx, STAGING_BELL);
            app.click(cx, STAGING_BELL, Modifiers::default());
            assert!(sidebar.read(cx).details_open(), "still open");
            let now = Timestamp::now();
            let state = app.state.read(cx);
            assert_eq!(
                state.active_environment_id(),
                Some(demo::ENVIRONMENT_ID),
                "the bell doesn't switch"
            );
            app.click(cx, ONE_HOUR, Modifiers::default());
            let state = app.state.read(cx);
            let until = state
                .environment_paused_until(demo::STAGING_ID, now)
                .expect("staging muted");
            assert!(until.as_unix_seconds() - now.as_unix_seconds() > 3500.);
            assert_eq!(
                state.environment_paused_until(demo::ENVIRONMENT_ID, now),
                None,
                "the one on screen isn't"
            );
            assert_eq!(state.active_environment_id(), Some(demo::ENVIRONMENT_ID));
            let rows = switcher_rows(state, now);
            assert!(rows[1].muted && !rows[0].muted);
        })),
    );
}

/// The design rule: the switcher keeps its width whatever the connection
/// says (a long error, URLs passed over): the mute row's chips stay under
/// the pointer.
#[test]
fn the_switcher_keeps_its_width_while_reconnecting() {
    /// The mute row's `1h`, as while connected.
    const ONE_HOUR: gpui::Point<gpui::Pixels> = gpui::Point {
        x: px(300.),
        y: px(804.),
    };
    run_app(
        crate::WINDOW_SIZE,
        demo_state,
        Body::Sync(Box::new(|app, cx| {
            app.state.update(cx, |state, cx| {
                state.apply(CoreEvent::Connection(
                    ic_core::ConnectionState::Reconnecting {
                        error: "master-01.example.com:5665: error trying to connect: tcp connect \
                            error: Connection refused (os error 111); sat-ams-01.example.com:5665: \
                            HTTP 503: Service Unavailable"
                            .to_owned(),
                        attempt: 17,
                        retry_at: Timestamp::from_unix_seconds(
                            Timestamp::now().as_unix_seconds() + 42.,
                        ),
                        untrusted: None,
                    },
                ));
                cx.notify();
            });
            app.draw(cx);
            app.click(cx, SWITCHER, Modifiers::default());
            assert!(sidebar(app, cx).read(cx).details_open());
            app.click(cx, ONE_HOUR, Modifiers::default());
            let state = app.state.read(cx);
            assert!(
                state
                    .environment_paused_until(demo::ENVIRONMENT_ID, Timestamp::now())
                    .is_some(),
                "the chip was where it always is"
            );
        })),
    );
}

/// The smallest window (900 × 560) with 11 environments: the switcher fits
/// above the footer (its list scrolls), so *add environment* is there to
/// click.
#[test]
fn the_switcher_fits_the_smallest_window_with_many_environments() {
    run_app(
        gpui::size(px(900.), px(560.)),
        |cx: &mut App| {
            let mut config = demo::config();
            demo::set_count(&mut config, 11);
            cx.new(|_| AppState::demo(config, Timestamp::now()))
        },
        Body::Sync(Box::new(|app, cx| {
            assert_eq!(app.state.read(cx).environments().len(), 11);
            app.click(cx, point(px(150.), px(540.)), Modifiers::default());
            assert!(sidebar(app, cx).read(cx).details_open());
            app.click(cx, point(px(150.), px(503.)), Modifiers::default());
            assert!(
                app.workspace.read(cx).environment_editor().is_some(),
                "add environment was in the window"
            );
        })),
    );
}
