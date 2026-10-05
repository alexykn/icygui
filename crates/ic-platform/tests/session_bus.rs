//! The adapters against a real D-Bus session bus and a real process
//! environment (Linux): the tray as a tray host sees it over D-Bus,
//! tray-host detection, the keyring without a Secret Service, launch at
//! login with its directory taken from the environment, and autostart
//! entries started by `GLib`.
//!
//! Every test runs its body in a child process of its own (this test binary
//! again). Most children run under `dbus-run-session` with a private bus
//! whose configuration has no activatable services, so nothing reaches the
//! user's session, panel or keyring; the environment variables a test needs
//! are set for its child only. When a tool is missing (`dbus-run-session`,
//! `gio`), the test is skipped with a note.

#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "test harness and helpers: a failure should panic the test"
)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use futures::channel::mpsc::{TryRecvError, UnboundedReceiver};
use ic_core::ports::SecretStore as _;
use ic_platform::tray::{self, Tray, TrayCommand, TrayTone};
use ic_platform::{KeyringSecrets, PlatformError, autostart};
use zbus::blocking::Connection;
use zbus::blocking::fdo::DBusProxy;
use zbus::names::BusName;
use zbus::zvariant::{OwnedValue, Value};

/// Set in the child process, which runs the test's body.
const CHILD: &str = "IC_PLATFORM_TEST_CHILD";
const APP_ID: &str = "io.github.alexykn.icygui";
/// The tray icon's "no state" grey.
const GREY: [u8; 3] = [0x7d, 0x84, 0x8a];

// ---------------------------------------------------------------------------
// Child processes

/// The D-Bus session bus a child process gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bus {
    /// A private bus from `dbus-run-session`, without activatable services.
    Private,
    /// None: the address points to a socket that doesn't exist.
    Missing,
}

fn is_child() -> bool {
    env::var_os(CHILD).is_some()
}

/// Runs the test named `test` again in a child process with `bus` and the
/// extra environment `vars`, and fails if the child fails.
fn run_child(test: &str, bus: Bus, vars: &[(&str, OsString)]) {
    let exe = env::current_exe().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let mut command = match bus {
        Bus::Private => {
            let config = temp.path().join("session.conf");
            fs::write(&config, bus_config(temp.path())).unwrap();
            let mut command = Command::new("dbus-run-session");
            command
                .arg(format!("--config-file={}", config.display()))
                .arg("--")
                .arg(&exe);
            command
        }
        Bus::Missing => {
            let mut command = Command::new(&exe);
            let socket = temp.path().join("no-bus-here");
            command.env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", socket.display()),
            );
            command
        }
    };
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1");
    for (key, value) in vars {
        command.env(key, value);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound && bus == Bus::Private => {
            eprintln!("skipped {test}: dbus-run-session is not installed");
            return;
        }
        Err(error) => panic!("cannot start the child process for {test}: {error}"),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{test} failed in its child process ({}):\n--- stdout\n{stdout}\n--- stderr\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains("1 passed"),
        "the child process didn't run {test}:\n{stdout}"
    );
    for note in stdout.lines().filter(|line| line.starts_with("skipped:")) {
        eprintln!("{test}: {note}");
    }
}

/// A session bus configuration with no service directories, so nothing is
/// started on demand (no Secret Service, no notification daemon).
fn bus_config(dir: &Path) -> String {
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:dir={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
        dir.display()
    )
}

/// Polls `condition` until it holds; fails after ten seconds.
fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn thread_count() -> usize {
    fs::read_dir("/proc/self/task").unwrap().count()
}

// ---------------------------------------------------------------------------
// The tray as a StatusNotifierItem host sees it

/// The bus names of this process's tray items, oldest first.
fn tray_items(bus: &Connection) -> Vec<String> {
    let prefix = format!("org.kde.StatusNotifierItem-{}-", std::process::id());
    let mut names: Vec<(u64, String)> = DBusProxy::new(bus)
        .unwrap()
        .list_names()
        .unwrap()
        .into_iter()
        .filter_map(|name| {
            let name = name.to_string();
            let instance = name.strip_prefix(&prefix)?.parse().ok()?;
            Some((instance, name))
        })
        .collect();
    names.sort();
    names.into_iter().map(|(_, name)| name).collect()
}

/// The bus name of the only tray item of this process.
fn tray_item(bus: &Connection) -> String {
    let items = tray_items(bus);
    assert_eq!(items.len(), 1, "{items:?}");
    items[0].clone()
}

fn has_owner(bus: &Connection, name: &str) -> bool {
    DBusProxy::new(bus)
        .unwrap()
        .name_has_owner(BusName::try_from(name).unwrap())
        .unwrap()
}

/// A dbusmenu item on the wire: id, properties, children.
type RawMenuNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);
/// Icons on the wire: width, height, ARGB pixels.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

/// A dbusmenu item.
#[derive(Debug)]
struct MenuNode {
    id: i32,
    properties: HashMap<String, OwnedValue>,
    children: Vec<MenuNode>,
}

impl MenuNode {
    fn from_value(value: OwnedValue) -> Self {
        let (id, properties, children): RawMenuNode = value.try_into().unwrap();
        Self::new(id, properties, children)
    }

    fn new(id: i32, properties: HashMap<String, OwnedValue>, children: Vec<OwnedValue>) -> Self {
        Self {
            id,
            properties,
            children: children.into_iter().map(Self::from_value).collect(),
        }
    }

    fn string(&self, key: &str) -> Option<String> {
        self.properties
            .get(key)
            .map(|value| String::try_from(value.try_clone().unwrap()).unwrap())
    }

    /// The label as the host shows it: dbusmenu marks a mnemonic with `_`
    /// and writes a literal `_` as `__`.
    fn label(&self) -> String {
        let raw = self.string("label").unwrap_or_default();
        let mut shown = String::new();
        let mut chars = raw.chars();
        while let Some(c) = chars.next() {
            if c == '_' {
                shown.extend(chars.next());
            } else {
                shown.push(c);
            }
        }
        shown
    }

    fn enabled(&self) -> bool {
        self.properties
            .get("enabled")
            .is_none_or(|value| bool::try_from(value.try_clone().unwrap()).unwrap())
    }

    fn is_separator(&self) -> bool {
        self.string("type").as_deref() == Some("separator")
    }

    /// The children as text: `label`, `label (off)` when disabled, `---`
    /// for separators and `label ▸ a / b` for submenus.
    fn shown(&self) -> Vec<String> {
        self.children
            .iter()
            .map(|child| {
                if child.is_separator() {
                    return "---".to_owned();
                }
                let mut text = child.label();
                if !child.enabled() {
                    text.push_str(" (off)");
                }
                if !child.children.is_empty() {
                    text = format!("{text} ▸ {}", child.shown().join(" / "));
                }
                text
            })
            .collect()
    }

    /// The item with this visible label, anywhere below.
    fn find(&self, label: &str) -> &MenuNode {
        self.search(label)
            .unwrap_or_else(|| panic!("no menu item {label:?} in {:?}", self.shown()))
    }

    fn search(&self, label: &str) -> Option<&MenuNode> {
        self.children.iter().find_map(|child| {
            if !child.is_separator() && child.label() == label {
                Some(child)
            } else {
                child.search(label)
            }
        })
    }

    /// Whether any item below is a check or radio item.
    fn has_toggles(&self) -> bool {
        self.children
            .iter()
            .any(|child| child.properties.contains_key("toggle-type") || child.has_toggles())
    }
}

fn menu(bus: &Connection, item: &str) -> MenuNode {
    let reply = bus
        .call_method(
            Some(item),
            "/MenuBar",
            Some("com.canonical.dbusmenu"),
            "GetLayout",
            &(0_i32, -1_i32, Vec::<String>::new()),
        )
        .unwrap();
    let (_revision, (id, properties, children)): (u32, RawMenuNode) =
        reply.body().deserialize().unwrap();
    MenuNode::new(id, properties, children)
}

/// The Environment submenu as text.
fn environments(bus: &Connection, item: &str) -> Vec<String> {
    let menu = menu(bus, item);
    menu.find("Environment")
        .children
        .iter()
        .map(|child| {
            let mut text = child.label();
            if !child.enabled() {
                text.push_str(" (off)");
            }
            text
        })
        .collect()
}

/// An environment's name without the mark or indentation in front.
fn environment_name(label: &str) -> &str {
    label
        .strip_prefix("✓ ")
        .or_else(|| label.strip_prefix('\u{2003}'))
        .unwrap_or(label)
}

/// The environments the host shows as the current one: checked (a check
/// item's toggle state) or marked with `✓`.
fn current_environments(bus: &Connection, item: &str) -> Vec<String> {
    let menu = menu(bus, item);
    menu.find("Environment")
        .children
        .iter()
        .filter(|child| {
            let checked = child
                .properties
                .get("toggle-state")
                .is_some_and(|state| i32::try_from(state.try_clone().unwrap()).unwrap() == 1);
            checked || child.label().starts_with('✓')
        })
        .map(|child| environment_name(&child.label()).to_owned())
        .collect()
}

/// Clicks the environment with this name, as a host does.
fn click_environment(bus: &Connection, item: &str, name: &str) {
    let menu = menu(bus, item);
    let environment = menu
        .find("Environment")
        .children
        .iter()
        .find(|child| environment_name(&child.label()) == name)
        .unwrap_or_else(|| panic!("no environment {name:?}"));
    send_click(bus, item, environment.id);
}

/// Clicks the item with this visible label, as a host does.
fn click(bus: &Connection, item: &str, label: &str) {
    send_click(bus, item, menu(bus, item).find(label).id);
}

fn send_click(bus: &Connection, item: &str, id: i32) {
    bus.call_method(
        Some(item),
        "/MenuBar",
        Some("com.canonical.dbusmenu"),
        "Event",
        &(id, "clicked", Value::from(0_i32), 0_u32),
    )
    .unwrap();
}

fn item_property(bus: &Connection, item: &str, name: &str) -> OwnedValue {
    bus.call_method(
        Some(item),
        "/StatusNotifierItem",
        Some("org.freedesktop.DBus.Properties"),
        "Get",
        &("org.kde.StatusNotifierItem", name),
    )
    .unwrap()
    .body()
    .deserialize()
    .unwrap()
}

/// The tooltip's title and text.
fn tooltip(bus: &Connection, item: &str) -> (String, String) {
    let (_icon_name, _icons, title, text): (String, Pixmaps, String, String) =
        item_property(bus, item, "ToolTip").try_into().unwrap();
    (title, text)
}

/// The colour in the middle of the icon (the core of the mark), as RGB.
fn icon_core(bus: &Connection, item: &str) -> [u8; 3] {
    let icons: Pixmaps = item_property(bus, item, "IconPixmap").try_into().unwrap();
    assert_eq!(icons.len(), 1);
    let (width, height, argb) = &icons[0];
    let (width, height) = (
        usize::try_from(*width).unwrap(),
        usize::try_from(*height).unwrap(),
    );
    assert_eq!(argb.len(), width * height * 4);
    let index = (height / 2 * width + width / 2) * 4;
    // Network byte order ARGB.
    assert_eq!(argb[index], 0xff, "opaque");
    [argb[index + 1], argb[index + 2], argb[index + 3]]
}

/// The next command from the tray; fails after ten seconds.
fn next_command(commands: &mut UnboundedReceiver<TrayCommand>) -> TrayCommand {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match commands.try_recv() {
            Ok(command) => return command,
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("no tray command: {error:?}"),
        }
    }
}

/// Fails if a command arrives within a short while.
fn assert_no_command(commands: &mut UnboundedReceiver<TrayCommand>) {
    thread::sleep(Duration::from_millis(100));
    match commands.try_recv() {
        Err(TryRecvError::Empty) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

fn some_environments() -> Vec<(String, String)> {
    let long = "x".repeat(300);
    [
        ("env-a", "prod"),
        ("env-b", "staging"),
        ("env-c", "R&D"),
        ("env-d", "prod_cluster"),
        ("env-e", "line\nbreak"),
        ("env-f", ""),
        ("env-g", &long),
    ]
    .into_iter()
    .map(|(id, name)| (id.to_owned(), name.to_owned()))
    .collect()
}

/// How the host shows the environments after prod and staging: names as
/// given (`&` and `_` survive the mnemonic syntax), one line, the id for a
/// blank name, long names shortened.
fn other_environments() -> Vec<String> {
    [
        "R&D".to_owned(),
        "prod_cluster".to_owned(),
        "line break".to_owned(),
        "env-f".to_owned(),
        format!("{}…", "x".repeat(59)),
    ]
    .into_iter()
    .map(|name| format!("\u{2003}{name}"))
    .collect()
}

/// The expected Environment submenu: these two first, then the others.
fn shown_environments(prod: &str, staging: &str) -> Vec<String> {
    let mut shown = vec![prod.to_owned(), staging.to_owned()];
    shown.extend(other_environments());
    shown
}

#[test]
fn tray_menu_over_dbus() {
    if !is_child() {
        run_child("tray_menu_over_dbus", Bus::Private, &[]);
        return;
    }
    let bus = Connection::session().unwrap();
    let tray = Tray::new("icygui").unwrap();
    let mut commands = tray.commands();
    let item = tray_item(&bus);

    assert_eq!(
        menu(&bus, &item).shown(),
        [
            "Open icygui",
            "---",
            "Pause notifications ▸ For 30 minutes / For 1 hour / Until 08:00",
            "---",
            "Quit icygui",
        ]
    );

    let list = some_environments();
    tray.set_environments(&list, Some("env-a"));
    assert_eq!(current_environments(&bus, &item), ["prod"]);

    // The user picks staging, but the switch doesn't happen (it fails, or
    // the app refuses): prod stays the only current one, both before and
    // after the app sends the unchanged list again.
    click_environment(&bus, &item, "staging");
    assert_eq!(
        next_command(&mut commands),
        TrayCommand::SwitchEnvironment("env-b".to_owned())
    );
    assert_eq!(current_environments(&bus, &item), ["prod"]);
    tray.set_environments(&list, Some("env-a"));
    assert_eq!(current_environments(&bus, &item), ["prod"]);

    // How the host shows it: plain items, the active one marked and
    // disabled.
    assert_eq!(
        environments(&bus, &item),
        shown_environments("✓ prod (off)", "\u{2003}staging")
    );
    assert!(
        !menu(&bus, &item).has_toggles(),
        "no check items: muda ticks those itself when clicked"
    );

    // A switch that happens moves the mark.
    tray.set_environments(&list, Some("env-b"));
    assert_eq!(current_environments(&bus, &item), ["staging"]);
    assert_eq!(
        environments(&bus, &item),
        shown_environments("\u{2003}prod", "✓ staging (off)")
    );
    click_environment(&bus, &item, "prod_cluster");
    assert_eq!(
        next_command(&mut commands),
        TrayCommand::SwitchEnvironment("env-d".to_owned())
    );
    assert_eq!(current_environments(&bus, &item), ["staging"]);

    click(&bus, &item, "Open icygui");
    assert_eq!(next_command(&mut commands), TrayCommand::Open);
    click(&bus, &item, "For 30 minutes");
    assert_eq!(
        next_command(&mut commands),
        TrayCommand::PauseFor(Duration::from_mins(30))
    );
    click(&bus, &item, "Until 08:00");
    let TrayCommand::PauseFor(pause) = next_command(&mut commands) else {
        panic!("not a pause");
    };
    assert!(
        pause > Duration::ZERO && pause <= Duration::from_hours(25),
        "{pause:?}"
    );

    tray.set_paused(Some("18:30".to_owned()));
    assert_eq!(
        menu(&bus, &item).shown()[2..4],
        ["Paused until 18:30 (off)", "Resume notifications"]
    );
    click(&bus, &item, "Resume notifications");
    assert_eq!(next_command(&mut commands), TrayCommand::Resume);
    // The status line has no action, even for hosts that click it anyway.
    click(&bus, &item, "Paused until 18:30");
    assert_no_command(&mut commands);
    tray.set_paused(None);
    assert!(
        !menu(&bus, &item)
            .shown()
            .iter()
            .any(|line| line.contains("Paused"))
    );

    // A left click on the icon opens the window.
    bus.call_method(
        Some(item.as_str()),
        "/StatusNotifierItem",
        Some("org.kde.StatusNotifierItem"),
        "Activate",
        &(0_i32, 0_i32),
    )
    .unwrap();
    assert_eq!(next_command(&mut commands), TrayCommand::Open);

    click(&bus, &item, "Quit icygui");
    assert_eq!(next_command(&mut commands), TrayCommand::Quit);
    assert_no_command(&mut commands);
}

#[test]
fn tray_state_over_dbus() {
    if !is_child() {
        run_child("tray_state_over_dbus", Bus::Private, &[]);
        return;
    }
    let bus = Connection::session().unwrap();
    let tray = Tray::new("icygui").unwrap();
    let item = tray_item(&bus);

    let id = String::try_from(item_property(&bus, &item, "Id")).unwrap();
    assert_eq!(id, "icygui");
    assert_eq!(icon_core(&bus, &item), GREY, "not connected yet");
    assert_eq!(tooltip(&bus, &item), ("icygui".to_owned(), String::new()));

    tray.set_state(Some(TrayTone::Critical), "prod · 3 critical\n5 warning");
    assert_eq!(icon_core(&bus, &item), TrayTone::Critical.rgb());
    assert_eq!(
        tooltip(&bus, &item),
        (
            "icygui".to_owned(),
            "prod · 3 critical\n5 warning".to_owned()
        )
    );

    // Unchanged values are fine to send again (every snapshot does).
    tray.set_state(Some(TrayTone::Critical), "prod · 3 critical\n5 warning");
    assert_eq!(icon_core(&bus, &item), TrayTone::Critical.rgb());

    tray.set_state(Some(TrayTone::Ok), "prod\tall fine\u{7}");
    assert_eq!(icon_core(&bus, &item), TrayTone::Ok.rgb());
    assert_eq!(tooltip(&bus, &item).1, "prod all fine ");

    tray.set_state(None, "prod · not connected");
    assert_eq!(icon_core(&bus, &item), GREY);
    assert_eq!(tooltip(&bus, &item).1, "prod · not connected");
}

#[test]
fn tray_lifecycle_over_dbus() {
    if !is_child() {
        run_child("tray_lifecycle_over_dbus", Bus::Private, &[]);
        return;
    }
    let bus = Connection::session().unwrap();

    // Created although no tray host runs (nobody would show it; see
    // `host_available`).
    let tray = Tray::new("icygui").unwrap();
    let mut commands = tray.commands();
    let mut taken_again = tray.commands();
    assert!(
        matches!(taken_again.try_recv(), Err(TryRecvError::Closed)),
        "a second receiver is closed"
    );
    let item = tray_item(&bus);
    drop(tray);
    assert!(
        matches!(commands.try_recv(), Err(TryRecvError::Closed)),
        "the stream ends with the tray"
    );
    wait_until("the item to leave the bus", || !has_owner(&bus, &item));

    // Events go to the most recently created tray.
    let first = Tray::new("icygui").unwrap();
    let mut first_commands = first.commands();
    let second = Tray::new("icygui").unwrap();
    let mut second_commands = second.commands();
    let items = tray_items(&bus);
    assert_eq!(items.len(), 2, "{items:?}");
    click(&bus, &items[1], "Open icygui");
    assert_eq!(next_command(&mut second_commands), TrayCommand::Open);
    assert_no_command(&mut first_commands);
    drop(second);
    drop(first);
    wait_until("both items to leave the bus", || {
        tray_items(&bus).is_empty()
    });

    // Creating and dropping trays leaks no threads.
    let baseline = thread_count();
    for _ in 0..20 {
        let tray = Tray::new("icygui").unwrap();
        tray.set_state(Some(TrayTone::Warning), "x");
        drop(tray);
    }
    wait_until("the trays' threads to end", || {
        thread_count() <= baseline + 1
    });
    wait_until("the items to leave the bus", || tray_items(&bus).is_empty());
}

// ---------------------------------------------------------------------------
// Tray host detection

/// The items that registered with a fake watcher.
type Registered = Arc<Mutex<Vec<String>>>;

/// A `StatusNotifierWatcher` that says whether a host is registered.
struct Watcher {
    host_registered: bool,
    items: Registered,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: &str) {
        self.items.lock().unwrap().push(service.to_owned());
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        self.host_registered
    }

    #[zbus(property)]
    fn registered_status_notifier_items(&self) -> Vec<String> {
        self.items.lock().unwrap().clone()
    }
}

/// A watcher without the `IsStatusNotifierHostRegistered` property.
struct TerseWatcher {
    items: Registered,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl TerseWatcher {
    fn register_status_notifier_item(&self, service: &str) {
        self.items.lock().unwrap().push(service.to_owned());
    }
}

fn serve_watcher<I: zbus::object_server::Interface>(watcher: I) -> Connection {
    zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at("/StatusNotifierWatcher", watcher)
        .unwrap()
        .build()
        .unwrap()
}

#[test]
fn tray_host_detection_over_dbus() {
    if !is_child() {
        run_child("tray_host_detection_over_dbus", Bus::Private, &[]);
        return;
    }
    let bus = Connection::session().unwrap();
    assert!(!tray::host_available(), "no watcher on the bus");

    let items = Registered::default();
    let watcher = serve_watcher(Watcher {
        host_registered: true,
        items: Arc::clone(&items),
    });
    assert!(tray::host_available());
    // The tray registers with the watcher like with a real one.
    let tray = Tray::new("icygui").unwrap();
    let item = tray_item(&bus);
    wait_until("the tray to register", || {
        items.lock().unwrap().contains(&item)
    });
    drop(tray);
    drop(watcher);
    wait_until("the watcher to leave", || !tray::host_available());

    let watcher = serve_watcher(Watcher {
        host_registered: false,
        items: Registered::default(),
    });
    assert!(!tray::host_available(), "a watcher without a host");
    drop(watcher);

    let watcher = serve_watcher(TerseWatcher {
        items: Registered::default(),
    });
    assert!(tray::host_available(), "benefit of the doubt");
    drop(watcher);
}

// ---------------------------------------------------------------------------
// Without a session bus, and without a Secret Service

#[test]
fn without_a_session_bus() {
    if !is_child() {
        run_child("without_a_session_bus", Bus::Missing, &[]);
        return;
    }
    let started = Instant::now();
    let error = Tray::new("icygui").unwrap_err();
    assert!(matches!(error, PlatformError::Tray(_)), "{error}");
    assert!(!tray::host_available());
    let error = KeyringSecrets::new().get("env").unwrap_err();
    assert!(
        error
            .message
            .starts_with("cannot open the Secret Service: "),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(5), "fails fast");
}

#[test]
fn keyring_without_a_secret_service() {
    if !is_child() {
        run_child("keyring_without_a_secret_service", Bus::Private, &[]);
        return;
    }
    let secrets = KeyringSecrets::new();
    let check = |error: ic_core::ports::SecretError| {
        assert!(
            error
                .message
                .starts_with("cannot open the Secret Service: "),
            "{error}"
        );
        assert!(
            error.message.contains("org.freedesktop.secrets"),
            "names the missing service: {error}"
        );
    };
    // Warm up whatever zbus starts once per process.
    check(secrets.get("env").unwrap_err());
    let baseline = thread_count();
    let started = Instant::now();
    for _ in 0..30 {
        check(secrets.get("env").unwrap_err());
        check(
            secrets
                .set("env", &secrecy::SecretString::from("pw"))
                .unwrap_err(),
        );
        check(secrets.delete("env").unwrap_err());
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    wait_until("the connection threads to end", || {
        thread_count() <= baseline + 1
    });
    assert!(format!("{secrets:?}").contains(r#"state: "closed""#));
}

// ---------------------------------------------------------------------------
// Launch at login, with the directory from the environment

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn launch_at_login_in_xdg_config_home() {
    if !is_child() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config");
        run_child(
            "launch_at_login_in_xdg_config_home",
            Bus::Missing,
            &[
                ("XDG_CONFIG_HOME", config.clone().into()),
                ("HOME", temp.path().join("home").into()),
            ],
        );
        // Created private, and the entry is gone again.
        assert_eq!(mode(&config), 0o700);
        assert_eq!(mode(&config.join("autostart")), 0o700);
        assert!(
            !config
                .join("autostart")
                .join("io.github.alexykn.icygui.desktop")
                .exists()
        );
        assert!(!temp.path().join("home").exists(), "HOME not used");
        return;
    }
    let config = PathBuf::from(env::var_os("XDG_CONFIG_HOME").unwrap());
    let entry = config.join("autostart").join(format!("{APP_ID}.desktop"));
    let exe = Path::new("/opt/icygui/bin/icygui");

    assert!(!autostart::is_enabled(APP_ID));
    autostart::set_enabled(true, APP_ID, "icygui", exe).unwrap();
    assert!(autostart::is_enabled(APP_ID));
    let contents = fs::read_to_string(&entry).unwrap();
    assert!(
        contents.contains("\nExec=/opt/icygui/bin/icygui --background\n"),
        "{contents}"
    );
    assert_eq!(mode(&entry), 0o644);

    autostart::set_enabled(false, APP_ID, "icygui", exe).unwrap();
    assert!(!autostart::is_enabled(APP_ID));
    assert!(!entry.exists());
}

#[test]
fn launch_at_login_falls_back_to_home() {
    if !is_child() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        run_child(
            "launch_at_login_falls_back_to_home",
            Bus::Missing,
            &[
                // Relative: ignored, as the XDG spec says.
                ("XDG_CONFIG_HOME", "relative/config".into()),
                ("HOME", home.clone().into()),
            ],
        );
        let autostart = home.join(".config").join("autostart");
        assert!(autostart.join("io.github.alexykn.icygui.desktop").exists());
        assert_eq!(mode(&home.join(".config")), 0o700);
        return;
    }
    let exe = Path::new("/opt/icygui/bin/icygui");
    autostart::set_enabled(true, APP_ID, "icygui", exe).unwrap();
    assert!(autostart::is_enabled(APP_ID));
    assert!(!Path::new("relative").exists());
}

// ---------------------------------------------------------------------------
// Autostart entries as GLib (GNOME, Cinnamon, MATE) starts them

/// Directory names that need quoting or escaping in `Exec`.
const TRICKY_DIRECTORIES: [&str; 16] = [
    "My Apps",
    "it's here",
    "\"quoted\"",
    "back`tick`",
    "$HOME",
    "back\\slash",
    "trailing\\",
    "back\\\nslash-newline",
    "~user",
    "a;b|c&d>e<f*g?h#i(j)k",
    "tab\there",
    "new\nline",
    "carriage\rreturn",
    "ünïcødé 日本",
    "  double  spaces  ",
    "]]>",
];

#[test]
fn autostart_entries_start_under_glib() {
    if !is_child() {
        let temp = tempfile::tempdir().unwrap();
        run_child(
            "autostart_entries_start_under_glib",
            Bus::Private,
            &[
                ("XDG_CONFIG_HOME", temp.path().join("config").into()),
                ("IC_PLATFORM_TEST_ROOT", temp.path().join("apps").into()),
            ],
        );
        return;
    }
    match Command::new("gio").arg("help").output() {
        Ok(output) if output.status.success() => {}
        _ => {
            println!("skipped: gio (GLib) is not installed");
            return;
        }
    }
    let root = PathBuf::from(env::var_os("IC_PLATFORM_TEST_ROOT").unwrap());
    let config = PathBuf::from(env::var_os("XDG_CONFIG_HOME").unwrap());
    let entry = config.join("autostart").join(format!("{APP_ID}.desktop"));

    for (index, directory) in TRICKY_DIRECTORIES.iter().enumerate() {
        let dir = root.join(directory);
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("icygui");
        fs::write(
            &exe,
            "#!/bin/sh\nprintf '%s\\0' \"$0\" \"$@\" > \"$IC_PLATFORM_ARGV\"\n",
        )
        .unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        let argv_file = root.join(format!("argv-{index}"));

        autostart::set_enabled(true, APP_ID, "icygui", &exe).unwrap();
        let output = Command::new("gio")
            .arg("launch")
            .arg(&entry)
            .env("IC_PLATFORM_ARGV", &argv_file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{directory:?}: gio launch failed: {}\n{}",
            String::from_utf8_lossy(&output.stderr),
            fs::read_to_string(&entry).unwrap()
        );
        wait_until(&format!("{directory:?} to start"), || argv_file.exists());
        let mut argv = String::new();
        wait_until(&format!("{directory:?} to write its arguments"), || {
            argv = fs::read_to_string(&argv_file).unwrap_or_default();
            argv.ends_with("--background\0")
        });
        let argv: Vec<&str> = argv.trim_end_matches('\0').split('\0').collect();
        assert_eq!(
            argv,
            [exe.to_str().unwrap(), "--background"],
            "{directory:?}"
        );
    }

    // A '%' can't be written so that GLib starts it: refused up front.
    let dir = root.join("100% sure");
    let error = autostart::set_enabled(true, APP_ID, "icygui", &dir.join("icygui")).unwrap_err();
    assert!(
        matches!(error, PlatformError::InvalidArgument { .. }),
        "{error}"
    );
}
