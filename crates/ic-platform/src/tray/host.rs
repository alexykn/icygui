//! Whether the desktop shows tray icons ([`super::host_available`]).
//!
//! On Linux and the BSDs the icon is a `StatusNotifierItem`: it is shown
//! only while a `StatusNotifierWatcher` runs on the session bus and a host
//! (the panel) has registered with it. KDE Plasma, Xfce, Cinnamon, MATE,
//! `LXQt`, waybar and others provide one; stock GNOME doesn't (it needs the
//! `AppIndicator` extension). `tray-icon` creates the item whether or not
//! anybody will show it, so the app has to ask.

/// Whether a tray host is running.
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
pub(super) fn available() -> bool {
    status_notifier::available()
}

/// The menu bar always shows status items.
#[cfg(not(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
)))]
pub(super) fn available() -> bool {
    true
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
mod status_notifier {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use zbus::blocking::fdo::DBusProxy;
    use zbus::blocking::{Connection, proxy};
    use zbus::names::BusName;
    use zbus::proxy::CacheProperties;

    /// The watcher's well-known name, object path and interface.
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    const WATCHER_PATH: &str = "/StatusNotifierWatcher";
    /// How long to wait for the session bus to answer.
    const TIMEOUT: Duration = Duration::from_secs(2);

    /// Asks the session bus on a helper thread, so a bus that doesn't
    /// answer can't block the caller for longer than [`TIMEOUT`].
    pub(super) fn available() -> bool {
        let (sender, receiver) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("icygui-tray-host".to_owned())
            .spawn(move || {
                // The receiver is gone only after a timeout; nothing to do.
                let _ = sender.send(query());
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "cannot check for a tray host");
            return false;
        }
        match receiver.recv_timeout(TIMEOUT) {
            Ok(Ok(available)) => {
                tracing::debug!(available, "tray host");
                available
            }
            Ok(Err(error)) => {
                tracing::info!(%error, "no tray host: cannot ask the session bus");
                false
            }
            Err(_) => {
                tracing::warn!("no tray host: the session bus doesn't answer");
                false
            }
        }
    }

    /// A watcher owns its name and says a host is registered. Watchers that
    /// can't answer the property are given the benefit of the doubt:
    /// KDE's and GNOME's extension always report `true` anyway.
    fn query() -> zbus::Result<bool> {
        let connection = Connection::session()?;
        let watcher = BusName::try_from(WATCHER)?;
        if !DBusProxy::new(&connection)?.name_has_owner(watcher)? {
            return Ok(false);
        }
        let proxy = proxy::Builder::<zbus::blocking::Proxy<'_>>::new(&connection)
            .destination(WATCHER)?
            .path(WATCHER_PATH)?
            .interface(WATCHER)?
            // No GetAll and no signal subscription for a single read.
            .cache_properties(CacheProperties::No)
            .build()?;
        match proxy.get_property::<bool>("IsStatusNotifierHostRegistered") {
            Ok(registered) => Ok(registered),
            Err(error) => {
                tracing::debug!(%error, "the tray watcher doesn't say whether a host is registered");
                Ok(true)
            }
        }
    }
}
