# Icinga 2 desktop client — Rust + GPUI implementation plan

Status: **plan v3.** M0 is done (spike results in `docs/spikes.md`); M1–M5 are built and reviewed: every crate, the `ic-core` runtime and the whole app (waves 1–4). Wave 5 makes it a releasable rc1 (M6: packaging, documentation, a review and fix pass); see §4 for rc1 and v1.
Inputs: `design/project/Icinga Client v2.dc.html` (primary design, screens 2a–2c), `design/project/Icinga Client.dc.html` (turn 1: command palette, dashboard editor, detail sub-tabs), `design/chats/chat1.md`, the sidebar screenshot, and the Icinga 2 REST API reference (`doc/12-icinga2-api.md`, Icinga/icinga2 master).

## 0. Decisions

| # | Question | Decision |
|---|---|---|
| D1 | What is a sidebar group? | A **folder of dashboards inside one environment**. The sidebar never switches servers. |
| D2 | Switching Icinga servers | Separate **environment switcher**: click `● prod master-01 2s ⌄` in the sidebar footer (or ⌘K → "Switch environment"); it lists every environment with its health, unread notifications and mute. **Every saved environment runs its own engine in the background** (event stream, rules, event log, notifications); the active one is the one the window shows, so switching is instant. Each Icinga cluster sees one stream and one lean load per environment, as before; engines off screen cost it no more than the one on screen. Every environment has its own groups and dashboards. *(Changed before rc1 was merged: it was "one environment is active at a time" and only that one was connected.)* |
| D3 | History | **Live monitoring first.** No Icinga DB integration. A small local event log (default 48h) feeds the notification centre and a "recent events" view. |
| D4 | Background | The app **keeps running in the menu bar / system tray** after the window closes and keeps notifying. Optional launch at login. |
| D5 | Notification source | **Client-side rules** on the live event stream. They don't depend on Icinga notification users or contacts. |
| D6 | Editing | **No config editing.** Config is managed with Ansible. No config packages, no creating or deleting objects, no debug console, no process restart. **The client never changes object attributes** (no `objects/modify`, no `enable_*` switches, per object or global). Only runtime operations through `/v1/actions` are in scope: ack, downtime, check now, comments, passive results. |
| D7 | Repo | Cargo workspace **in this repo**, next to `project/` and `chats/`. |
| D8 | Code quality | `clippy::pedantic` (plus a curated set of restriction lints) is denied in CI, with a structured, layered workspace (§3). |
| D9 | Testing without a real Icinga | **Mock environments** (`ic-mock`, §3.6): a local Icinga 2 API look-alike with several environments, simulated state changes, actions and fault injection. Used by tests and as dev servers the app connects to. |

> Note on D6: muting, watching and pausing notifications are **local to the client** (§2.7). They never touch Icinga's own `enable_notifications`, so they can't drift from the Ansible-managed config. The client only *displays* Icinga's switches read-only (e.g. "active checks disabled" in the Check section), because a disabled check explains a stale result.

---

## 1. What the design asks for

| Area | Design (v2) | Notes for implementation |
|---|---|---|
| Window | 1440×900, sidebar 300px + main area, macOS-style traffic lights in the sidebar header | GPUI transparent titlebar on macOS, client-side decorations on Linux |
| Sidebar | Zed project/thread pattern: **groups** (folders) each with **dashboards** ("threads"). Active group row `#30353a` with `⌄  +  ···`. Active item `#272c30`. Each item: 8px state dot + label + problem count | Footer: sidebar toggle, notification-centre (clock) icon, environment switcher `● prod master-01 2s ⌄` (the design's `● master-01 · 2s` with the environment's name first and a chevron; click = switcher), `+` new dashboard |
| List | One line per problem, Icinga Web style: 22px state circle (filled = unhandled, hollow ring = ack/downtime) with time-in-state under it; `service on host`; plugin output (1 line, ellipsis); right-aligned tag (`ack m.keller`, `downtime`) | Header: dashboard name, view name, `severity ↓` sort, `···`. Summary bar: `12 critical · 29 warning · 24 unknown`, `handled hidden` toggle |
| Detail pane | Click a row → 620px second column (Icinga Web's 2-column layout). `↗ open as tab`, `×` | Service pane: big state circle, name, `on <host> · 14m · hard 3/3`, action buttons with keys (**a**ck, **d**owntime, **r** check now, **c**omment), plugin output, perfdata table (value/warn/crit), check key/values, comments |
| Host pane | State, address, uptime, actions; sub-tabs `services 23 · history · vars · config` | Services as compact rows, `+ 16 more ok` collapse. "config" = read-only view of object attributes |
| From turn 1 (keep) | ⌘K command palette; dashboard view editor (filter/columns); detail sub-tabs; host-group grid; event stream | Re-skinned to the calmer v2 look |

### Design tokens (from the v2 source)

- Font: **IBM Plex Mono** 400/500/600, bundled with the app (OFL licence). Sizes 10.5 / 11 / 11.5 / 12 / 12.5 / 13 / 13.5 / 18px.
- Surfaces: window `#1d2125`, detail pane `#1a1d21`, code block `#16191c`, button `#262a2e`, selected row `#2a3036`, active group `#30353a`, active item `#272c30`.
- Borders: `#33383c` (outer), `#2e3337` (splits), `#2a2f33` (header rules), `#262a2e` (row rules).
- Text: `#f2f3f4` / `#eceeef` (strong), `#d6d8da` (body), `#b5b9bc`, `#8b9094` (muted), `#6c7175` (faint).
- State: crit/down `#e06c6c`, warn `#e5b04a`, ok/up `#56b870`, unknown/unreachable `#a97fdb`, pending `#3a3f43`, accent/link `#74ade8` (primary button text `#10161d`, key hint `#2b4766`).
- Heights: header bars 40px, summary bar 36px, group row 36px, item row 30px, footer 38px, list row padding 10px 18px, action button 28px with 5px radius.

All of this lives in one `Theme` struct (`ic-ui-kit`). A light theme is a second `Theme` value, not a rewrite.

---

## 2. Feature spec

Everything below is backed by the Icinga 2 REST API (`https://<endpoint>:5665/v1/…`) unless marked **client-side**.

### 2.1 Environments
- An **environment** = one Icinga cluster: its API URLs in order of preference (one for a single master or a load balancer; one per node for an HA pair or a master with satellites; ENV-12), TLS (system roots or Icinga's CA for every URL; a pinned certificate per URL), auth by API user + password **or** client certificate. Secrets live in the OS keychain (macOS Keychain, Secret Service on Linux), never in config files.
- **Topology (ENV-12):** every connect finds the node behind the URL (`node_name` → endpoint → zone). A node of the top-level zone sees the whole cluster; one in a child zone only its zone (a *partial view*, used only while no master answers, always labelled); without permission to read the zones the view is *not verified*. The engine keeps trying the preferred URLs gently and switches back; what changed outside the zone during a fallback notifies once a master is back.
- **Every environment runs** (D2): each saved environment has its own connection runtime from the app's start (also in the background, `--background`) until it is deleted; a change of its URLs, login or TLS restarts it. One of them is **active**: the window, the footer and the palette show it, and switching only changes which one that is (instant, no reconnect, no reload). Engines off screen publish their snapshots less often (nobody looks at them) but notify as promptly. Dashboards, groups and notification rules are stored per environment.
- On connect: auth check `GET /v1`, the node and its zone (`/v1/status/IcingaApplication`, `/v1/objects/zones`), version and app state from `GET /v1/status`, permission probe (§5).
- Footer switcher `● prod master-01 2s ⌄` = environment, connected node + age of the last event-stream message, a button with a chevron. Yellow dot when stale (> 30s), red when disconnected (with retry countdown). Its popover lists every environment with health, unread notifications and mute (a bell on each row mutes that environment), keeps its width and fits any window.

### 2.2 Object browsing (Icinga Web parity)
- **Hosts, services, host groups, service groups**: `GET /v1/objects/…` with `attrs` and `joins`.
- **Comments, downtimes**: own list views with bulk removal.
- **Users / user groups / notifications** (read-only: who Icinga notifies), **dependencies** (parents/children in the host pane), **endpoints and zones** (cluster health), **commands** (read-only, Config tab).
- **Problem views**: unhandled service problems, host problems, all. `handled hidden` toggle (handled = acknowledged, in downtime, or host down/unreachable).
- **Sort**: severity (Icinga Web order), last state change, host, service. **Group by**: host, host group, service group.
- **Search**: sidebar search filters dashboards. ⌘K searches hosts, services, groups, dashboards and commands.

### 2.3 Per-object actions (host and service)
Each action works on one object, a multi-selection, or every match of a palette query. Objects are always addressed by name, never by `filter` (which needs the `filter-expression` permission from Icinga 2.17 on): hosts and services go in separate requests of up to 200 names, sent one after another (20 per request for downtimes with `all_services` or child options).

| Action | Endpoint | Options in the dialog |
|---|---|---|
| Check now | `actions/reschedule-check` | `force` (default on), optional `next_check` |
| Acknowledge | `actions/acknowledge-problem` | comment, `sticky`, `persistent`, `expiry` (`notify` always sent as `false`) |
| Remove acknowledgement | `actions/remove-acknowledgement` | — |
| Schedule downtime | `actions/schedule-downtime` | start/end, fixed or flexible + `duration`, `all_services` (hosts), `child_options` (none / triggered / non-triggered), `trigger_name` |
| Remove downtime | `actions/remove-downtime` | one downtime, or all downtimes of an object |
| Add / remove comment | `actions/add-comment`, `actions/remove-comment` | comment, `expiry` |
| Submit passive check result | `actions/process-check-result` | exit status, output, perfdata, `ttl` |
| Run command | `actions/execute-command` (Icinga 2.13+) | command type, command, endpoint, macros. Behind a confirmation |

**No attribute switches and no Icinga notification actions** (D6): no `send-custom-notification`, no `delay-notification`, no `objects/modify`. Icinga's `enable_*` flags are shown read-only in the pane; notification muting is local (§2.7).

Every action flows through one `Command` type (§3.3): optimistic UI state ("ack pending…"), the result as an inline toast, and failures (including 403) shown on the button that caused them.

**Keyboard**: `a` ack, `d` downtime, `r` check now, `c` comment, `x` select, `j/k` move, `Enter` open pane, `⌘Enter` open as tab, `Esc` close pane, `⌘K` palette, `⌘1…9` dashboards. All bindings are GPUI actions, so they are rebindable through a keymap file.

### 2.4 Detail panes
- **Service**: header, actions, plugin output (`output` + `long_output`), perfdata table parsed from Nagios perfdata (value / warn / crit / min / max, coloured when a threshold is crossed), check details (command, interval/retry, last/next check, attempt, latency, execution time, endpoint, check source), notification info (enabled, last sent), flapping %, custom vars, groups, comments and downtimes with remove buttons, `notes` / `notes_url` / `action_url` links.
- **Host**: the same plus address/address6, services list (OK collapsed), sub-tabs **services · history · vars · config**, parents/children from dependencies. "history" = local recent events for this host.
- "↗ open as tab" pins the pane as a closable sidebar entry under an "open" section.

### 2.5 Dashboards ("threads") and groups
- **Group** = named, ordered folder of dashboards inside one environment (D1). Group `+` adds a dashboard. Group `···` covers rename, reorder, delete, and notification settings.
- **Dashboard** = named list of **views**. A view = object type + **Icinga filter expression** (e.g. `host.vars.role == "postgres" && service.state != 0`) + columns + sort + group-by + display (list / grouped list / host-group grid / summary / event stream).
- v1 renders one view per dashboard exactly like the v2 list. Multi-view layouts come in M7.
- Sidebar dot + count = worst unhandled state and number of unhandled problems (**client-side**, evaluated by `ic-filter` against the live object store, so counts stay current without extra API queries).
- Stored as versioned TOML per environment. Import and export are file-based (handy for sharing dashboards across the team).

### 2.6 Live events (no Icinga DB)
- The connection runtime subscribes to `/v1/events` for `CheckResult`, `StateChange`, `Flapping`, `AcknowledgementSet/Cleared`, `CommentAdded/Removed`, `DowntimeAdded/Removed/Started/Triggered`, `ObjectCreated/Modified/Deleted`, and `Notification` (Icinga's own notifications: they keep the panes' "notified" row current).
- Events update the in-memory store. A trimmed copy (state changes, acks, downtimes, flapping) goes into a local SQLite log with **48h retention** (configurable). The log feeds the notification centre, the "recent events" view and the host/service history tab. Nothing more ambitious than that (D3).

### 2.7 Native notifications
Client-side rules on the live event stream (D5).

**Rule scopes**, inherited from top to bottom; each level can inherit, override, or mute:
1. **Environment default** (e.g. "hard CRITICAL/DOWN + recoveries, skip handled").
2. **Group** (e.g. the `databases` group notifies, `sandbox` is silent).
3. **Dashboard / thread**: inherit / on / off / custom.
4. **Object**: "watch" or "mute" a host or service from its pane. Mute can expire ("mute for 2h").

Bell controls live in the group and dashboard `···` menus. Muted items show a bell-off glyph in the sidebar.

**Conditions**: states (critical, warning, unknown, down, unreachable, recovery); hard only (default) or soft too; skip handled; event kinds (state change, ack set/cleared, downtime start/end, flapping start/stop); minimum time in state (e.g. only after 5 min, cancelled if the object recovers before then); quiet hours; sound on/off; urgency.

**Matching**: the object an event belongs to is tested against every dashboard's filter. The **most specific scope wins** (object > dashboard > group > environment). An object in several dashboards produces **one** notification (dedupe key: object + new state + state-change timestamp).

**Storm control**: more than N matching events within a few seconds → one summary notification ("14 new problems in databases/production").

**Content**: title `CRITICAL · postgres-replication on db-prod-03` (with several environments, the environment's name in front: `staging · CRITICAL · …`), body = first line of the output, subtitle = group / dashboard. Click → bring the window back (or create it), switch to the notification's environment and open the object pane. **Acknowledge** / **Open** action buttons on both platforms; *Acknowledge* acts in the notification's own environment without switching. Every environment notifies, whichever is on screen (NOTE-08).

**Delivery**: GPUI's built-in `cx.show_system_notification` (XDG notifications via `notify-rust` on Linux, `UNUserNotificationCenter` on macOS, action responses come back on the main thread). It's verified on Linux (`docs/spikes.md`). macOS only delivers from an app bundle, so dev builds on a Mac run from a dev `.app` (`cargo xtask bundle`: a debug build unless `--release`). The adapter sits behind a `Notifier` port, so a richer Linux backend (urgency, replace) can be swapped in later.

**Notification centre**: the clock icon in the footer (its badge counts the environment on screen) lists recent notifications from the local log, newest first under time sections, with unread state, "mark all read" (what the list shows), and pause. With several environments, sub-tabs pick the scope: the environment on screen (default), another, or all; a scope with unread notifications shows in the accent colour. Labels say where a notification matched (a click filters to it); silent ones say why; a storm collapses into its summary.

**Pause**: pausing (30m / 1h / until 08:00) holds for every environment; one environment can also be muted on its own (from the switcher or the palette), shown there as muted.

**Background mode (D4)**: closing the window keeps the process and every environment's connection runtime alive (GPUI `QuitMode::Explicit`, verified on Linux). A tray / menu-bar icon (`tray-icon`: StatusNotifierItem over D-Bus on Linux without GTK, `NSStatusItem` on macOS) shows the worst unhandled state across all environments (coloured dot), a tooltip with a line per environment, and has a menu with Open, Pause notifications (30m / 1h / until tomorrow), Environment, and Quit. Optional launch at login (LaunchAgent on macOS, XDG autostart `.desktop` on Linux).

### 2.8 Other
- **⌘K command palette**: objects, dashboards, actions on the focused or selected objects, pause/mute notifications, environment switch.
- **Cluster health**: endpoints/zones connected state and `/v1/status` stats (checks/min, latency, pending).
- **Settings**: environments, notification defaults, quiet hours, theme, reconcile interval, log retention, launch at login.

---

## 3. Architecture

### 3.1 Principles
1. **Layered, one-way dependencies.** Pure domain at the bottom, I/O in the middle, UI on top. The UI never talks HTTP. The core never imports GPUI.
2. **Functional core, imperative shell.** Filter evaluation, severity sorting, perfdata parsing, rule matching, dedupe and storm control are pure functions over plain data, so they're unit-testable without network, OS or UI.
3. **Single source of truth per environment.** One `ObjectStore` per environment (every environment's engine runs, D2), changed only by its sync engine. Views are derived from the active one's.
4. **Commands in, events out.** The UI sends `Command`s (acknowledge, schedule downtime, …) to the core. The core publishes `CoreEvent`s (store changed, connection state, command result, notification raised). No shared mutable state across the boundary except an immutable store snapshot (`Arc`).
5. **Ports and adapters for the OS.** Keychain, notifications, tray, autostart and file paths are traits in core. Implementations live in `ic-platform` (keychain, tray, autostart) and `ic-app` (notifications, which go through GPUI). Tests use in-memory fakes.
6. **Typed errors in libraries, context at the edge.** `thiserror` enums in every library crate. `anyhow` only in the binary's `main`. No `unwrap`/`expect` outside tests (enforced by lints).

### 3.2 Workspace layout

```
Cargo.toml                 workspace, [workspace.dependencies], [workspace.lints]
rust-toolchain.toml        pinned stable toolchain + clippy, rustfmt
rustfmt.toml  clippy.toml  deny.toml  .github/workflows/ci.yml
assets/                    app icon, tray icons (bundling)
docs/                      spike results, development notes
crates/
  ic-model/       pure domain types: Host, Service, HostState/ServiceState, StateType,
                  Acknowledgement, Comment, Downtime, CheckResult, Perfdata (+ parser),
                  Severity ordering, handled/unhandled, durations. No I/O, no async.
  ic-filter/      lexer + parser + evaluator for the Icinga filter DSL subset, evaluated
                  against ic-model objects. Pure.
  ic-api/         Icinga 2 REST client: transport (reqwest + rustls, CA / pin / client cert),
                  typed queries, all actions (§2.3), /v1/status,
                  /v1/events NDJSON stream with reconnect + backoff. Wire DTOs (serde) are
                  private and mapped into ic-model types at the edge.
  ic-config/      settings, environments, groups, dashboards, notification rules:
                  serde types, versioned TOML with migrations, atomic writes, paths.
  ic-rules/       notification rule engine: scope resolution, condition matching, delayed
                  ("min time in state") timers as data, dedupe, storm control. Pure; outputs
                  `NotificationIntent`s.
  ic-core/        application engine, UI-agnostic: EnvironmentRuntime (tokio), sync engine
                  (initial load → event stream → periodic reconcile), ObjectStore + derived
                  dashboard views, Command dispatcher, event log (rusqlite), traits for
                  Keychain / Notifier / Tray / Autostart.
  ic-platform/    adapters: keyring (Keychain / Secret Service), tray (tray-icon: ksni
                  backend on Linux, NSStatusItem on macOS), autostart (LaunchAgent /
                  XDG autostart).
  ic-ui-kit/      GPUI building blocks styled to the design: Theme/tokens, bundled
                  IBM Plex Mono (OFL), StateCircle, ListRow, PaneHeader, Button+KeyHint,
                  SubTabs, KvTable, CodeBlock, Toast, Dialog. Text inputs from
                  gpui-component, re-themed.
  ic-app/         the binary: GPUI app, window and views (Sidebar, DashboardList,
                  ServicePane, HostPane, dialogs, CommandPalette, Settings,
                  NotificationCenter), actions + keymap, bridge between ic-core and GPUI,
                  GPUI-backed Notifier.
  ic-mock/        Icinga 2 API look-alike (§3.6) with scenario data, simulator and fault
                  injection; lib + `icinga-mock` binary. For tests and development, and
                  linked into the app only for `icygui --demo` (ENV-10).
spikes/           M0 experiments kept as regression checks (background mode, tray,
                  notifications); `linux-headless.sh` runs them under Xvfb in CI.
xtask/            cargo xtask: icons, bundle (.app/.dmg, .deb/.tar.gz), mock (start all
                  mock environments), screenshots (README images from --demo)
```

Dependency graph (arrows = "depends on"):

```
ic-app ──► ic-ui-kit ──► (gpui, gpui-component), ic-model
   │
   ├────► ic-platform ──► ic-core (traits only)
   └────► ic-core ──► ic-api ──► ic-model
               ├────► ic-rules ──► ic-filter ──► ic-model
               └────► ic-config ──► ic-model

ic-mock ──► ic-model, ic-filter          (dev-dependency of ic-api, ic-core; ic-app links it for `--demo`)
```

`ic-mock` keeps its own wire structs, written from the API docs, instead of sharing `ic-api`'s, so a mistake in `ic-api`'s serde mapping can't be mirrored by the mock and go unnoticed.

`ic-model`, `ic-filter`, `ic-rules` and `ic-config` have no async runtime and no I/O beyond `ic-config`'s file access. That keeps them fast to compile and test.

### 3.3 Runtime model

```
                 Command (mpsc)                     HTTP / event stream
  GPUI main  ───────────────────►  EnvironmentRuntime ◄──────────────────► Icinga 2 API
  thread     ◄───────────────────  (tokio, bg thread)
                 CoreEvent (mpsc)        │  owns ObjectStore, SyncEngine,
                 + Arc<StoreSnapshot>    │  RuleEngine, EventLog, Notifier
```

- **Tokio runtime** on a dedicated background thread, owned by `ic-core`. GPUI keeps its own executor on the main thread. The bridge in `ic-app` is a GPUI task that drains `CoreEvent`s and updates `Entity<…>` models, so views re-render through normal GPUI notifications.
- **Sync engine**: (1) parallel initial queries for hosts, services, comments, downtimes, groups with explicit `attrs`; (2) open `/v1/events` and apply changes; (3) reconcile with a lean reload and diff (adaptively every 5 or 15 minutes, configurable; after a reconnect that followed a long gap, with jitter; after a restart), because the stream has no replay and reconnects leave gaps; never periodic full-attribute reloads (docs/performance.md). Store updates are batched (~100ms) into one new snapshot so a check-result burst doesn't cause a burst of re-renders.
- **Notifications don't need the UI**: the rule engine runs inside the runtime, so background mode works with no window open.
- **Commands** carry an id. The runtime runs the request, then emits `CommandResult { id, outcome }`. Optimistic UI state is keyed by that id.

### 3.4 Code quality and tooling

Workspace lints (inherited by every crate with `[lints] workspace = true`):

```toml
[workspace.lints.rust]
unsafe_code = "deny"            # opt back in per module with #[expect(unsafe_code, reason = "...")]
missing_debug_implementations = "warn"
missing_docs = "warn"
unreachable_pub = "warn"
unused_qualifications = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
# restriction lints we opt into
allow_attributes = "warn"       # #[expect] instead of #[allow], so stale exceptions fail
allow_attributes_without_reason = "warn"
dbg_macro = "warn"
expect_used = "warn"
panic = "warn"
print_stderr = "warn"
print_stdout = "warn"
todo = "warn"
undocumented_unsafe_blocks = "warn"
unimplemented = "warn"
unwrap_used = "warn"
# pedantic lint that fights GPUI's builder style
must_use_candidate = "allow"
```

- CI runs `cargo clippy --workspace --all-targets --locked -- -D warnings`, so every warning above fails the build. `clippy.toml` relaxes `unwrap`/`expect`/`panic`/`print`/`dbg` inside tests only.
- Exceptions use `#[expect(lint, reason = "…")]` so they fail once they're no longer needed.
- `rustfmt` check and `cargo-deny`: licence allow-list, vulnerability advisories for every crate, "unmaintained" only for our direct dependencies (GPUI's transitive ones aren't ours to replace), and crates.io as the only source.
- Edition 2024, Rust 1.97.0 pinned in `rust-toolchain.toml`.
- **GPUI source**: `gpui-pre` =0.3.8 from crates.io (a snapshot of zed@279fe07, published by the gpui-component maintainer), renamed to `gpui` in the workspace, plus `gpui-pre-platform` with the `wayland`, `x11` and `font-kit` features. That's the exact GPUI `gpui-component` 0.7.1 is built against, so both share one set of types. crates.io's own `gpui` (0.2.2) is a year old. GPUI is only used in `ic-ui-kit`, `ic-app` and `spikes`, so switching to a Zed git revision later is contained.
- Logging: `tracing` with a rolling file appender in the log dir. Credentials and auth headers are never logged (a redacting `Debug` impl on secret types).
- CI matrix: Linux (x86_64) and macOS (arm64): fmt → clippy → test, plus the headless background-mode spike on Linux. A nightly job runs the `ic-api` integration suite against the `icinga/icinga2` Docker image as a contract test for `ic-mock` (§3.6); another (`perf.yml`, PERF-07) runs the `#[ignore]`d production-scale tests in release builds against `ic-mock`'s `large` scenario, where they fail when a budget of `docs/performance.md` is missed.

### 3.5 Testing strategy
| Layer | How |
|---|---|
| `ic-model` | Unit tests, and property tests (`proptest`) for the perfdata parser and severity ordering |
| `ic-filter` | Table-driven tests; expressions cross-checked against a real Icinga via the API's `filter` |
| `ic-rules` | Scenario tests: event sequences in → `NotificationIntent`s out (scope resolution, dedupe, storm, min-time-in-state, quiet hours), using a fake clock |
| `ic-api` | Integration tests against an in-process `ic-mock` on a random port: queries, every action, event stream, TLS pinning, auth failures, 403s, reconnects |
| `ic-core` | Runtime tests against `ic-mock` with scripted scenarios (state flips, storms, dropped streams) and fake platform ports; reconcile diff tests |
| `ic-app` | GPUI `TestAppContext` tests for list/pane/selection state and keyboard actions, fed with `ic-mock` scenario data |
| Contract | Nightly: the `ic-api` suite runs against the `icinga/icinga2` Docker image with an equivalent seeded config, so the mock can't drift from the real API |

### 3.6 Mock environments (`ic-mock`)

There's no real Icinga to develop against, so the project gets its own: a small Icinga 2 API look-alike that the client can't tell apart from the real thing for everything it uses.

- **Wire-compatible subset** of the API, over **HTTPS** with a self-signed certificate (fingerprint printed at start, so the client's pinning path gets exercised) and **Basic auth** with configurable users and permissions (e.g. a read-only user that gets 403 on actions):
  - `GET /v1`, `GET /v1/status`
  - `GET /v1/objects/{hosts,services,hostgroups,servicegroups,comments,downtimes,users,usergroups,notifications,dependencies,endpoints,zones,checkcommands}` with `filter` (evaluated by `ic-filter`), `attrs`, `joins`, and the `X-HTTP-Method-Override: GET` form
  - `POST /v1/actions/*` for every action in §2.3. Each one changes the mock's state and emits the matching events (ack → `AcknowledgementSet`, downtime → `DowntimeAdded`/`Started`, check now → a `CheckResult` shortly after)
  - `POST /v1/events`: an NDJSON stream honouring `types` and `filter`
- **Environments** are scenario files: `prod-cluster` (the design's sample data: postgres-replication on db-prod-03, rabbitmq-queue on mq-prod-01, the down k8s node with unreachable children, …), `staging` (small), `lab` (nearly empty), `large` (generated, ~2k hosts / 20k services for performance work). `icinga-mock --env prod-cluster:5665 --env staging:5666 --env lab:5667` serves several at once; `cargo xtask mock` starts the default set.
- **Simulator**: seedable and deterministic. Steady churn (weighted random state changes, soft → hard after `max_check_attempts`), recoveries, flapping, a host going down with its children turning unreachable, downtimes starting and ending, and **storms** (dozens of problems in a few seconds, for notification storm control). There's a speed factor for demos.
- **Fault injection**: drop event streams (tests reconnect + reconcile), added latency, 5xx responses, bad credentials, a rotated certificate (pin mismatch).
- **Test API**: tests run the mock in-process on a random port with a control handle (`set_state`, `emit`, `drop_streams`, `advance_clock`), so tests don't need sleeps.
- **Dev**: a dev config lists the mock environments, so they show up in the app's environment switcher like real ones.

---

## 4. Milestones

1. **M0 Foundations + spikes** ✅: workspace, lints, CI, `cargo-deny`, theme tokens, bundled font, themed window, spikes (`docs/spikes.md`).
2. **M1 Static UI + scenario data**: `ic-model` objects (hosts, services, acks, downtimes, comments, perfdata, severity), `ic-mock` scenario data, then sidebar, list (2a) and panes (2b/2c) rendered from it, pixel-matched to the design. Client-side window decorations on Linux.
3. **M2 Live data against mock environments**: `ic-mock` server (objects, events, simulator, faults) and `ic-api` built together, then environments + keychain, sync engine, footer status, environment switcher.
4. **M3 Actions**: all §2.3 actions, dialogs, bulk selection, optimistic state, permission-aware buttons.
5. **M4 Dashboards + filters**: `ic-filter`, group/dashboard CRUD in the sidebar, counts and dots, sort/group-by, ⌘K palette.
6. **M5 Notifications + background**: `ic-rules`, GPUI notifier, `QuitMode::Explicit` + tray, launch at login, notification centre, event log, dev `.app` bundle for testing on macOS.
7. **M6 Packaging**: signed `.app`/`.dmg`, AppImage + `.deb`, notification categories on macOS.
8. **M7 Polish**: multi-view dashboards, host-group grid, cluster health, light theme.

**Releases:**
- **rc1** = M0–M6: every requirement in `docs/requirements.md` except those marked *(v1)*, after a general review and fix pass over the whole project. rc1 is merged into `main`, then tried against a real production Icinga.
- **v1** = rc1 + M7 + the fixes from that production trial. M7 is built after rc1, not squeezed into the rc1 work.

### 4.1 Road to rc1 (remaining work, in order)

Status markers: ✅ done · ⏳ in progress · ☐ not started.

1. **Environments step** (ENV-01, ENV-12, NOTE-08). ✅ Built, reviewed (19 findings) and fixed: several API URLs per environment for any Icinga topology (single master, HA pair, master with satellites or child zones, a load balancer or round-robin DNS in front); the connected node's zone decides between a full view, a partial view (always labelled) and an unverified view; one engine per saved environment, so every environment notifies and switching is instant; the footer switcher and the notification centre across environments.
   ⏳ **Follow-up round on the user's feedback:** built (every point below; the footer keeps its layout without `·` separators, which don't fit its 300 px beside both names), waiting for the user's review of the screenshots:
   - **Footer trigger:** environment name, then the connected node, then the age of the last event (`prod-cluster · master-01 · 0s`) with the health dot; the node changes the moment icygui fails over to another master.
   - **Switcher popover, environment list:** the selected environment is marked with the selected-row background, like a selected service in the list, with no check-mark column, so all rows align; no notification or object counts on the rows (that is what the notification centre is for); each row ends in a right-aligned gear icon in a fixed slot (dimmed, brighter on hover or selection) that opens that environment's editor without switching; "add environment" stays; the "edit <environment>" item goes.
   - **Switcher popover, nodes section:** every master and satellite of the cluster, one per line with a coloured dot (green when connected, the state colour when not), its zone dimmed (`master-01 · master`), the node icygui is connected to marked with the selected-row background; long lists scroll inside the popover, and the popover fits small windows. Data from the endpoints and zones icygui already loads plus the 30-second status poll: at most one extra small request per poll (every 5 minutes in quiet mode). The full per-node cluster health view stays in v1 (M7).
   - **Notification centre:** the environment scope moves from under the title to a bar at the bottom of the panel, styled like the app's chips and selection (the pause chips, the bulk-action bar), the selected scope with the filled selection background, no underline tabs; it replaces the "history kept on this computer" text, which goes away; "notification settings" becomes a gear icon in that bottom bar; title row, pause row, list and bottom bar share one alignment grid; unread per scope stays colour-only; with one environment the scope control is hidden; many environments overflow without wrapping.
   - **Command palette:** every row has a mark in the dot's fixed slot: the target's state dot for object actions (acknowledge, downtime, check now, comment, remove…), the stack mark in the worst state's colour for "all N matches", a small muted icon for commands without a target (reload, new dashboard, pause, settings); object labels always read `service on host · state`, never the raw `host!service` key; the open or selected object no longer gets separate rows: its actions are ordinary rows that rank first and show their key hints (a, d, r, c), so the same object never appears twice.
   - **App-wide labels:** no trailing "…" on button, menu or command labels; an ellipsis only where text is actually cut off.
   - The user reviews real screenshots of each of these before the step counts as done.
2. ☐ **Quiet mode** (new requirement PERF-09; adjusts PERF-05, PERF-08 and the NOTE requirements).
   - **When:** for every inactive environment, and for the active one while the window is hidden (closed to the tray, or minimised where the operating system reports it reliably). Not when the window is merely unfocused: a dashboard on a second screen stays fully live. On by default, with a switch in the general settings. No "stay fully live" setting per environment and no per-dashboard "never throttle" toggle (the stream can't be narrowed per object without the `filter-expression` permission, and notifications are never throttled anyway).
   - **What changes:** the event stream subscribes only to state changes, acknowledgements, downtimes, flapping, notifications and object creation or deletion, not to check results (about 95–99 % less stream traffic per client); notifications are never delayed; the status poll runs every 5 minutes, the reconcile about every 30 minutes; no late-check watchdog and no loading of row details; only dashboards with notification rules are evaluated. Stale while quiet: output text, last-check times and late markers; after an acknowledgement or downtime ends, the "still a problem?" confirmation uses the rule engine's 1–5 minute fallback.
   - **Instant wake-up** (opening an object that went red must feel faster than 1–2 s): state-change events carry the full check result, so an object that changed state is already fresh; a notification prefetches its object's full details in the background (one small request, skipped beyond the storm threshold); on waking or opening, the object being opened gets one by-name request before anything else, then the rows on screen, then the remaining problems, while the full stream reopens without a gap; the pane never blocks and shows an "updating" hint only after about 300 ms, in a fixed slot.
   - **Load pacing scales with the installation, never by fixed times:** background starts (launch at login) wait a random delay proportional to the number of services (about 3 s per 1 000 services, at most 90 s; under a second for small installations; a start the user opens never waits); all by-name requests of a client share a request budget (a token bucket, e.g. 5 per second with bursts of 10) that small installations never reach, with the object being opened always first; the reconcile interval follows the object count instead of fixed steps and stretches (up to 60 minutes) while the event stream has been continuous; quiet mode applies on top.
   - **Measured, not guessed:** quiet versus live (stream events per second, requests per minute, client CPU) against `ic-mock`'s large scenario; the time from a notification click to a filled pane; mode switches without lost events; 20 simultaneous starts against the local Docker Icinga at a small and a large size (master memory, response times) to calibrate the pacing factors. Never against a production Icinga. Results go into `docs/performance.md`.
3. ☐ **Palette regression test:** Ctrl-Enter on any verb always opens the dialog listing every target and never acts directly (the palette's behaviour is otherwise unchanged until the trial).
4. ☐ **Final gate and merge:** fmt, clippy with `-D warnings`, all tests, the contract tests against the Docker Icinga, `cargo deny`, CI green on Linux and macOS; then rc1 is merged into `main` and tried at work with `docs/first-run.md`.

### 4.2 After rc1 (v1)

- **M7:** multi-view dashboards, the host-group grid, the full cluster health view (zones, endpoints, latency, work queues), the light theme with follow-system (UI-02).
- **Fixes from the production trial**, including the macOS desktop integration (notifications, the menu-bar item, the tray, close to tray), which so far is only compiled and unit-tested in CI and has never run on a real Mac.
- **Filter autocomplete** in the dashboard editor (and other filter fields), in the style of Zed: completions from the parser at the cursor and from the live data (attributes with descriptions after `host.` and `service.`, the custom variable names that exist after `vars.`, ranked by how often they occur, the values in use inside strings, host group, service group and service names, functions with their signatures, state constants); fuzzy matching; Tab or Enter to accept, Ctrl-Space to open; hover help; the parse error underlined at its position.
- **Palette multi-select:** Shift+arrows extend the selection, Cmd/Ctrl+A selects all matches, Cmd/Ctrl+click toggles rows; the selection is drawn as one rounded outline per block, with a counter in the palette footer ("3 selected · ↵ acknowledge all"); Enter with a verb opens one bulk dialog for the selection, Enter without a verb opens a combined view of the selected hosts like Icinga Web's (a summary row per host, their services grouped by host, the bulk action bar; pinnable as a tab or savable as a dashboard). The "all N matches" row lists every object it counts. Decided after the user has tried the current palette at work.
- **Ideas offered, not yet decided:** sharing a dashboard through the clipboard; team dashboard groups that follow a shared file or URL read-only.

### 4.3 Product rules (standing decisions)

- **Gentle on production Icinga:** no load or contract tests against a production instance (the contract tests refuse anything but the disposable Docker Icinga); lean loads, by-name batches, jittered reconnects, request budgets that scale with the installation; no periodic full-attribute reloads.
- **The operator always sees what an action targets** before anything is sent.
- **Design:** every element fits the existing design and components (design files, theme tokens, `ic-ui-kit`); nothing changes size or position with state (unread, health, mute, connection); selection is the selected-row background, never a check-mark column; every list and palette row has a mark in its fixed dot slot; object labels read `service on host`; no duplicate rows for the same object; no trailing "…" on labels (only for real truncation). New UI is shown to the user as real screenshots, never as text mock-ups.
- **Environments:** one environment per Icinga cluster, with one or more API URLs; every environment notifies, whichever is on screen; the connected node is always visible and changes on failover.

---

## 5. API permissions the client needs

Read: `objects/query/{Host,Service,HostGroup,ServiceGroup,Comment,Downtime,Notification,Dependency,Endpoint,Zone}`, `status/query`, `events/*` for the event types in §2.6. These are exactly the types rc1 queries (who was notified comes from `Notification` objects; check commands are shown by name from the objects). `objects/query/{User,UserGroup,CheckCommand}` join the list only with a feature that reads those objects (users and commands in the Config tab, M7); asking for them earlier would grant read access to contact data and command lines for nothing.
Operate: `actions/{reschedule-check,acknowledge-problem,remove-acknowledgement,schedule-downtime,remove-downtime,add-comment,remove-comment,process-check-result}`. Opt-in: `actions/execute-command`, which with free-form macros can run any command on the agents; the user guide's `ApiUser` leaves it out and explains how to grant it (a separate `ApiUser`, a permission filter).
The client probes permissions on connect, greys out what it can't do, and shows why on hover. A ready-to-paste `ApiUser` snippet (for your Ansible role) goes in the README.

---

## 6. Risks

Results of the M0 spikes; details in `docs/spikes.md`.

| Risk | Status |
|---|---|
| GPUI on Linux quits the app when the last window closes | **Resolved**: `QuitMode::Explicit` keeps it running; reopening works. Verified on Linux |
| Tray on Linux needs a GTK loop GPUI doesn't run | **Resolved**: `tray-icon` with the `ksni` backend, no GTK. Menu clicks reach GPUI. Verified on Linux. GNOME still needs the AppIndicator extension to *show* it |
| Native notifications | **Resolved on Linux** with GPUI's built-in API, including action buttons. **macOS open**: bundle-only; needs a run on a Mac |
| macOS menu-bar icon next to GPUI | **Open**: needs a run on a Mac (`cargo run -p spikes --bin background`) |
| The rest of the macOS desktop integration: close to the menu bar, the tray menu, launch at login, keychain access after `install.sh` re-signs an update | **Open**: built, but only run on Linux (the UI and tray tests are Linux-only). Checklist in `docs/spikes.md` (*Still to run on a Mac*) |
| GPUI source is a third-party snapshot (`gpui-pre`) | Accepted for gpui-component compatibility. GPUI is confined to `ic-ui-kit`/`ic-app`; bump `gpui-pre` and `gpui-component` together |
| macOS builds need the Metal compiler (separate download since Xcode 26) | Documented; CI installs it when missing |
| The mock drifts from the real API | Nightly contract tests against `icinga/icinga2` in Docker (§3.5) |
| Large installations (10k+ services) | `uniform_list` virtualisation, snapshot batching, `attrs` restricted to what is displayed; `ic-mock`'s `large` environment for profiling |
