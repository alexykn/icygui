# Requirements checklist

Every requirement has a stable ID. The final production-readiness audit checks each one against code and tests and records evidence. Sources: PLAN.md (decisions D1–D9, sections 1–3), the design (`design/project/*.html`, `design/chats/chat1.md`), docs/architecture.md, and the user's instructions in the planning conversation.

## Connection and environments

- **ENV-01** Several environments can be configured; exactly one is active. Switching happens from the footer status (environment switcher), never from the sidebar (D1, D2).
- **ENV-02** Add/edit/delete environment: name, URL (https only), auth (basic username + password, or client certificate and key files), TLS (CA file, pinned SHA-256, server-name override, system roots), author name.
- **ENV-03** Passwords live in the OS keychain (macOS Keychain, Secret Service); never in config files or logs. Deleting an environment deletes its secret and its event log.
- **ENV-04** "Test connection" shows user, version, permissions and missing permissions.
- **ENV-05** On a TLS failure (unknown CA or name mismatch), the certificate's fingerprint, subject, issuer and expiry are shown, with "Trust this certificate" (sets the pin). A pin mismatch shows both fingerprints.
- **ENV-06** Connection status in the footer: endpoint name plus age of the last event (`master-01 · 2s`), coloured by health (connected / stale > 30 s / reconnecting / failed). Clicking it opens the switcher with details.
- **ENV-07** Automatic reconnect with backoff; banner with retry countdown and "Retry now". Auth failure and TLS failure stop retrying and show actionable banners.
- **ENV-08** First run without environments shows an onboarding form.
- **ENV-09** Works with a least-privilege API user without `filter-expression` (Icinga 2.17 default). Buttons for actions the user may not run are disabled with an explanation.

## Live data

- **LIVE-01** Initial load, then the event stream; changes appear within about a second without polling.
- **LIVE-02** Periodic reconcile (configurable interval) and reconcile after reconnect; missed changes are detected.
- **LIVE-03** Objects created or deleted in Icinga appear or disappear.
- **LIVE-04** 20 000 services stay smooth: virtualised lists, nothing per frame proportional to the object count.

## Sidebar and dashboards

- **DASH-01** The sidebar matches the design: search, groups (folders) with active highlight, chevron, + and ···, dashboard rows with state dot (worst unhandled), label and unhandled count, footer icons (sidebar toggle, notification centre, environment status, +).
- **DASH-02** Groups: create, rename, collapse, reorder, delete (with confirmation), per-group notification setting.
- **DASH-03** Dashboards: create (+ on a group), edit, rename, duplicate, move to another group, reorder, delete, per-dashboard notification setting.
- **DASH-04** The dashboard editor has name, hosts/services, filter in Icinga's language with live validation (error position) and live match count, problems only, hide handled, sort, group by.
- **DASH-05** Default dashboards for a new environment ("overview": problems, host problems, all services).
- **DASH-06** Export and import dashboard groups (files), with fresh ids on import.
- **DASH-07** Sort menu (severity, last state change, host, service; direction) and the "handled hidden" toggle are persisted per dashboard.
- **DASH-08** Group-by (host, host group, service group) with headers.

## Lists and panes

- **LIST-01** Rows as in the design (2a): 22 px state circle (filled = unhandled, hollow = handled), time in state, `service on host`, output (ellipsis), right tag (ack author, downtime, flapping).
- **LIST-02** The summary bar shows counts per state with dots, and the handled toggle.
- **LIST-03** Keyboard: j/k and arrows move, Enter opens, Esc closes, x multi-select, shift-click range; selection survives updates.
- **PANE-01** Service pane as in 2b: state, name, host link, duration, hard/soft attempt; action buttons with key hints (a, d, r, c); plugin output and long output; perfdata table with threshold colouring; check details; comments; downtimes; vars; groups; notes and links.
- **PANE-02** Host pane as in 2c: state, address, uptime/output; actions; tabs: services (OK collapsed into "+ N more ok"), history, vars, config (read-only check config and feature flags); parents and children from dependencies.
- **PANE-03** "↗ open as tab" pins an object in the sidebar's "open" section; pinned tabs persist.
- **PANE-04** History tab from the local event log ("recorded locally since …").
- **PANE-05** Copy name, output and filter expression; open notes/action URLs in the browser.

## Actions (runtime operations only; D6)

- **ACT-01** Check now (force), on one object or a selection.
- **ACT-02** Acknowledge (comment, sticky, persistent, expiry; never asks Icinga to notify) and remove acknowledgement.
- **ACT-03** Schedule downtime (start/end with presets, fixed/flexible + duration, all services, child options, trigger) and remove downtimes (single, or all for the selection).
- **ACT-04** Add comment and remove comment.
- **ACT-05** Submit a passive check result (state, output, perfdata).
- **ACT-06** Run command (check/event command, endpoint defaulting sensibly, macros, TTL) behind a confirmation.
- **ACT-07** Bulk actions on multi-selection. Results show as toasts with per-object failures; changed objects refresh within about a second.
- **ACT-08** The client never calls `objects/modify`, config packages, object creation or deletion, the console or process restart. Icinga's feature flags are shown read-only.

## Notifications (D4, D5)

- **NOTE-01** Native OS notifications (macOS UNUserNotificationCenter from the app bundle; Linux XDG via D-Bus) with Acknowledge and Open buttons; clicking opens the object's pane (window recreated if closed).
- **NOTE-02** Rules per environment (default), group, dashboard ("thread") and object (watch / mute with expiry), with inheritance as specified in docs/architecture.md (ic-rules).
- **NOTE-03** Conditions: states, hard only, skip handled, acknowledgement/downtime/flapping events, minimum duration, sound.
- **NOTE-04** Recoveries only for notified problems; dedupe across dashboards; storm control with summary; quiet hours (midnight-crossing, days, allow critical); pause (30 m, 1 h, until tomorrow, resume).
- **NOTE-05** Notification centre (footer clock icon): recent notifications including silent ones, unread badge, mark all read, open the object.
- **NOTE-06** Notification settings UI for every rule field, quiet hours, storm control, and the watched/muted list.
- **NOTE-07** No notifications for the initial load; missed changes found by reconcile do notify.

## Background and platform

- **BG-01** Closing the window keeps the app running in the tray / menu bar (configurable). Quit from the tray or the menu quits.
- **BG-02** The tray icon shows the worst unhandled state of the active environment; the tooltip has counts; the menu has Open, Pause notifications, Environments, Quit.
- **BG-03** Optional launch at login (LaunchAgent / XDG autostart) starts in background mode (`--background`).
- **BG-04** Single instance: a second launch brings the running instance's window forward.
- **BG-05** macOS app menu (About, Settings ⌘,, Quit), Edit menu for text inputs, keyboard shortcuts with cmd on macOS and ctrl on Linux.
- **BG-06** Linux client-side window decorations matching the design; window icon; window size and position remembered.

## Look and feel

- **UI-01** Dark theme exactly per the design tokens (PLAN.md §1); IBM Plex Mono bundled.
- **UI-02** Light theme and "follow system" option.
- **UI-03** ⌘K / ctrl-K command palette: dashboards, hosts, services, actions, environment switch, settings, pause.
- **UI-04** Relative times refresh (footer every second, rows periodically).
- **UI-05** Empty, loading and error states for every view (no environment, connecting, no permission, filter error, empty dashboard).

## Quality and operations

- **OPS-01** `cargo fmt`, `clippy::pedantic` with `-D warnings`, tests and `cargo-deny` pass in CI on Linux and macOS.
- **OPS-02** Contract tests against a real Icinga 2 (Docker) run in CI.
- **OPS-03** Mock environments (`ic-mock`, `cargo xtask mock`) for development and tests (D9).
- **OPS-04** Packaging: macOS `.app` (+ `.dmg`); Linux `.deb`, `.tar.gz`, `.desktop` entry and icons; `cargo xtask install`.
- **OPS-05** Logs go to a rotating file in the log directory; secrets are never logged.
- **OPS-06** The config file is written atomically with a backup; corrupt config is reported with restore options; the file is private (0600).
- **OPS-07** Documentation: README (features, install, ApiUser permissions, TLS setup), user guide, development guide, architecture.

## Brand and distribution

- **REL-01** Logo: inspired by Zed and Delta, built on Icinga's orange warning circle with the design's blue accent; a single SVG source renders the app icon (all sizes, `.icns`), the mark and the README banner.
- **REL-02** macOS releases are universal binaries with the hardened runtime. Without a Developer ID (current state) they're ad-hoc signed; with the Developer ID secrets set, they're signed, notarized and stapled (app and `.dmg`) with no code changes.
- **REL-03** Optional Homebrew tap: when a tap token is configured, the release workflow updates `Casks/icygui.rb` (macOS; clears quarantine while builds aren't notarized) and `Formula/icygui.rb` (Linux); `uninstall quit` and `zap` are correct.
- **REL-08** `install.sh` (`curl … | bash`):
  - macOS and Linux, x86_64 and arm64; latest or pinned version;
  - SHA-256 verification against `SHA256SUMS`;
  - macOS: install into `/Applications` (or `~/Applications`), quit a running instance first, re-sign with a per-machine self-signed identity (falling back to ad-hoc), needs no Xcode or Command Line Tools;
  - Linux: user-local install with desktop entry and icons, plus hints for missing libraries;
  - `--uninstall` and `--purge`;
  - never runs a partially downloaded script.
- **REL-04** Linux releases: `.deb` and `.tar.gz` for x86_64 and aarch64 with desktop entry and icons; `SHA256SUMS` (optionally GPG-signed) and build-provenance attestations for every artifact.
- **REL-05** One-tag releases: the tag must match the workspace version; the release can be rebuilt by hand for an existing tag.
- **REL-06** `icygui --version` and `--help` work without opening a window (the Homebrew formula test relies on it).
- **REL-07** The tray icon uses the logo's mark, tinted with the worst unhandled state.
