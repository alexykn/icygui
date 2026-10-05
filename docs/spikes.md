# M0 spikes: platform risks

Run on 2026-10-05 against `gpui-pre` 0.3.8 (snapshot of zed@279fe07) and `tray-icon` 0.26.

| Risk (PLAN.md §6) | Result | Verified where |
|---|---|---|
| GPUI quits the app when the last window closes (Linux) | **Resolved.** `Application::with_quit_mode(QuitMode::Explicit)` keeps the process and event loop alive with no window open; a window can be opened again afterwards. GPUI's default (`QuitMode::Default`) does quit on Linux, so the app must set `Explicit`. | Linux (X11 under Xvfb), scripted |
| Tray on Linux needs a GTK loop | **Resolved.** `tray-icon` with only the `ksni` feature talks StatusNotifierItem/dbusmenu over D-Bus, with no GTK in the dependency tree. A dbusmenu click on "Open" reached GPUI's main thread and reopened the window. | Linux, D-Bus session, scripted |
| Native notifications | **Simpler than planned.** GPUI has `cx.show_system_notification` with action buttons and a response callback: XDG notifications via `notify-rust` on Linux, `UNUserNotificationCenter` on macOS. A notification posted with no window open showed its "Acknowledge" / "Open" buttons in dunst, and choosing "Acknowledge" came back to the app as `action = "ack"`. | Linux with dunst, scripted |
| macOS notifications need a signed bundle | **Confirmed by GPUI's own code:** without a bundle identifier GPUI logs "system notifications disabled: not running from an app bundle" and posts nothing. A dev `.app` bundle is needed to test them. | Code reading; needs a Mac run |
| macOS menu-bar icon next to GPUI | **Open.** Same spike binary; `tray-icon` creates an `NSStatusItem` on the main thread inside GPUI's `NSApplication`. | Needs a Mac run |

## Observations

- GPUI's Linux backend gives the notification body-click action the visible label `default`. GNOME hides it; dunst lists it in its context menu.
- GPUI cannot dismiss or replace notifications on Linux (`dismiss` is a no-op) and sets no urgency. That's enough for v1. If critical alerts need `urgency=critical` (GNOME keeps those on screen), the Linux notifier adapter can call `notify-rust` directly.
- Without a StatusNotifierWatcher (GNOME without the AppIndicator extension), `tray-icon` still starts (`assume_sni_available`); the icon just isn't shown. The app keeps running and notifying.
- The Linux build renders through `wgpu`. Under Xvfb it picked llvmpipe (software Vulkan), which is how CI runs the spike.
- On macOS, GPUI compiles its Metal shaders with `xcrun metal` at build time. Xcode 26 ships the Metal toolchain separately (`xcodebuild -downloadComponent MetalToolchain`).

## How to run

```sh
cargo build -p spikes --bin background

# Linux, headless: Xvfb + private D-Bus + dunst, drives tray and notification from outside
spikes/linux-headless.sh

# Any desktop, interactive: close the window, then use the tray / menu-bar icon
cargo run -p spikes --bin background
```

Scripted result on Linux:

```
PASS  tray icon created
PASS  window closed, no windows left
PASS  app alive after its last window closed
PASS  notification posted with no window open
PASS  notification action reached the app
PASS  tray menu click reached the app
PASS  window reopened from the tray
PASS  app quit on request
PASS  dunst showed the Acknowledge action
NOTE  default quit mode quit after the last window closed
```

### Still to run on a Mac

1. `cargo run -p spikes --bin background`: does the menu-bar icon appear, and do Open / Quit work after closing the window?
2. The same binary inside a dev `.app` bundle (comes with `cargo xtask bundle`): does the permission prompt appear, and do the notification's Acknowledge / Open buttons come back to the app?
