//! GPUI tests: the real window on GPUI's headless platform (no display
//! server, no GPU: layout, text shaping, focus, key bindings and hit testing
//! run; painting is discarded), driven by keystrokes, mouse clicks and
//! snapshot updates. Most run on the test fixture; [`live`] runs the whole
//! app against a real core and an in-process mock Icinga.
//!
//! GPUI's `TestAppContext` needs its `test-support` feature, which would build
//! a second copy of GPUI; the headless platform needs nothing extra. It runs
//! on the test thread only on Linux (macOS needs the process's main thread),
//! so these tests are Linux-only. Each test runs one app; a lock keeps them
//! from running in parallel.

use std::any::Any;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures::FutureExt as _;
use futures::future::LocalBoxFuture;
use gpui::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Entity, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseUpEvent, Pixels, PlatformInput, Point, Size, Window, point, px, size,
};
use ic_config::{GroupBy, Sort, SortKey};
use ic_core::snapshot::{DashboardResult, DashboardRow, Snapshot};
use ic_model::{Comment, CommentKind, ObjectKey, Timestamp};
use ic_rules::DashboardRef;
use ic_ui_kit::Metrics;

use crate::actions::ObjectAction;
use crate::app_state::AppState;
use crate::chrome::ControlsPreference;
use crate::dashboard::DashboardView;
use crate::fixture::FixtureOptions;
use crate::open_main_window;
use crate::operate::dialog::DialogKind;
use crate::pane::{HostTab, ObjectPane};
use crate::window_state::InitialBounds;
use crate::workspace::{self, ToggleSidebar, Workspace};

mod actions;
mod background;
mod editing;
mod environments;
mod every_environment;
mod live;
mod live_actions;
mod notifications;
mod states;
mod topology;

/// One headless app at a time.
static HEADLESS: Mutex<()> = Mutex::new(());

/// A running app with the main window open.
struct Harness {
    state: Entity<AppState>,
    workspace: Entity<Workspace>,
    window: AnyWindowHandle,
}

impl Harness {
    /// Draws the window once. `update_window` passes the root as a view
    /// handle without leasing it, so the root can render inside.
    fn draw(&self, cx: &mut App) {
        cx.update_window(self.window, |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
    }

    fn in_window<R>(&self, cx: &mut App, f: impl FnOnce(&mut Window, &mut App) -> R) -> R {
        cx.update_window(self.window, |_, window, cx| f(window, cx))
            .unwrap()
    }

    /// Types `keys` (space separated, GPUI syntax: `j`, `shift-down`,
    /// `ctrl-enter`), drawing after each, as the user would see it.
    fn keys(&self, cx: &mut App, keys: &str) {
        for key in keys.split_whitespace() {
            let keystroke = Keystroke::parse(key).unwrap();
            self.in_window(cx, |window, cx| {
                window.dispatch_keystroke(keystroke, cx);
            });
            self.draw(cx);
        }
    }

    /// Clicks at `position` with `modifiers`, then draws.
    fn click(&self, cx: &mut App, position: Point<Pixels>, modifiers: Modifiers) {
        self.press(cx, position, modifiers);
        self.release(cx, position, modifiers);
        self.draw(cx);
    }

    /// Presses the left button at `position`.
    fn press(&self, cx: &mut App, position: Point<Pixels>, modifiers: Modifiers) {
        self.in_window(cx, |window, cx| {
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
        });
    }

    /// Releases the left button at `position`.
    fn release(&self, cx: &mut App, position: Point<Pixels>, modifiers: Modifiers) {
        self.in_window(cx, |window, cx| {
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers,
                    click_count: 1,
                }),
                cx,
            );
        });
    }

    /// Publishes a snapshot whose production dashboard has `rows`, as the
    /// core does after re-evaluating it, then draws.
    fn publish_rows(&self, cx: &mut App, rows: Vec<DashboardRow>) {
        self.state.update(cx, |state, cx| {
            let old = state.snapshot().clone();
            let mut dashboards = (*old.dashboards).clone();
            let result = dashboards.get_mut(&production()).unwrap();
            *result = DashboardResult {
                rows: Arc::new(rows),
                ..result.clone()
            };
            state.set_snapshot(Arc::new(Snapshot {
                revision: old.revision + 1,
                dashboards: Arc::new(dashboards),
                ..(*old).clone()
            }));
            cx.notify();
        });
        self.draw(cx);
    }

    fn dashboard(&self, cx: &App) -> Entity<DashboardView> {
        self.workspace.read(cx).dashboard().clone()
    }

    fn cursor(&self, cx: &App) -> Option<(usize, ObjectKey)> {
        self.dashboard(cx).read(cx).cursor_in(cx)
    }

    fn pane_object(&self, cx: &App) -> Option<ObjectKey> {
        self.dashboard(cx).read(cx).pane_object(cx)
    }

    fn marked(&self, cx: &App) -> Vec<ObjectKey> {
        self.dashboard(cx).read(cx).marked(cx)
    }

    /// The selected dashboard's rows.
    fn rows(&self, cx: &App) -> Arc<Vec<DashboardRow>> {
        let state = self.state.read(cx);
        state
            .result(state.selected().unwrap())
            .unwrap()
            .rows
            .clone()
    }

    /// The object in row `index` of the selected dashboard.
    fn row_key(&self, cx: &App, index: usize) -> ObjectKey {
        match &self.rows(cx)[index] {
            DashboardRow::Object(key) => key.clone(),
            DashboardRow::Group { label, .. } => panic!("row {index} is the group {label}"),
        }
    }
}

/// The middle of list row `index` (with the list scrolled to the top): the
/// sidebar is 300px wide, the header and summary bar are 41 and 37px tall
/// with their rules, rows 61px.
fn row_position(index: usize) -> Point<Pixels> {
    let metrics = ic_ui_kit::Theme::dark().metrics;
    let top =
        Metrics::with_rule(metrics.header_height) + Metrics::with_rule(metrics.summary_bar_height);
    #[expect(clippy::cast_precision_loss, reason = "small row indices")]
    let row = Metrics::with_rule(metrics.row_height) * index as f32;
    point(px(700.), top + row + px(30.))
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::default()
    }
}

fn secondary() -> Modifiers {
    Modifiers::secondary_key()
}

/// Runs `test` in a headless app with the fixture (and `options`) in the
/// main window at the design's size. Panics inside are caught, the app
/// quits, and the panic is re-raised on the test thread.
fn run(options: FixtureOptions, test: impl FnOnce(&Harness, &mut App) + 'static) {
    run_sized(options, crate::WINDOW_SIZE, test);
}

/// [`run`] with the main window `window_size` large (headless windows can't
/// be resized once open).
fn run_sized(
    options: FixtureOptions,
    window_size: Size<Pixels>,
    test: impl FnOnce(&Harness, &mut App) + 'static,
) {
    run_app(
        window_size,
        move |cx| cx.new(|_| AppState::fixture_with(Timestamp::now(), options)),
        Body::Sync(Box::new(test)),
    );
}

/// A test that waits for the app (a live core, timers): it gets the
/// harness and the app's async context and runs on the app's event loop.
type AsyncTest = Box<dyn FnOnce(Rc<Harness>, AsyncApp) -> LocalBoxFuture<'static, ()>>;

/// A test that runs to completion inside the app's start-up callback.
type SyncTest = Box<dyn FnOnce(&Harness, &mut App)>;

/// How a test drives the app.
enum Body {
    /// Runs to completion before the event loop does anything else.
    Sync(SyncTest),
    /// Runs on the event loop, so tasks, timers and the core's events
    /// make progress while it waits.
    Async(AsyncTest),
}

/// Runs `body` in a headless app whose main window (`window_size`) shows
/// the state `setup` creates. Panics inside are caught, the app quits, and
/// the panic is re-raised on the test thread.
fn run_app(
    window_size: Size<Pixels>,
    setup: impl FnOnce(&mut App) -> Entity<AppState> + 'static,
    body: Body,
) {
    let _guard = HEADLESS.lock().unwrap_or_else(PoisonError::into_inner);
    // A hung event loop must fail the run instead of blocking it.
    let finished = Arc::new(AtomicBool::new(false));
    let watched = finished.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_mins(3);
        while Instant::now() < deadline {
            if watched.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("a headless app didn't quit within three minutes");
        std::process::abort();
    });
    let failure: Rc<RefCell<Option<Box<dyn Any + Send>>>> = Rc::default();
    let record = failure.clone();
    gpui_platform::headless()
        .with_assets(ic_ui_kit::Assets)
        // As the app runs: closing the window doesn't quit by itself.
        .with_quit_mode(gpui::QuitMode::Explicit)
        .run(move |cx: &mut App| {
            ic_ui_kit::init(cx).unwrap();
            // Draw our own window controls even though headless windows have
            // server-side decorations.
            cx.set_global(ControlsPreference::Always);
            workspace::bind_keys(cx);
            let state = setup(cx);
            let handle =
                open_main_window(state.clone(), InitialBounds::Centered(window_size), cx).unwrap();
            let workspace = handle
                .read(cx)
                .unwrap()
                .view()
                .clone()
                .downcast::<Workspace>()
                .unwrap();
            let harness = Harness {
                state,
                workspace,
                window: handle.into(),
            };
            harness.draw(cx);
            match body {
                Body::Sync(test) => {
                    let outcome = catch_unwind(AssertUnwindSafe(|| test(&harness, cx)));
                    if let Err(panic) = outcome {
                        *record.borrow_mut() = Some(panic);
                    }
                    // Quitting takes effect once the event loop runs.
                    cx.spawn(async |cx| cx.update(|cx| cx.quit())).detach();
                }
                Body::Async(test) => {
                    let harness = Rc::new(harness);
                    cx.spawn(async move |cx| {
                        let outcome = AssertUnwindSafe(test(harness, cx.clone()))
                            .catch_unwind()
                            .await;
                        if let Err(panic) = outcome {
                            *record.borrow_mut() = Some(panic);
                        }
                        cx.update(|cx| cx.quit());
                    })
                    .detach();
                }
            }
        });
    finished.store(true, Ordering::SeqCst);
    if let Some(panic) = failure.borrow_mut().take() {
        resume_unwind(panic);
    }
}

/// Waits (drawing the window every 50 ms, so views render and ask for what
/// they need) until `condition` holds, at most `timeout`.
async fn wait_for(
    app: &Harness,
    cx: &AsyncApp,
    what: &str,
    timeout: Duration,
    mut condition: impl FnMut(&Harness, &mut App) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        if cx.update(|cx| {
            app.draw(cx);
            condition(app, cx)
        }) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
}

fn production() -> DashboardRef {
    DashboardRef {
        group_id: "demo-overview".to_owned(),
        dashboard_id: "demo-overview-production".to_owned(),
    }
}

fn replication() -> ObjectKey {
    ObjectKey::service("db-prod-03", "postgres-replication")
}

#[test]
fn the_main_window_renders_and_reacts() {
    run(FixtureOptions::default(), |app, cx| {
        assert!(app.workspace.read(cx).is_sidebar_open());

        // ctrl-b and the footer button dispatch ToggleSidebar.
        let toggle = |cx: &mut App| {
            app.in_window(cx, |window, cx| {
                window.dispatch_action(Box::new(ToggleSidebar), cx);
            });
            app.draw(cx);
        };
        toggle(cx);
        assert!(!app.workspace.read(cx).is_sidebar_open(), "hidden");
        toggle(cx);
        assert!(app.workspace.read(cx).is_sidebar_open(), "shown again");

        // Selecting a dashboard re-renders the header, summary and list.
        let network = DashboardRef {
            group_id: "demo-platform".to_owned(),
            dashboard_id: "demo-platform-network".to_owned(),
        };
        let selected = app.state.update(cx, |state, cx| {
            cx.notify();
            state.select(network)
        });
        app.draw(cx);
        assert!(selected);

        // Typing in the search field filters the sidebar.
        let sidebar = app.workspace.read(cx).sidebar().clone();
        let search = sidebar.read(cx).search_input().clone();
        app.in_window(cx, |window, cx| {
            search.update(cx, |input, cx| input.replace_all("netw", window, cx));
        });
        app.draw(cx);
        assert_eq!(sidebar.read(cx).query(), "netw");

        // Collapsing a group, then clearing the search.
        app.state.update(cx, |state, cx| {
            state.toggle_group("demo-lab");
            cx.notify();
        });
        app.in_window(cx, |window, cx| {
            search.update(cx, |input, cx| input.replace_all("", window, cx));
        });
        app.draw(cx);
        assert!(sidebar.read(cx).query().is_empty());
    });
}

#[test]
fn the_keyboard_moves_the_cursor_and_opens_the_pane() {
    run(FixtureOptions::default(), |app, cx| {
        assert_eq!(app.cursor(cx), None, "nothing selected at start");
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx), Some((0, app.row_key(cx, 0))));
        app.keys(cx, "j down");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(2));
        app.keys(cx, "k");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(1));
        assert_eq!(app.pane_object(cx), None, "moving doesn't open the pane");

        app.keys(cx, "enter");
        assert_eq!(app.pane_object(cx), Some(app.row_key(cx, 1)));
        assert_eq!(app.pane_object(cx), Some(replication()));

        // With the pane open, it follows the cursor.
        app.keys(cx, "up");
        assert_eq!(app.pane_object(cx), Some(app.row_key(cx, 0)));

        let last = app.rows(cx).len() - 1;
        app.keys(cx, "end");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(last));
        app.keys(cx, "j");
        assert_eq!(
            app.cursor(cx).map(|(index, _)| index),
            Some(last),
            "stops at the end"
        );
        app.keys(cx, "home");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(0));

        app.keys(cx, "escape");
        assert_eq!(app.pane_object(cx), None, "escape closes the pane");
        assert!(app.cursor(cx).is_some(), "the cursor stays");
    });
}

#[test]
fn clicks_select_mark_and_act() {
    run(FixtureOptions::default(), |app, cx| {
        app.click(cx, row_position(2), Modifiers::default());
        let third = app.row_key(cx, 2);
        assert_eq!(app.cursor(cx), Some((2, third.clone())));
        assert_eq!(
            app.pane_object(cx),
            Some(third.clone()),
            "a click opens the pane"
        );

        // Shift-click marks the range from the clicked row.
        app.click(cx, row_position(4), shift());
        let expected: Vec<ObjectKey> = (2..=4).map(|index| app.row_key(cx, index)).collect();
        assert_eq!(app.marked(cx), expected);
        assert_eq!(
            app.pane_object(cx),
            Some(app.row_key(cx, 4)),
            "the pane follows"
        );

        // Ctrl/cmd-click adds a row; x on the cursor removes it again.
        app.click(cx, row_position(0), secondary());
        assert_eq!(app.marked(cx).len(), 4);
        assert_eq!(app.marked(cx)[0], app.row_key(cx, 0));
        app.keys(cx, "x");
        assert_eq!(app.marked(cx), expected);

        // Actions apply to the marked rows, in list order: the dialog
        // opens for them (those Icinga would refuse are skipped).
        app.keys(cx, "a");
        let request = app.state.read(cx).last_request().cloned().unwrap();
        assert_eq!(request.action, ObjectAction::Acknowledge);
        assert_eq!(request.targets, expected);
        assert_eq!(
            app.workspace.read(cx).modal(cx),
            Some(workspace::ModalKind::Action(DialogKind::Acknowledge))
        );
        // Escape closes the dialog, then the pane, then clears the marks.
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);
        app.keys(cx, "escape");
        assert_eq!(app.pane_object(cx), None);
        assert_eq!(app.marked(cx).len(), 3);
        app.keys(cx, "escape");
        assert!(app.marked(cx).is_empty());

        // Without marks, keys act on the cursor's object.
        app.keys(cx, "r");
        let request = app.state.read(cx).last_request().cloned().unwrap();
        assert_eq!(request.action, ObjectAction::CheckNow);
        assert_eq!(request.targets, [app.row_key(cx, 0)]);

        // A plain click clears the marks again.
        app.keys(cx, "shift-j shift-j");
        assert_eq!(app.marked(cx).len(), 3);
        app.click(cx, row_position(5), Modifiers::default());
        assert!(app.marked(cx).is_empty());

        // ctrl-a marks everything.
        app.keys(cx, "ctrl-a");
        assert_eq!(app.marked(cx).len(), app.rows(cx).len());
    });
}

#[test]
fn the_selection_survives_snapshot_updates() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "j j enter");
        assert_eq!(app.cursor(cx), Some((1, replication())));
        app.keys(cx, "shift-j");
        let marked = app.marked(cx);
        assert_eq!(marked.len(), 2);

        // The core publishes a snapshot with the rows in another order.
        let publish = |cx: &mut App, rows: Vec<DashboardRow>| app.publish_rows(cx, rows);
        let mut reversed: Vec<DashboardRow> = app.rows(cx).to_vec();
        reversed.reverse();
        let count = reversed.len();
        publish(cx, reversed.clone());
        let (index, key) = app.cursor(cx).unwrap();
        assert_eq!(key, app.row_key(cx, index), "the cursor is on its object…");
        assert_eq!(index, count - 3, "…which moved");
        assert_eq!(
            app.marked(cx).len(),
            2,
            "marks follow their objects: {:?}",
            app.marked(cx)
        );
        assert_eq!(
            app.pane_object(cx),
            Some(key),
            "the pane follows the cursor's object"
        );

        // The marked replication check recovers and leaves the list.
        let without: Vec<DashboardRow> = reversed
            .into_iter()
            .filter(|row| *row != DashboardRow::Object(replication()))
            .collect();
        publish(cx, without);
        assert_eq!(app.marked(cx).len(), 1, "its mark is dropped");
        assert!(!app.marked(cx).contains(&replication()));
        let (index, key) = app.cursor(cx).unwrap();
        assert_eq!(key, app.row_key(cx, index));
        // The pane keeps showing what it showed.
        assert!(app.pane_object(cx).is_some());
    });
}

#[test]
fn open_as_tab_shows_the_pane_full_width() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "j j enter ctrl-enter");
        assert_eq!(app.state.read(cx).active_tab(), Some(&replication()));
        assert_eq!(app.state.read(cx).tabs(), [replication()]);
        let tab = app.workspace.read(cx).tab(&replication()).cloned().unwrap();
        assert_eq!(tab.read(cx).object(), &replication());

        // The tab has the focus: its keys act on its object.
        app.keys(cx, "d");
        let request = app.state.read(cx).last_request().cloned().unwrap();
        assert_eq!(request.action, ObjectAction::ScheduleDowntime);
        assert_eq!(request.targets, [replication()]);
        assert_eq!(
            app.workspace.read(cx).modal(cx),
            Some(workspace::ModalKind::Action(DialogKind::Downtime))
        );
        app.keys(cx, "escape");
        assert_eq!(app.workspace.read(cx).modal(cx), None);

        // Back to the dashboard: the list and its pane are as they were.
        app.state.update(cx, |state, cx| {
            state.select(production());
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(app.state.read(cx).active_tab(), None);
        assert_eq!(app.pane_object(cx), Some(replication()));
        app.keys(cx, "j");
        assert_eq!(
            app.cursor(cx).map(|(index, _)| index),
            Some(2),
            "the list has the focus again"
        );

        // Reopen the tab from the sidebar, then close it.
        app.state.update(cx, |state, cx| {
            state.activate_tab(&replication());
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(app.state.read(cx).active_tab(), Some(&replication()));

        // A tab that followed its host link still closes as itself.
        tab.update(cx, |pane, cx| {
            pane.navigate(ObjectKey::host("db-prod-03"), cx);
        });
        app.draw(cx);
        assert_eq!(tab.read(cx).object(), &ObjectKey::host("db-prod-03"));
        tab.update(cx, ObjectPane::close);
        app.draw(cx);
        assert!(app.state.read(cx).tabs().is_empty());
        assert_eq!(app.state.read(cx).active_tab(), None);
        assert!(app.workspace.read(cx).tab(&replication()).is_none());
        assert_eq!(
            app.pane_object(cx),
            Some(app.row_key(cx, 2)),
            "the dashboard is shown again as it was"
        );
    });
}

#[test]
fn the_pane_follows_links_and_goes_back() {
    run(FixtureOptions::default(), |app, cx| {
        let dashboard = app.dashboard(cx);
        dashboard.update(cx, |view, cx| view.open_object(&replication(), cx));
        app.draw(cx);
        let pane = dashboard.read(cx).pane(cx).unwrap();
        assert_eq!(pane.read(cx).object(), &replication());

        pane.update(cx, |pane, cx| {
            pane.navigate(ObjectKey::host("db-prod-03"), cx);
        });
        app.draw(cx);
        assert_eq!(pane.read(cx).object(), &ObjectKey::host("db-prod-03"));
        assert!(pane.read(cx).can_go_back());
        assert_eq!(pane.read(cx).host_tab(), HostTab::Services);
        for tab in [
            HostTab::History,
            HostTab::Vars,
            HostTab::Config,
            HostTab::Services,
        ] {
            pane.update(cx, |pane, cx| pane.select_host_tab(tab, cx));
            app.draw(cx);
            assert_eq!(pane.read(cx).host_tab(), tab);
        }

        pane.update(cx, ObjectPane::back);
        app.draw(cx);
        assert_eq!(pane.read(cx).object(), &replication());
        assert!(!pane.read(cx).can_go_back());

        // Moving the cursor replaces the pane's object and its history.
        pane.update(cx, |pane, cx| {
            pane.navigate(ObjectKey::host("db-prod-03"), cx);
        });
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(2));
        assert_eq!(pane.read(cx).object(), &app.row_key(cx, 2));
        assert!(!pane.read(cx).can_go_back());
    });
}

#[test]
fn sorting_and_grouping_keep_the_selection() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "j j");
        assert_eq!(app.cursor(cx).map(|(_, key)| key), Some(replication()));
        let changed = app.state.update(cx, |state, cx| {
            cx.notify();
            state.update_view(&production(), |view| {
                view.sort = Sort {
                    key: SortKey::Host,
                    descending: false,
                };
                view.group_by = GroupBy::Host;
            })
        });
        assert!(changed);
        app.draw(cx);
        assert!(
            matches!(app.rows(cx)[0], DashboardRow::Group { .. }),
            "grouped"
        );
        let (index, key) = app.cursor(cx).unwrap();
        assert_eq!(key, replication(), "still on the same object");
        assert_eq!(app.row_key(cx, index), replication());
        // j skips the group headers.
        app.keys(cx, "j");
        let (next, _) = app.cursor(cx).unwrap();
        assert!(next > index);
        assert!(matches!(app.rows(cx)[next], DashboardRow::Object(_)));
    });
}

#[test]
fn every_pane_and_dashboard_renders() {
    run(FixtureOptions::default(), |app, cx| {
        let snapshot = app.state.read(cx).snapshot().clone();
        let dashboard = app.dashboard(cx);
        let objects: Vec<ObjectKey> = snapshot
            .hosts
            .values()
            .map(|host| host.key())
            .chain(
                snapshot
                    .services
                    .values()
                    .map(|service| service.object_key()),
            )
            .collect();
        for key in &objects {
            dashboard.update(cx, |view, cx| view.open_object(key, cx));
            app.draw(cx);
            assert_eq!(app.pane_object(cx).as_ref(), Some(key));
        }
        let references: Vec<DashboardRef> = snapshot.dashboards.keys().cloned().collect();
        for reference in references {
            app.state.update(cx, |state, cx| {
                state.select(reference);
                cx.notify();
            });
            app.draw(cx);
        }
        // A tab for a host, with the sidebar hidden.
        app.state.update(cx, |state, cx| {
            state.open_tab(ObjectKey::host("sw-core-ams-02"));
            cx.notify();
        });
        app.keys(cx, "ctrl-b");
        assert!(!app.workspace.read(cx).is_sidebar_open());
    });
}

#[test]
fn twenty_thousand_rows_render_only_whats_on_screen() {
    run(
        FixtureOptions {
            generated_rows: 20_000,
        },
        |app, cx| {
            assert_eq!(app.rows(cx).len(), 20_000);
            let dashboard = app.dashboard(cx);
            let visible = dashboard.read(cx).visible_rows();
            assert!(
                (10..=20).contains(&visible.len()),
                "built {} rows",
                visible.len()
            );

            // Jump to the end: the list scrolls there and still builds only
            // a screenful.
            app.keys(cx, "end");
            app.draw(cx);
            let visible = dashboard.read(cx).visible_rows();
            assert!(visible.contains(&19_999), "{visible:?}");
            assert!(visible.len() <= 20);
            app.keys(cx, "pageup");
            let (index, _) = app.cursor(cx).unwrap();
            assert!(index < 19_999 && index > 19_970, "{index}");

            // Frames cost the same at the top and the bottom of the list;
            // nothing in them walks all rows.
            let started = Instant::now();
            for _ in 0..20 {
                app.keys(cx, "k");
            }
            let per_frame = started.elapsed() / 20;
            eprintln!("20 000 rows: {per_frame:?} per key press and frame (debug build)");
            app.keys(cx, "ctrl-a");
            assert_eq!(app.marked(cx).len(), 20_000);
        },
    );
}

#[test]
fn header_menus_open_and_close() {
    use crate::dashboard::HeaderMenu;

    run(FixtureOptions::default(), |app, cx| {
        let dashboard = app.dashboard(cx);
        // `severity ↓` sits left of the 22px `···` button, 18px from the
        // window's right edge with 12px between them.
        let sort = point(px(1352.), px(20.));
        let options = point(px(1411.), px(20.));
        app.click(cx, sort, Modifiers::default());
        assert_eq!(dashboard.read(cx).open_menu(), Some(HeaderMenu::Sort));
        // Clicking the trigger again closes it rather than reopening it.
        app.click(cx, sort, Modifiers::default());
        assert_eq!(dashboard.read(cx).open_menu(), None);

        app.click(cx, options, Modifiers::default());
        assert_eq!(dashboard.read(cx).open_menu(), Some(HeaderMenu::Options));
        app.keys(cx, "escape");
        assert_eq!(dashboard.read(cx).open_menu(), None, "escape closes menus");

        app.click(cx, sort, Modifiers::default());
        assert_eq!(dashboard.read(cx).open_menu(), Some(HeaderMenu::Sort));
        // A click elsewhere closes the menu and still does its job.
        app.click(cx, row_position(3), Modifiers::default());
        assert_eq!(dashboard.read(cx).open_menu(), None);
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(3));
    });
}

#[test]
fn escape_closes_the_open_menu_before_anything_behind_it() {
    use crate::pane::PaneMenu;
    use crate::sidebar::SidebarMenu;

    run(FixtureOptions::default(), |app, cx| {
        let sidebar = app.workspace.read(cx).sidebar().clone();
        // Two marked rows and the footer's connection details: Escape
        // closes the details, the marks stay.
        app.keys(cx, "j x j x");
        assert_eq!(app.marked(cx).len(), 2);
        app.click(cx, point(px(150.), px(880.)), Modifiers::default());
        assert!(sidebar.read(cx).details_open());
        app.keys(cx, "escape");
        assert!(!sidebar.read(cx).details_open(), "the details closed");
        assert_eq!(app.marked(cx).len(), 2, "the marks stay");
        app.keys(cx, "escape");
        assert!(app.marked(cx).is_empty(), "the next Escape is the list's");

        // A pane and a group's `···` menu: Escape closes the menu only.
        app.keys(cx, "j enter");
        assert!(app.pane_object(cx).is_some());
        app.click(cx, point(px(274.), px(59.)), Modifiers::default());
        assert_eq!(
            sidebar.read(cx).open_menu(),
            Some(&SidebarMenu::Group("demo-overview".to_owned()))
        );
        app.keys(cx, "escape");
        assert_eq!(sidebar.read(cx).open_menu(), None);
        assert!(app.pane_object(cx).is_some(), "the pane stays");

        // The pane's own `···` menu too.
        let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
        pane.update(cx, ObjectPane::open_more_menu);
        app.draw(cx);
        assert_eq!(pane.read(cx).open_menu(), Some(PaneMenu::More));
        app.keys(cx, "escape");
        assert_eq!(pane.read(cx).open_menu(), None);
        assert!(app.pane_object(cx).is_some(), "the pane stays");
        app.keys(cx, "escape");
        assert_eq!(app.pane_object(cx), None, "the next Escape closes it");
    });
}

fn network() -> DashboardRef {
    DashboardRef {
        group_id: "demo-platform".to_owned(),
        dashboard_id: "demo-platform-network".to_owned(),
    }
}

/// The middle of the sidebar's dashboard row `index` (0 = `overview` under
/// the `overview` group): the header is 41px, group rows 36px, dashboard
/// rows 30px.
fn sidebar_item(index: usize) -> Point<Pixels> {
    #[expect(clippy::cast_precision_loss, reason = "small row indices")]
    let row = 30. * index as f32;
    point(px(150.), px(41. + 36. + 15.) + px(row))
}

#[test]
fn clicks_outside_the_list_keep_the_keys_working() {
    run(FixtureOptions::default(), |app, cx| {
        // The sidebar's empty space below the groups.
        app.click(cx, point(px(150.), px(700.)), Modifiers::default());
        app.keys(cx, "j j enter");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(1));
        assert_eq!(app.pane_object(cx), Some(replication()));

        // The dashboard that's already selected.
        app.click(cx, sidebar_item(1), Modifiers::default());
        assert_eq!(app.state.read(cx).selected(), Some(&production()));
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(2));

        // The footer, and a group header (which collapses the group).
        app.click(cx, point(px(150.), px(880.)), Modifiers::default());
        app.keys(cx, "k");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(1));
        app.click(cx, point(px(150.), px(59.)), Modifiers::default());
        app.keys(cx, "k");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(0));
        app.click(cx, point(px(150.), px(59.)), Modifiers::default());

        // Enter in the search field shows the first match and hands the
        // keys back to the list.
        let sidebar = app.workspace.read(cx).sidebar().clone();
        app.click(cx, point(px(150.), px(20.)), Modifiers::default());
        app.keys(cx, "n e t");
        assert_eq!(sidebar.read(cx).query(), "net", "typing goes to the field");
        app.keys(cx, "enter");
        assert_eq!(app.state.read(cx).selected(), Some(&network()));
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(0));
        assert_eq!(sidebar.read(cx).query(), "net", "j went to the list");

        // Escape clears the search and does the same.
        app.click(cx, point(px(150.), px(20.)), Modifiers::default());
        app.keys(cx, "escape");
        assert_eq!(sidebar.read(cx).query(), "");
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(1));
    });
}

#[test]
fn the_keyboard_switches_dashboards_and_tabs() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-4");
        assert_eq!(app.state.read(cx).selected(), Some(&network()));
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(0));
        app.keys(cx, "ctrl-9");
        assert_eq!(
            app.state.read(cx).selected(),
            Some(&network()),
            "there's no ninth dashboard"
        );

        app.keys(cx, "ctrl-2 j j enter ctrl-enter");
        assert_eq!(app.state.read(cx).selected(), Some(&production()));
        assert_eq!(app.state.read(cx).active_tab(), Some(&replication()));
        // Escape in a tab goes back to the dashboard and keeps the tab.
        app.keys(cx, "escape");
        assert_eq!(app.state.read(cx).active_tab(), None);
        assert_eq!(app.state.read(cx).tabs(), [replication()]);
        app.keys(cx, "j");
        assert_eq!(
            app.cursor(cx).map(|(index, _)| index),
            Some(2),
            "the list has the keys"
        );

        // ctrl-tab cycles through the tab and the dashboard.
        app.keys(cx, "ctrl-tab");
        assert_eq!(app.state.read(cx).active_tab(), Some(&replication()));
        app.keys(cx, "ctrl-tab");
        assert_eq!(app.state.read(cx).active_tab(), None);
        app.keys(cx, "ctrl-shift-tab");
        assert_eq!(app.state.read(cx).active_tab(), Some(&replication()));
        // ctrl-1 from a tab shows that dashboard.
        app.keys(cx, "ctrl-1");
        assert_eq!(app.state.read(cx).active_tab(), None);
        assert_ne!(app.state.read(cx).selected(), Some(&production()));
        app.keys(cx, "ctrl-2 ctrl-tab");
        assert_eq!(app.state.read(cx).active_tab(), Some(&replication()));
        // ctrl-w closes it; the list gets the keys back.
        app.keys(cx, "ctrl-w");
        assert!(app.state.read(cx).tabs().is_empty());
        assert_eq!(app.state.read(cx).active_tab(), None);
        app.keys(cx, "k");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(1));

        // A collapsed group's dashboards aren't numbered.
        app.state.update(cx, |state, cx| {
            state.toggle_group("demo-overview");
            cx.notify();
        });
        app.draw(cx);
        app.keys(cx, "ctrl-1");
        assert_eq!(app.state.read(cx).selected(), Some(&network()));
    });
}

#[test]
fn a_host_with_many_notes_keeps_its_services_in_reach() {
    run(FixtureOptions::default(), |app, cx| {
        let host = ObjectKey::host("db-prod-03");
        app.state.update(cx, |state, cx| {
            let old = state.snapshot().clone();
            let mut comments = (*old.comments).clone();
            let notes = comments.entry(host.clone()).or_default();
            for index in 0..12 {
                notes.push(Comment {
                    name: format!("db-prod-03!note-{index}"),
                    object: host.clone(),
                    author: "m.keller".to_owned(),
                    text: "Maintenance window agreed with the storage team.\n".repeat(3),
                    kind: CommentKind::User,
                    entry_time: Timestamp::now(),
                    expire_time: None,
                    persistent: true,
                });
            }
            state.set_snapshot(Arc::new(Snapshot {
                revision: old.revision + 1,
                comments: Arc::new(comments),
                ..(*old).clone()
            }));
            cx.notify();
        });
        let dashboard = app.dashboard(cx);
        dashboard.update(cx, |view, cx| view.open_object(&host, cx));
        app.draw(cx);
        let pane = dashboard.read(cx).pane(cx).unwrap();
        assert_eq!(pane.read(cx).object(), &host);
        app.draw(cx);
        // The notes scroll with the services instead of squeezing them out.
        let scroll = pane.read(cx).body_scroll().clone();
        let viewport = scroll.bounds();
        assert!(viewport.size.height > px(500.), "{viewport:?}");
        assert!(
            scroll.max_offset().y > px(500.),
            "{:?}",
            scroll.max_offset()
        );
    });
}

#[test]
fn a_narrow_window_narrows_the_pane_then_covers_the_list() {
    // A 1280px laptop screen: the list keeps 440px, the pane gives way.
    run_sized(
        FixtureOptions::default(),
        size(px(1280.), px(800.)),
        |app, cx| {
            app.keys(cx, "j enter");
            app.draw(cx);
            let list = app.dashboard(cx).read(cx).list_bounds(cx).unwrap();
            assert_eq!(list.origin.x, px(300.));
            assert!((px(435.)..=px(441.)).contains(&list.size.width), "{list:?}");
            let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
            let body = pane.read(cx).body_scroll().bounds();
            assert_eq!(body.origin.x + body.size.width, px(1280.));
            assert!(body.size.width > px(500.), "{body:?}");
        },
    );
    // The 900px minimum: the pane covers the list; Escape shows it again.
    run_sized(
        FixtureOptions::default(),
        size(px(900.), px(600.)),
        |app, cx| {
            app.keys(cx, "j enter");
            app.draw(cx);
            let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
            let body = pane.read(cx).body_scroll().bounds();
            assert_eq!(body.origin.x, px(300.), "{body:?}");
            assert_eq!(body.size.width, px(600.));
            // j still moves the cursor, and the pane follows it.
            app.keys(cx, "j");
            assert_eq!(app.pane_object(cx), Some(replication()));
            app.keys(cx, "escape");
            assert_eq!(app.pane_object(cx), None);
            let list = app.dashboard(cx).read(cx).list_bounds(cx).unwrap();
            assert_eq!(list.size.width, px(600.));
        },
    );
}

#[test]
fn a_click_spanning_a_reorder_selects_nothing() {
    run(FixtureOptions::default(), |app, cx| {
        let pressed = app.row_key(cx, 2);
        app.press(cx, row_position(2), Modifiers::default());
        // A snapshot moves the rows while the button is down.
        let mut reversed: Vec<DashboardRow> = app.rows(cx).to_vec();
        reversed.reverse();
        app.publish_rows(cx, reversed);
        assert_ne!(
            app.row_key(cx, 2),
            pressed,
            "another object is under the mouse"
        );
        app.release(cx, row_position(2), Modifiers::default());
        app.draw(cx);
        assert_eq!(app.cursor(cx), None, "neither object was clicked");
        assert_eq!(app.pane_object(cx), None);

        // An ordinary click still selects the object under the mouse.
        app.click(cx, row_position(2), Modifiers::default());
        assert_eq!(app.cursor(cx), Some((2, app.row_key(cx, 2))));
    });
}

#[test]
fn a_cursor_whose_object_left_takes_no_action() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "j j");
        assert_eq!(app.cursor(cx).map(|(_, key)| key), Some(replication()));
        // The replication check recovers and leaves the list.
        let without: Vec<DashboardRow> = app
            .rows(cx)
            .iter()
            .filter(|row| **row != DashboardRow::Object(replication()))
            .cloned()
            .collect();
        app.publish_rows(cx, without);
        assert_eq!(app.cursor(cx), None, "the cursor detached");
        // `a` doesn't hit the row that slid into its place.
        app.keys(cx, "a");
        assert_eq!(app.state.read(cx).last_request(), None);
        // j picks that row up.
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(1));
        app.keys(cx, "a");
        let request = app.state.read(cx).last_request().cloned().unwrap();
        assert_eq!(request.targets, [app.row_key(cx, 1)]);
    });
}
