//! The app in the background on Linux (BG-01..04, NOTE-01, REL-07), run as
//! a process the way a desktop sees it: on a private D-Bus session with a
//! fake tray host (`StatusNotifierWatcher`) and a fake notification
//! server, without a display (GPUI runs headless).
//!
//! - `icygui --demo --background` starts in the tray without a window,
//!   with every environment's engine running (PLAN.md D2); the tray item's
//!   tooltip names each environment, its connection and its counts, and
//!   the icon's core takes the worst unhandled state's colour; a problem
//!   storm reaches the notification server with the environment's name in
//!   front of the title, the critical urgency, a sound or silence, the
//!   desktop entry and *Acknowledge* and *Open*; *Open* on a notification
//!   opens the window; the tray's *Quit* quits.
//! - Two launches of the real app: the second hands over to the first,
//!   whose window comes forward, and exits.
//! - Without a notification server, notifications cost one warning and no
//!   thread or connection each.
//!
//! Each test runs its body in a child process of its own under
//! `dbus-run-session` with a bus that has no activatable services, so
//! nothing reaches the user's session. Without `dbus-run-session` the
//! tests are skipped with a note.

#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "test harness and helpers: a failure should panic the test"
)]

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::blocking::fdo::DBusProxy;
use zbus::zvariant::{OwnedValue, Value};

/// Set in the child process, which runs the test's body.
const CHILD: &str = "ICYGUI_BACKGROUND_TEST_CHILD";
/// The tray icon's "no state" grey.
const GREY: [u8; 3] = [0x7d, 0x84, 0x8a];

fn is_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

/// Runs the test named `test` again in a child process on a private bus.
fn run_child(test: &str) {
    let exe = std::env::current_exe().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("session.conf");
    fs::write(&config, bus_config(temp.path())).unwrap();
    let output = Command::new("dbus-run-session")
        .arg(format!("--config-file={}", config.display()))
        .arg("--")
        .arg(&exe)
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output();
    let output = match output {
        Ok(output) => output,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
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
}

/// A session bus configuration with no service directories.
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

/// Polls `condition` until it holds; fails after `seconds`.
fn wait_until(what: &str, seconds: u64, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// The desktop: a tray host and a notification server

/// A `StatusNotifierWatcher` with a host.
struct Watcher {
    items: Mutex<Vec<String>>,
}

#[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
impl Watcher {
    fn register_status_notifier_item(&self, service: &str) {
        self.items.lock().unwrap().push(service.to_owned());
    }

    #[zbus(property)]
    fn is_status_notifier_host_registered(&self) -> bool {
        // A host, whether or not an item registered yet.
        self.items.lock().is_ok()
    }
}

/// A notification as the server received it.
#[derive(Debug)]
struct Received {
    id: u32,
    app_name: String,
    icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedValue>,
}

impl Received {
    fn hint_string(&self, key: &str) -> Option<String> {
        self.hints
            .get(key)
            .map(|value| String::try_from(value.try_clone().unwrap()).unwrap())
    }

    fn urgency(&self) -> u8 {
        u8::try_from(self.hints.get("urgency").unwrap().try_clone().unwrap()).unwrap()
    }
}

/// The notification server.
#[derive(Default)]
struct Notifications {
    received: Arc<Mutex<Vec<Received>>>,
    closed: Mutex<Vec<u32>>,
    next: AtomicU32,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Notifications {
    #[expect(clippy::too_many_arguments, reason = "the D-Bus method's signature")]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        assert_eq!(replaces_id, 0, "every notification is new");
        assert_eq!(expire_timeout, -1, "the server decides");
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        self.received.lock().unwrap().push(Received {
            id,
            app_name,
            icon: app_icon,
            summary,
            body,
            actions,
            hints,
        });
        id
    }

    fn close_notification(&self, id: u32) {
        self.closed.lock().unwrap().push(id);
    }

    fn get_capabilities(&self) -> Vec<String> {
        let mut capabilities = vec!["actions".to_owned(), "body".to_owned()];
        if self.next.load(Ordering::SeqCst) < u32::MAX {
            capabilities.push("sound".to_owned());
        }
        capabilities
    }
}

/// Serves a tray host.
fn serve_tray() -> Connection {
    zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.kde.StatusNotifierWatcher")
        .unwrap()
        .serve_at(
            "/StatusNotifierWatcher",
            Watcher {
                items: Mutex::new(Vec::new()),
            },
        )
        .unwrap()
        .build()
        .unwrap()
}

/// Serves the tray host and the notification server; returns the
/// notifications received and the server's connection (to emit clicks).
fn serve_desktop() -> (Connection, Connection, Arc<Mutex<Vec<Received>>>) {
    let watcher = serve_tray();
    let server = Notifications::default();
    let received = server.received.clone();
    let notifications = zbus::blocking::connection::Builder::session()
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .serve_at("/org/freedesktop/Notifications", server)
        .unwrap()
        .build()
        .unwrap();
    (watcher, notifications, received)
}

/// Clicks a notification's button (`default`: its body).
fn invoke(server: &Connection, id: u32, action: &str) {
    server
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "ActionInvoked",
            &(id, action),
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
// The tray as a host sees it

/// The tray item of process `pid`, once it is on the bus.
fn tray_item(bus: &Connection, pid: u32) -> Option<String> {
    let prefix = format!("org.kde.StatusNotifierItem-{pid}-");
    DBusProxy::new(bus)
        .unwrap()
        .list_names()
        .unwrap()
        .into_iter()
        .map(|name| name.to_string())
        .find(|name| name.starts_with(&prefix))
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

/// Icons on the wire: width, height, ARGB pixels.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

/// The tooltip's text.
fn tooltip(bus: &Connection, item: &str) -> String {
    let (_icon_name, _icons, _title, text): (String, Pixmaps, String, String) =
        item_property(bus, item, "ToolTip").try_into().unwrap();
    text
}

/// The colour in the middle of the icon (the core of the mark).
fn icon_core(bus: &Connection, item: &str) -> [u8; 3] {
    let icons: Pixmaps = item_property(bus, item, "IconPixmap").try_into().unwrap();
    let (width, height, argb) = &icons[0];
    let (width, height) = (
        usize::try_from(*width).unwrap(),
        usize::try_from(*height).unwrap(),
    );
    let index = (height / 2 * width + width / 2) * 4;
    [argb[index + 1], argb[index + 2], argb[index + 3]]
}

/// A dbusmenu item on the wire: id, properties, children.
type RawMenuNode = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

/// The ids and labels of the menu's items, depth first.
fn menu_items(bus: &Connection, item: &str) -> Vec<(i32, String)> {
    fn walk(node: RawMenuNode, out: &mut Vec<(i32, String)>) {
        let (id, properties, children) = node;
        if let Some(label) = properties.get("label") {
            let label = String::try_from(label.try_clone().unwrap()).unwrap();
            out.push((id, label.replace('_', "")));
        }
        for child in children {
            walk(child.try_into().unwrap(), out);
        }
    }
    let reply = bus
        .call_method(
            Some(item),
            "/MenuBar",
            Some("com.canonical.dbusmenu"),
            "GetLayout",
            &(0_i32, -1_i32, Vec::<String>::new()),
        )
        .unwrap();
    let (_revision, root): (u32, RawMenuNode) = reply.body().deserialize().unwrap();
    let mut items = Vec::new();
    walk(root, &mut items);
    items
}

/// Clicks the menu item whose label ends with `label` (environments carry
/// a mark or an indent in front), as a host does.
fn click(bus: &Connection, item: &str, label: &str) {
    let items = menu_items(bus, item);
    let (id, _) = items
        .iter()
        .find(|(_, text)| text.ends_with(label))
        .unwrap_or_else(|| panic!("no menu item {label:?} in {items:?}"));
    bus.call_method(
        Some(item),
        "/MenuBar",
        Some("com.canonical.dbusmenu"),
        "Event",
        &(*id, "clicked", Value::from(0_i32), 0_u32),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// The app

/// The app, started with `args` and its files in `home`, without a
/// display; its log goes to `<home>/run.log`.
fn launch(home: &Path, args: &[&str], extra: &[(&str, &str)]) -> Child {
    let log = fs::File::create(home.join(format!("run-{}.log", args.join("_")))).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_icygui"));
    command
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("RUST_LOG", "info,icygui=debug")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    for (key, value) in extra {
        command.env(key, value);
    }
    fs::create_dir_all(home.join("run")).unwrap();
    command.spawn().unwrap()
}

fn log_of(home: &Path, args: &[&str]) -> String {
    fs::read_to_string(home.join(format!("run-{}.log", args.join("_")))).unwrap_or_default()
}

/// Waits for `child` to exit; kills it after `seconds`.
fn wait_exit(child: &mut Child, seconds: u64) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Kills the app if a check failed before it quit.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn home() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// A critical problem's notification: our app name, icon and desktop
/// entry, critical urgency, a sound or silence, and its buttons.
fn check_problem_notification(notification: &Received) {
    assert_eq!(notification.app_name, "icygui");
    assert_eq!(notification.icon, "io.github.alexykn.icygui");
    assert_eq!(notification.urgency(), 2, "critical stays on screen");
    assert_eq!(
        notification.hint_string("desktop-entry").as_deref(),
        Some("io.github.alexykn.icygui")
    );
    assert!(
        notification.hint_string("sound-name").is_some()
            || notification.hints.contains_key("suppress-sound"),
        "{:?}",
        notification.hints
    );
    assert_eq!(
        notification.actions,
        ["default", "", "acknowledge", "Acknowledge", "open", "Open"],
        "Open once: the body's own action has no label of its own"
    );
    assert!(!notification.body.is_empty());
}

#[test]
fn the_demo_runs_in_the_tray_and_notifies_the_desktop() {
    if !is_child() {
        run_child("the_demo_runs_in_the_tray_and_notifies_the_desktop");
        return;
    }
    let bus = Connection::session().unwrap();
    let (_watcher, server, received) = serve_desktop();
    let (_dir, home) = home();
    let args = ["--demo", "--background"];
    let mut app = Running(launch(
        &home,
        &args,
        &[("ICYGUI_DEMO_SEED", "3"), ("ICYGUI_DEMO_STORM", "6")],
    ));
    let pid = app.0.id();

    // In the tray, without a window.
    let mut item = None;
    wait_until("the tray item", 30, || {
        item = tray_item(&bus, pid);
        item.is_some()
    });
    let item = item.unwrap();
    wait_until("the demo to load", 60, || {
        tooltip(&bus, &item).starts_with("prod-cluster (demo) · connected")
    });
    // Every environment connects in the background too.
    wait_until("every environment", 60, || {
        let text = tooltip(&bus, &item);
        text.contains("\nstaging (demo) · connected") && text.contains("\nlab (demo) · connected")
    });
    let text = tooltip(&bus, &item);
    assert!(text.contains("unhandled"), "{text}");
    assert_ne!(icon_core(&bus, &item), GREY, "tinted with the worst state");
    let labels: Vec<String> = menu_items(&bus, &item)
        .into_iter()
        .map(|(_, label)| label)
        .collect();
    assert!(labels.contains(&"Open icygui".to_owned()), "{labels:?}");
    assert!(
        labels.contains(&"prod-cluster".to_owned())
            || labels.iter().any(|label| label.ends_with("prod-cluster")),
        "{labels:?}"
    );
    let log = log_of(&home, &args);
    assert!(
        log.contains("starting in the background, in the tray"),
        "{log}"
    );
    assert!(!log.contains("main window opened"), "no window yet");

    // A storm: the first problems reach the desktop, critical, with
    // Acknowledge and Open, and the environment's name in front.
    let prod_critical = "prod-cluster · CRITICAL · ";
    wait_until("a notification", 90, || {
        received
            .lock()
            .unwrap()
            .iter()
            .any(|notification| notification.summary.starts_with(prod_critical))
    });
    let (id, problem) = {
        let received = received.lock().unwrap();
        let notification = received
            .iter()
            .find(|notification| notification.summary.starts_with(prod_critical))
            .unwrap();
        check_problem_notification(notification);
        (notification.id, notification.summary.clone())
    };

    // Open on the notification: the window opens for its object.
    invoke(&server, id, "open");
    wait_until("the window", 20, || {
        let log = log_of(&home, &args);
        log.contains("notification clicked") && log.contains("main window opened")
    });

    // Pause from the tray: the tooltip says until when; then resume.
    click(&bus, &item, "For 30 minutes");
    wait_until("the pause", 10, || {
        tooltip(&bus, &item).contains("notifications paused until")
    });
    click(&bus, &item, "Resume notifications");
    wait_until("the resume", 10, || {
        !tooltip(&bus, &item).contains("paused")
    });

    // Switch environments from the tray: at once, its engine runs.
    click(&bus, &item, "staging");
    wait_until("staging", 20, || {
        let log = log_of(&home, &args);
        log.contains("switching environment") && log.contains("\"staging\"")
    });

    // Quit from the tray.
    click(&bus, &item, "Quit icygui");
    let status = wait_exit(&mut app.0, 20).unwrap_or_else(|| {
        panic!("didn't quit:\n{}", log_of(&home, &args));
    });
    assert!(status.success(), "{status}");
    eprintln!("notified about {problem}");
}

/// The threads of process `pid` named `name` (as the kernel shortens it).
fn threads_named(pid: u32, name: &str) -> usize {
    fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|task| {
            fs::read_to_string(task.path().join("comm")).is_ok_and(|comm| comm.trim() == name)
        })
        .count()
}

/// Without a notification server (a tiling WM without a notification
/// daemon), notifications stay in the centre: one warning, then a pause,
/// and no thread or bus connection per notification, for an app that runs
/// in the tray for weeks.
#[test]
fn a_missing_notification_server_costs_nothing_per_notification() {
    if !is_child() {
        run_child("a_missing_notification_server_costs_nothing_per_notification");
        return;
    }
    let bus = Connection::session().unwrap();
    let _tray = serve_tray();
    let (_dir, home) = home();
    let args = ["--demo", "--background"];
    let mut app = Running(launch(
        &home,
        &args,
        &[("ICYGUI_DEMO_SEED", "3"), ("ICYGUI_DEMO_STORM", "4")],
    ));
    let pid = app.0.id();
    wait_until("the tray item", 30, || tray_item(&bus, pid).is_some());
    wait_until("the missing server noticed", 90, || {
        log_of(&home, &args).contains("no notification server on the session bus")
    });
    // Several storms more, each with notifications to post.
    thread::sleep(Duration::from_secs(14));
    let log = log_of(&home, &args);
    assert_eq!(
        log.matches("no notification server on the session bus")
            .count(),
        1,
        "warned once:\n{log}"
    );
    assert!(
        !log.contains("refused a notification"),
        "no Notify without a server:\n{log}"
    );
    assert!(
        threads_named(pid, "icygui-notify-c") <= 1,
        "one listener at most, not one per notification"
    );
    let item = tray_item(&bus, pid).unwrap();
    click(&bus, &item, "Quit icygui");
    let status = wait_exit(&mut app.0, 20).expect("quits");
    assert!(status.success());
}

#[test]
fn a_second_launch_brings_the_first_forward() {
    if !is_child() {
        run_child("a_second_launch_brings_the_first_forward");
        return;
    }
    let bus = Connection::session().unwrap();
    let (_watcher, _server, _received) = serve_desktop();
    let (_dir, home) = home();
    // The first starts at login, in the tray (no environment yet).
    let first_args = ["--background"];
    let mut first = Running(launch(&home, &first_args, &[]));
    let pid = first.0.id();
    wait_until("the first's tray item", 30, || {
        tray_item(&bus, pid).is_some()
    });
    assert!(!log_of(&home, &first_args).contains("main window opened"));

    // The second hands over and exits; the first opens its window.
    let mut second = launch(&home, &[], &[]);
    let status = wait_exit(&mut second, 20).expect("the second launch exits");
    assert!(status.success(), "{status}: {}", log_of(&home, &[]));
    assert!(
        log_of(&home, &[]).contains("icygui is already running"),
        "{}",
        log_of(&home, &[])
    );
    wait_until("the first's window", 20, || {
        log_of(&home, &first_args).contains("main window opened")
    });

    // Quit from the tray.
    let item = tray_item(&bus, pid).unwrap();
    click(&bus, &item, "Quit icygui");
    let status = wait_exit(&mut first.0, 20).expect("the first quits");
    assert!(status.success());
}
