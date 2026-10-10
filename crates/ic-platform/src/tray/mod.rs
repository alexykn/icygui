//! The tray / menu-bar icon (PLAN.md D4, BG-02, REL-07).
//!
//! The icon is the logo's mark tinted with the worst unhandled state; its
//! menu has Open, Pause notifications (30 minutes, 1 hour, until 08:00),
//! Resume while paused, the environments, and Quit. Clicks arrive as
//! [`TrayCommand`]s on the receiver from [`Tray::commands`].
//!
//! Backends (`tray-icon`): an `NSStatusItem` on macOS; on Linux a
//! `StatusNotifierItem` over D-Bus (`ksni`), which needs no GTK main loop.
//! On Linux a left click on the icon opens the window; on macOS a click
//! shows the menu.
//!
//! # No tray host
//!
//! GNOME shows `StatusNotifierItem`s only with the `AppIndicator`
//! extension. Without a tray host [`Tray::new`] still succeeds and the app
//! keeps working, but nobody sees the icon or can open its menu. Check
//! [`host_available`] before relying on the tray: in particular, before
//! keeping the app running when its last window closes (BG-01), quit
//! instead or tell the user, or the app runs on invisibly with no way to
//! open or quit it.
//!
//! # Process-wide event handlers
//!
//! `tray-icon` delivers menu and icon events through process-wide
//! handlers that can be set only once. The first [`Tray::new`] installs
//! them and routes events to the most recently created tray; menu events
//! whose ids aren't the tray's are ignored. Nothing else in the process
//! may install `muda`/`tray-icon` event handlers.

mod host;
mod icon;
mod menu;

use std::cell::{Cell, RefCell};
use std::fmt;
use std::sync::{Mutex, Once, PoisonError};
use std::time::Duration;

use futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use ic_model::{CheckableState, HostState, ServiceState};
use jiff::Zoned;
use tray_icon::menu::MenuEvent;
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::PlatformError;
use menu::{Entry, MenuAction, MenuState};

/// The colour of the tray icon: the design's state colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrayTone {
    /// A critical service or a down host: `#e06c6c`.
    Critical,
    /// A warning: `#e5b04a`.
    Warning,
    /// An unknown service or an unreachable host: `#a97fdb`.
    Unknown,
    /// Nothing unhandled: `#56b870`.
    Ok,
}

impl TrayTone {
    /// The colour as RGB bytes. "No state" (`None` in
    /// [`Tray::set_state`]) is a mid grey, `#7d848a`, that stays visible on
    /// light and dark panels.
    pub const fn rgb(self) -> [u8; 3] {
        match self {
            Self::Critical => [0xe0, 0x6c, 0x6c],
            Self::Warning => [0xe5, 0xb0, 0x4a],
            Self::Unknown => [0xa9, 0x7f, 0xdb],
            Self::Ok => [0x56, 0xb8, 0x70],
        }
    }

    /// The tone of a state; `None` (grey) for pending.
    pub fn for_state(state: CheckableState) -> Option<Self> {
        match state {
            CheckableState::Host(HostState::Down)
            | CheckableState::Service(ServiceState::Critical) => Some(Self::Critical),
            CheckableState::Service(ServiceState::Warning) => Some(Self::Warning),
            CheckableState::Host(HostState::Unreachable)
            | CheckableState::Service(ServiceState::Unknown) => Some(Self::Unknown),
            CheckableState::Host(HostState::Up) | CheckableState::Service(ServiceState::Ok) => {
                Some(Self::Ok)
            }
            CheckableState::Host(HostState::Pending)
            | CheckableState::Service(ServiceState::Pending) => None,
        }
    }

    /// The tone for a connected environment's worst unhandled state
    /// (`Summary::worst_unhandled` of the snapshot's overall summary):
    /// nothing unhandled is [`TrayTone::Ok`]. For a disconnected
    /// environment pass `None` to [`Tray::set_state`] instead.
    pub fn for_worst_unhandled(worst: Option<CheckableState>) -> Option<Self> {
        worst.map_or(Some(Self::Ok), Self::for_state)
    }
}

/// What the tray icon shows: a state's tint, or the blind look (a dashed
/// grey orbit around a hollow core, the node yellow) while an
/// environment has had no live data for a while: its states may be
/// outdated, so the icon claims none of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrayLook {
    /// Tinted with this tone (`None`: grey).
    State(Option<TrayTone>),
    /// No live data.
    Blind,
}

/// What the user chose in the tray.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TrayCommand {
    /// Show the main window (create it if it is closed).
    Open,
    /// Pause notifications for this long, counted from the click. "Until
    /// 08:00" is converted to the time until the next 08:00 local time
    /// (later the same day when chosen before 08:00).
    PauseFor(Duration),
    /// Resume notifications.
    Resume,
    /// Switch to the environment with this id. Hosts that ignore disabled
    /// items may send the active environment's id; treat that as a no-op.
    SwitchEnvironment(String),
    /// Quit the app.
    Quit,
}

/// The tray / menu-bar icon and its menu.
///
/// Create it on the main thread once the event loop runs (macOS requires
/// both; the type is not `Send`, so it stays there). Dropping it removes
/// the icon. On Linux it is created even if no tray host shows it; see
/// [`host_available`].
pub struct Tray {
    icon: TrayIcon,
    /// Our end of the route, to recognise it when dropping.
    sender: UnboundedSender<TrayCommand>,
    commands: Cell<Option<UnboundedReceiver<TrayCommand>>>,
    state: RefCell<MenuState>,
    entries: RefCell<Vec<Entry>>,
    /// What the icon on screen shows.
    shown: Cell<Shown>,
    tooltip: RefCell<String>,
}

impl Tray {
    /// Shows the icon in the "no state" grey, with the default menu.
    ///
    /// # Errors
    ///
    /// - [`PlatformError::InvalidArgument`]: `app_name` is blank.
    /// - [`PlatformError::Tray`]: the icon can't be created: on macOS when
    ///   not called on the main thread, on Linux when there is no D-Bus
    ///   session bus.
    pub fn new(app_name: &str) -> Result<Self, PlatformError> {
        let app_name = menu::clean_label(app_name);
        if app_name.is_empty() {
            return Err(PlatformError::invalid("application name", "is empty"));
        }
        install_event_handlers();

        let image = icon::tray_icon(TrayLook::State(None))
            .map_err(|error| PlatformError::Tray(error.to_string()))?;
        // Linux hosts show the title as the tooltip's heading; macOS would
        // draw it as text next to the icon in the menu bar.
        let (title, tooltip) = if cfg!(target_os = "macos") {
            (None, app_name.clone())
        } else {
            (Some(app_name.clone()), String::new())
        };
        let mut builder = TrayIconBuilder::new()
            .with_id(&app_name)
            .with_icon(image)
            .with_tooltip(&tooltip);
        if let Some(title) = &title {
            builder = builder.with_title(title);
        }
        // The icon is created before the menu: on macOS, menus can only be
        // created on the main thread, and creating the icon first turns a
        // call from another thread into an error instead of a panic.
        let icon = builder
            .build()
            .map_err(|error| PlatformError::Tray(error.to_string()))?;

        let state = MenuState::new(&app_name);
        let entries = menu::entries(&state);
        let built =
            menu::build(&entries).map_err(|error| PlatformError::Tray(error.to_string()))?;
        icon.set_menu(Some(Box::new(built)));

        let (sender, receiver) = mpsc::unbounded();
        set_route(Some(sender.clone()));
        tracing::info!("tray icon created");
        Ok(Self {
            icon,
            sender,
            commands: Cell::new(Some(receiver)),
            state: RefCell::new(state),
            entries: RefCell::new(entries),
            shown: Cell::new(Shown::Look(TrayLook::State(None))),
            tooltip: RefCell::new(tooltip),
        })
    }

    /// The commands chosen in the tray, for the app's event loop. Take it
    /// once; later calls return a receiver that is already closed. The
    /// stream ends when the tray is dropped.
    pub fn commands(&self) -> UnboundedReceiver<TrayCommand> {
        self.commands.take().unwrap_or_else(|| {
            tracing::warn!("the tray's commands were taken already");
            mpsc::unbounded().1
        })
    }

    /// Shows the worst state (`None`: grey, for "not connected" or "no
    /// environment") and a tooltip, e.g. `prod-cluster · 3 critical ·
    /// 5 warning`. Unchanged values are not sent to the OS again, so this
    /// can be called for every snapshot. Failures are logged.
    pub fn set_state(&self, worst: Option<TrayTone>, tooltip: &str) {
        self.set_look(TrayLook::State(worst), tooltip);
    }

    /// [`Tray::set_state`] for any look, the blind one too.
    pub fn set_look(&self, look: TrayLook, tooltip: &str) {
        if self.shown.get() != Shown::Look(look) {
            let shown = icon::tray_icon(look)
                .map_err(|error| error.to_string())
                .and_then(|image| {
                    self.icon
                        .set_icon(Some(image))
                        .map_err(|error| error.to_string())
                });
            match shown {
                Ok(()) => self.shown.set(Shown::Look(look)),
                Err(error) => {
                    self.shown.set(Shown::Unknown);
                    tracing::warn!(%error, "cannot update the tray icon");
                }
            }
        }

        let tooltip = tooltip_text(tooltip);
        if *self.tooltip.borrow() != tooltip {
            match self.icon.set_tooltip(Some(&tooltip)) {
                Ok(()) => *self.tooltip.borrow_mut() = tooltip,
                Err(error) => tracing::warn!(%error, "cannot update the tray tooltip"),
            }
        }
    }

    /// Lists the environments, as `(id, name)` pairs in display order, in
    /// the menu's environment lines, each with its status and no check
    /// mark (16e; `active` is kept for the menu's state). Choosing one sends
    /// [`TrayCommand::SwitchEnvironment`] and changes nothing in the menu
    /// by itself. Call this again once the active environment has changed,
    /// and whenever the list changes.
    pub fn set_environments(&self, environments: &[(String, String)], active: Option<&str>) {
        self.update_menu(|state| {
            state.environments = environments.to_vec();
            state.active = active.map(str::to_owned);
        });
    }

    /// What each environment's line says after its name, as `(id,
    /// status)` (`live`, `no data 3m`). Keep it coarse: a changed status
    /// rebuilds the menu, which closes it on macOS.
    pub fn set_environment_statuses(&self, statuses: &[(String, String)]) {
        self.update_menu(|state| state.statuses = statuses.to_vec());
    }

    /// Shows `Paused until …` (with this text) and `Resume notifications`
    /// while paused (`Some`, e.g. `"18:30"` or `"tomorrow 08:00"`, as the
    /// app formats it), or hides them (`None`).
    pub fn set_paused(&self, paused_until: Option<String>) {
        self.update_menu(|state| state.paused_until = paused_until);
    }

    /// Applies `change` and rebuilds the menu if its entries changed.
    fn update_menu(&self, change: impl FnOnce(&mut MenuState)) {
        let mut state = self.state.borrow().clone();
        change(&mut state);
        let entries = menu::entries(&state);
        if *self.entries.borrow() != entries {
            match menu::build(&entries) {
                Ok(built) => {
                    self.icon.set_menu(Some(Box::new(built)));
                    *self.entries.borrow_mut() = entries;
                }
                Err(error) => {
                    tracing::warn!(%error, "cannot rebuild the tray menu");
                    return;
                }
            }
        }
        *self.state.borrow_mut() = state;
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let mut route = lock_route();
        if route
            .as_ref()
            .is_some_and(|sender| sender.same_receiver(&self.sender))
        {
            *route = None;
        }
    }
}

impl fmt::Debug for Tray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tray")
            .field("id", &self.icon.id().0)
            .field("shown", &self.shown.get())
            .field("tooltip", &self.tooltip.borrow())
            .field("menu", &self.state.borrow())
            .finish_non_exhaustive()
    }
}

/// Whether a tray host shows the icon: always on macOS; on Linux and the
/// BSDs only while a `StatusNotifierWatcher` with a registered host runs on
/// the session bus (not on stock GNOME, which needs the `AppIndicator`
/// extension).
///
/// Asks the session bus and blocks briefly (milliseconds; at most two
/// seconds if the bus doesn't answer, which counts as no host). A host can
/// come and go (a panel restarts, an extension is switched on), so ask
/// again when it matters, for example each time the window closes. With no
/// host, closing the window must not leave the app running unseen (BG-01).
pub fn host_available() -> bool {
    host::available()
}

/// The tooltip as one or more plain lines: control characters other than
/// line breaks become spaces.
fn tooltip_text(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect()
}

/// What the icon on screen shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shown {
    /// Unknown: the last update failed, so the next one is sent again.
    Unknown,
    /// This look.
    Look(TrayLook),
}

/// Where tray events go: the command channel of the live [`Tray`].
static ROUTE: Mutex<Option<UnboundedSender<TrayCommand>>> = Mutex::new(None);

fn lock_route() -> std::sync::MutexGuard<'static, Option<UnboundedSender<TrayCommand>>> {
    ROUTE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set_route(sender: Option<UnboundedSender<TrayCommand>>) {
    *lock_route() = sender;
}

/// Installs the process-wide menu and icon event handlers, once. Without
/// them `tray-icon` queues every event (including mouse moves over the
/// icon on macOS) in channels nobody reads.
fn install_event_handlers() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        MenuEvent::set_event_handler(Some(|event: MenuEvent| on_menu_event(&event)));
        TrayIconEvent::set_event_handler(Some(|event: TrayIconEvent| on_icon_event(&event)));
    });
}

/// Runs on the thread the backend reports clicks on (the main thread on
/// macOS, the D-Bus thread on Linux); it only sends on a channel.
fn on_menu_event(event: &MenuEvent) {
    if let Some(action) = MenuAction::from_id(event.id().as_ref()) {
        deliver(action.command(Zoned::now));
    } else {
        tracing::debug!(id = %event.id().0, "ignoring a menu event without an action");
    }
}

fn on_icon_event(event: &TrayIconEvent) {
    if let Some(command) = command_for_icon_event(event) {
        deliver(command);
    }
}

/// A left click on the icon opens the window where a click doesn't show
/// the menu: `StatusNotifierItem` hosts call `Activate` on a left click and
/// show the menu on a right click. On macOS a click shows the menu.
fn command_for_icon_event(event: &TrayIconEvent) -> Option<TrayCommand> {
    if cfg!(target_os = "macos") {
        return None;
    }
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => Some(TrayCommand::Open),
        _ => None,
    }
}

fn deliver(command: TrayCommand) {
    tracing::debug!(?command, "tray command");
    let route = lock_route();
    let sent = route
        .as_ref()
        .is_some_and(|sender| sender.unbounded_send(command).is_ok());
    if !sent {
        tracing::debug!("tray command dropped: no tray or nobody listening");
    }
}

#[cfg(test)]
mod tests {
    use tray_icon::{Rect, TrayIconId, dpi::PhysicalPosition};

    use super::*;

    fn click(button: MouseButton, button_state: MouseButtonState) -> TrayIconEvent {
        TrayIconEvent::Click {
            id: TrayIconId::new("icygui"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
            button_state,
        }
    }

    #[test]
    fn tones_follow_the_design_states() {
        use CheckableState::{Host, Service};
        for (state, tone) in [
            (Host(HostState::Down), Some(TrayTone::Critical)),
            (Service(ServiceState::Critical), Some(TrayTone::Critical)),
            (Service(ServiceState::Warning), Some(TrayTone::Warning)),
            (Host(HostState::Unreachable), Some(TrayTone::Unknown)),
            (Service(ServiceState::Unknown), Some(TrayTone::Unknown)),
            (Host(HostState::Up), Some(TrayTone::Ok)),
            (Service(ServiceState::Ok), Some(TrayTone::Ok)),
            (Host(HostState::Pending), None),
            (Service(ServiceState::Pending), None),
        ] {
            assert_eq!(TrayTone::for_state(state), tone, "{state:?}");
        }
        assert_eq!(TrayTone::for_worst_unhandled(None), Some(TrayTone::Ok));
        assert_eq!(
            TrayTone::for_worst_unhandled(Some(Service(ServiceState::Warning))),
            Some(TrayTone::Warning)
        );
    }

    #[test]
    fn left_click_opens_except_on_macos() {
        let open = if cfg!(target_os = "macos") {
            None
        } else {
            Some(TrayCommand::Open)
        };
        assert_eq!(
            command_for_icon_event(&click(MouseButton::Left, MouseButtonState::Up)),
            open
        );
        assert_eq!(
            command_for_icon_event(&click(MouseButton::Left, MouseButtonState::Down)),
            None
        );
        assert_eq!(
            command_for_icon_event(&click(MouseButton::Right, MouseButtonState::Up)),
            None
        );
        assert_eq!(
            command_for_icon_event(&click(MouseButton::Middle, MouseButtonState::Up)),
            None
        );
    }

    #[test]
    fn tooltips_keep_line_breaks_only() {
        assert_eq!(
            tooltip_text("prod · 3 critical\n5 warning\t\u{1b}x"),
            "prod · 3 critical\n5 warning  x"
        );
    }

    /// Clicks in the menu as the Linux host performs them (activating
    /// muda's snapshot items, which is what the ksni backend does) reach
    /// the command channel through the real event handler.
    ///
    /// This is the only test that touches the process-wide route.
    #[cfg(target_os = "linux")]
    #[test]
    fn menu_clicks_arrive_as_commands() {
        use tray_icon::menu::{ContextMenu as _, MenuItemKindSnapshot};

        install_event_handlers();
        let (sender, mut receiver) = mpsc::unbounded();
        set_route(Some(sender));

        let mut state = MenuState::new("icygui");
        state.environments = vec![
            ("env-a".to_owned(), "prod".to_owned()),
            ("env-b".to_owned(), "staging".to_owned()),
        ];
        state.active = Some("env-a".to_owned());
        state.paused_until = Some("18:30".to_owned());
        let built = menu::build(&menu::entries(&state)).unwrap();
        let items = built.snapshot_handle().items();

        let activate = |items: &[MenuItemKindSnapshot], text: &str| {
            let found = items.iter().find_map(|item| match item {
                MenuItemKindSnapshot::MenuItem(item) if item.text() == text => {
                    Some(item.activate.clone())
                }
                _ => None,
            });
            found.unwrap_or_else(|| panic!("no item {text:?}"))();
        };
        let submenu = |text: &str| {
            items
                .iter()
                .find_map(|item| match item {
                    MenuItemKindSnapshot::Submenu(submenu) if submenu.text() == text => {
                        Some(submenu.items())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no submenu {text:?}"))
        };

        let environment_lines = |items: &[MenuItemKindSnapshot]| -> Vec<String> {
            menu::tests::rendered::shown(items)
                .into_iter()
                .filter(|line| line.contains("prod") || line.contains("staging"))
                .collect()
        };
        let environments_before = environment_lines(&items);
        activate(&items, "Open icygui");
        activate(&items, "Resume notifications");
        activate(&submenu("Pause notifications"), "For 30 minutes");
        activate(&submenu("Pause notifications"), "Until 08:00");
        activate(&items, "staging");
        activate(&items, "Paused until 18:30");
        activate(&items, "Quit icygui");
        on_icon_event(&click(MouseButton::Left, MouseButtonState::Up));

        // A click changes nothing in the menu by itself; no line carries a
        // check mark (16e).
        assert_eq!(environment_lines(&items), environments_before);
        assert_eq!(
            environments_before,
            ["prod", "staging"],
            "{environments_before:?}"
        );

        let mut commands = Vec::new();
        while let Ok(command) = receiver.try_recv() {
            commands.push(command);
        }
        assert_eq!(commands.len(), 7, "{commands:?}");
        assert_eq!(commands[0], TrayCommand::Open);
        assert_eq!(commands[1], TrayCommand::Resume);
        assert_eq!(commands[2], TrayCommand::PauseFor(Duration::from_mins(30)));
        let TrayCommand::PauseFor(morning) = commands[3] else {
            panic!("{:?}", commands[3]);
        };
        assert!(
            morning > Duration::ZERO && morning <= Duration::from_hours(25),
            "{morning:?}"
        );
        assert_eq!(
            commands[4],
            TrayCommand::SwitchEnvironment("env-b".to_owned())
        );
        // The status line is disabled and has no action: nothing for it.
        assert_eq!(commands[5], TrayCommand::Quit);
        assert_eq!(commands[6], TrayCommand::Open);

        // Without a route, events are dropped quietly.
        set_route(None);
        activate(&items, "Open icygui");
        assert!(receiver.try_recv().is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn creating_the_tray_off_the_main_thread_is_an_error() {
        // A thread of our own is never the main thread, whatever the test
        // runner does; the menu must not be built there (muda would panic).
        let outcome = std::thread::spawn(|| match Tray::new("icygui") {
            Ok(_) => Err("created a tray off the main thread".to_owned()),
            Err(PlatformError::Tray(message)) => Ok(message),
            Err(other) => Err(other.to_string()),
        })
        .join()
        .unwrap();
        assert!(outcome.is_ok(), "{outcome:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_menu_bar_always_has_a_host() {
        assert!(host_available());
    }

    #[test]
    fn a_blank_app_name_is_rejected() {
        let error = Tray::new(" \n ").unwrap_err();
        assert!(
            matches!(error, PlatformError::InvalidArgument { .. }),
            "{error}"
        );
    }
}
