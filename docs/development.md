# Development

## Requirements

- Rust: installed automatically by rustup from `rust-toolchain.toml` (1.97.0, with clippy and rustfmt).
- **Linux** (Debian/Ubuntu package names):
  ```sh
  sudo apt-get install libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev libx11-xcb-dev \
    libwayland-dev libfontconfig-dev libfreetype-dev
  ```
  A Vulkan driver is needed at runtime; every desktop Mesa/NVIDIA install has one.
- **macOS**: Xcode with the Metal toolchain. On Xcode 26 and later, install the toolchain once:
  ```sh
  xcodebuild -downloadComponent MetalToolchain
  ```

## Everyday commands

```sh
cargo run -p ic-app                                  # the client
cargo test --workspace                               # tests
cargo clippy --workspace --all-targets -- -D warnings  # what CI enforces
cargo fmt --all
cargo deny check                                     # licences, advisories, sources
cargo run -p ic-app -- --demo                        # the app against a simulated Icinga
```

`cargo xtask` has the project's other tasks: `mock` (the mock Icinga environments as servers), `screenshots` (below), `icons` (after editing the logo), `bundle` and `package` (see `docs/releasing.md`), `install` (Linux, into `~/.local`) and `version`.

The first build compiles GPUI and takes a few minutes. Dependencies are built with `opt-level = 2` even in dev builds, so the UI stays smooth.

## Tests against Icinga, and load

- **Against a real Icinga:** only the disposable ones: the single master from `contract/run-icinga.sh` and the demo cluster from `demo/docker-compose.yml` (below). `cargo test -p ic-api --test contract` runs against either (see `contract/README.md`); the engine's cluster tests (`cargo test -p ic-core --test engine cluster::`) only against the demo cluster. Both refuse any other instance before sending a query: the URL must point to this machine and the fixture-only `viewer` user must log in; the cluster tests also check that the node is the cluster's `master-01` and that the compose project runs, since they stop its nodes.
- **Load and scale tests** run only against `ic-mock` (its `large` scenario and `MockControl::burst`) or `contract/scale/benchmark.sh`, which starts its own Icinga in Docker on localhost. Never point a load test at a production Icinga. The production-size tests are `#[ignore]`d; `.github/workflows/perf.yml` runs them nightly with `--release`, where they assert docs/performance.md's budgets (its steps are the commands to run them yourself).
- **Against a production Icinga,** the only test is normal use of the app: connect and look around, which costs one lean load like an Icinga Web session.

## The demo cluster (real Icinga in Docker)

`demo/` holds a real Icinga 2.15 cluster for Docker Compose: the demo users run icygui against (docs/demo.md), the harness for tests about Icinga's behaviour, the source of screenshots, and the base of the many-clients measurements. Two masters (`master-01` holds the configuration), the satellite zone `ams` and the HA satellite zone `fra`, about 280 hosts and 2 900 services whose states change by themselves, the six heartbeats of PLAN.md B3, acknowledgements, comments, downtimes and notifications.

| File | What it does |
|---|---|
| `demo/docker-compose.yml` | The cluster: a one-shot `pki` service makes the certificates with Icinga's CA (the CA and its key in the `ca` volume, which only master-01 mounts), five nodes (health checks: connected to every endpoint they talk to, master-01 also seeded), a TCP proxy for the dead-network scenario. Only the masters' API is published, on 127.0.0.1:5665 and 5666 (proxy 5667; `ICYGUI_DEMO_PORT_1`, `_2`, `_PROXY` move them). `ICYGUI_DEMO_SCALE_HOSTS=N` adds `contract/scale/generate.py`'s N hosts x 15 services to the master zone (below). |
| `demo/icinga/node.sh` | Each node's start: certificates, constants, zones (children connect to parents, masters to each other), features, API listener (masters enforce the filter-expression permission like contract/icinga/api.conf), then `icinga2 daemon`. |
| `demo/icinga/generate.py` | The objects, the same every run, into master-01's `zones.d` at every start; reuses `contract/scale/generate.py`'s outputs. Checks are Icinga's dummy check under plugin names; `vars.demo` holds each one's schedule (steady, short problems now and then, flapping, stuck). The contract fixtures (`contract/icinga/icygui-test.conf`, `icygui-groups.conf`) are loaded as they are, so the contract tests run here too. |
| `demo/icinga/seed.py` | After every start of master-01: passive results for the fixtures' passive checks, acknowledgements, comments and downtimes (only what is missing), then it keeps feeding the passive checks every 50 s as an outside system would. |
| `demo/up.sh` | `up -d`, waits until every node is healthy (also after a scenario left nodes unhealthy, where `up --wait` gives up), writes the CA to `target/demo-ca.crt` (`ICYGUI_DEMO_CA` elsewhere) and prints the tests' variables. |
| `demo/scenario.sh` | Breaks the cluster (`satellite-down`, `zone-cut-off`, `master-down`, `frozen-master`, `checks-stopped [zone]`, `beat-late [s]`, `problem-storm [n]`, `dead-network`), or gives the event streams lines (`blips [n]`), and says what icygui should show; `recover` mends it and waits until no check is overdue (Icinga's `next_update`; at most `ICYGUI_DEMO_SETTLE`, 300 s). API calls go through `docker compose exec` as `demo-admin`. |
| `demo/icygui/environment.toml`, `dashboards.toml` | The environment (settings file format) and the dashboards (the share format) for this cluster. |
| `demo/icygui/write-config.sh` | `cargo xtask demo-config`: writes both into a settings directory through `ic-config` (the environment loads like a settings file, the dashboards import like a shared export, ids from their names), copies the cluster's CA next to it, and follows `ICYGUI_DEMO_PORT_*` like the compose file. It writes no password: icygui asks for it and keeps it in the OS secret store. |
| `demo/screenshot.sh` | The screenshot harness (below). |

Start and stop: `docker compose -f demo/docker-compose.yml up -d --wait`, `… down -v` (everything is made fresh on the next `up`). The real-Icinga tests:

```sh
set -a; eval "$(demo/up.sh)"; set +a
cargo test -p ic-api --test contract                          # 13 contract tests on master-01
cargo test -p ic-core --test engine cluster:: -- --test-threads=1   # stops and starts nodes, about 5 minutes
```

`ICYGUI_CONTRACT_REQUIRED=1` and `ICYGUI_CLUSTER_REQUIRED=1` turn missing variables into failures (the `Demo cluster` workflow, `.github/workflows/cluster.yml`, sets both: nightly and for changes to `demo/`, `contract/`, `ic-api`, `ic-core` and the crates it builds on (`ic-config`, `ic-filter`, `ic-model`, `ic-rules`), or `Cargo.lock`). The cluster tests are what the mock had wrong before: the relay queue when an endpoint is gone (flat), a pinned check whose endpoint is gone (UNKNOWN with Icinga's words, after the checking node's 5-minute cold start), a cut-off zone (silent: no results, nothing UNKNOWN, `cluster-zone` critical), and an HA master pair (both check, one reports its checker feature paused). Each leaves the cluster recovered.

**Many clients on the demo cluster** (PLAN.md 4.2, *Many clients on one master*; Docker only, never the real masters; while no builder runs, since a shared CPU skews the numbers). Measure at the user's size, not the demo's 2 900 services: `ICYGUI_DEMO_SCALE_HOSTS=2000` loads `contract/scale/generate.py`'s 30 000 services into the master zone as well (docs/demo.md, *A production-size estate*). The clients are headless engines, many in one test process, each with its own tiered load, event stream and reconciles, the same start the app makes: they load the master, not this machine (50 GUI processes would measure the laptop's CPU and memory instead).

```sh
ICYGUI_DEMO_SCALE_HOSTS=2000 docker compose -f demo/docker-compose.yml up -d --wait
ICYGUI_SCALE_URL=https://127.0.0.1:5665 ICYGUI_SCALE_CONTAINER=icygui-demo-master-01-1 \
ICYGUI_SCALE_USER=icygui-demo ICYGUI_SCALE_PASSWORD=icygui-demo-password ICYGUI_SCALE_CLIENTS=50 \
  CARGO_INCREMENTAL=0 cargo test --locked -p ic-core --test engine starts::many_clients_start_at_once \
  -- --ignored --exact --nocapture
```

`ICYGUI_SCALE_START=background` starts them as launch at login does; `starts::by_name_requests_and_reconciles_cost_the_master` with `ICYGUI_SCALE_PACING` (`budget:<ms>`, `reconcile`) measures by-name batches and the reconcile. Both sample master-01's memory and CPU (`ICYGUI_SCALE_CONTAINER`) and time another client's small query, `ICYGUI_SCALE_PROBE` (`host!service`, by default the workload's `web-0000.prod.example.com!ping4`), which must answer 200 or the run fails. master-02 serves nobody here but takes its HA share of the checks: watch it with `docker stats`. No scenarios meanwhile; `seed.py` feeds a few passive results every 50 s, a small steady load.

**Screenshots from the demo cluster** (builders and reviewers; replaces `--demo` once the builders switch): `demo/screenshot.sh` starts the cluster if there is none and otherwise uses it as it is (it never starts a stopped node or thaws a frozen one: the cluster is shared, and a scenario or a cluster test may be in effect; it stops with a message then, and `start --recover` runs `demo/scenario.sh recover` first), builds the debug app, writes a throwaway settings directory under `target/demo-shots/` with the demo environment and dashboards, and starts icygui on Xvfb `:93` (started if nothing answers there; `DISPLAY_NUMBER` picks another) with a private home and a D-Bus session of its own, in which a throwaway GNOME Keyring holds the demo password (below). Once the engine logs `connected to Icinga` it warms the event streams up (`demo/scenario.sh blips`: four database services fail and recover, so *databases*' *db events* has lines; `--no-warm` skips it) and waits `SETTLE` seconds:

```sh
demo/screenshot.sh start --theme dark --select platform/fleet   # returns once connected and settled (SETTLE, 10 s)
demo/screenshot.sh shot /tmp/fleet.png
demo/screenshot.sh open cluster health                           # the palette: ctrl-k, the text, enter
demo/screenshot.sh scenario master-down                          # demo/scenario.sh
demo/screenshot.sh shot /tmp/health-master-down.png 60           # wait 60 s first
demo/scenario.sh recover; demo/screenshot.sh stop
demo/screenshot.sh capture /tmp/x.png --theme light --select overview/databases --open "Handling" --scenario zone-cut-off --wait 120
```

`key` and `type` send keys and text with `xdotool`; `shot` captures the icygui window with ImageMagick's `import`. The app's log is `target/demo-shots/home/.local/state/icygui/logs/icygui.log` (its terminal output `target/demo-shots/app.log`; `ICYGUI_SHOT_DIR` moves the work directory). `start --keep` keeps the last start's data directory, so event streams and history keep what earlier runs showed. Start the app before the scenario: `start` refuses a cluster with a stopped or frozen node. A check pinned to a stopped endpoint turns UNKNOWN only once the node running it has been up for 5 minutes, so take master-down shots at least 5 minutes after master-01 started. Remember `demo/scenario.sh recover` after a scenario: it stays in effect.

**Passwords in headless runs** go through the OS secret store, as on a desktop: icygui has no other place for credentials. The harness starts a `dbus-daemon` of its own and `gnome-keyring-daemon --unlock --components=secrets` on it (the keyring's files in `target/demo-shots/home`, unlocked with the throwaway test value `icygui-harness`; the daemon ends with the bus at `stop`), then stores the demo password through icygui's own code: `printf %s "$PASSWORD" | target/debug/examples/store_secret <environment id>` (`crates/ic-platform/examples/store_secret.rs`, `ic_platform::KeyringSecrets`; the password on stdin, never on the command line). It needs `dbus-daemon` and `gnome-keyring-daemon` (Debian/Ubuntu: `apt install dbus-daemon` and `apt install --no-install-recommends gnome-keyring`). Headless engine tests use the core's in-memory test store and need neither.

## Running the app

- `cargo run -p ic-app` connects to every environment of the settings file (each runs its own engine) and shows the active one (the first one if none is marked). Without environments the window shows the onboarding form: name, URL, login, TLS, *test connection* and *connect*. More environments are added, edited and switched from the connection status in the sidebar's footer (or the command palette, `ctrl-k` / `⌘K`). Passwords go to the system keychain (Secret Service on Linux, Keychain on macOS); without one, saving a password login says so and keeps the form open.
- `cargo run -p ic-app -- --demo` runs the whole app against a simulated Icinga in the same process: an `ic_mock::MockServer` with the `prod-cluster` scenario (the design's sample data in a 150-host estate) and its simulator running (checks at their intervals, new problems, recoveries, flapping, outages, downtimes, a problem storm every five minutes), through the real `ic-core`. Actions work against it. The demo also has a `staging` and a `lab` environment, each with its own mock server and simulator (storms every 15 and 30 minutes); every environment's engine runs from the start, so all three notify (the title names the environment) and switching between them is instant. Nothing is saved, the keychain isn't touched, and the event logs live in a temporary directory.
- Dashboards: the group header's `+` and `···`, a dashboard row's `···` or right click, and the footer's `+` hold the commands (new, rename, duplicate, move, mute, delete, import, export). `ctrl-n` / `⌘N` creates a dashboard; the editor previews the filter live, `ctrl-s` / `⌘S` saves and Escape discards. Without a desktop file chooser (no xdg-desktop-portal), import and export ask for the file's path in a small dialog.
- Notifications and the background: problems matching a dashboard's rule go to the desktop (urgency and sound by state, *Acknowledge* and *Open* buttons) and into the notification centre behind the footer's clock icon (unread badge, silent ones too, mark read, pause 30m / 1h / until 08:00). `ctrl-,` / `⌘,` opens the settings: keeping the app in the tray when the window closes, launch at login, the event log's retention, the reconcile interval, and every notification rule (default rule, per group and dashboard inherit/on/off/custom, quiet hours, storm control, watched and muted objects). The pane's `···` watches or mutes its object; the host pane's *history* tab and the service pane's *history* section read the local event log. With a tray icon a tray host shows (not on stock GNOME without the AppIndicator extension), closing the window keeps the app running; *Quit* in the tray, the app menu or `ctrl-q` / `⌘Q` ends it. A second `icygui` brings the running one's window forward and exits; `--background` (what launch at login runs) starts in the tray without a window.
- `--version` and `--help` print and exit without opening a window.

Files (from `ic_config::Paths`): the settings in `config.toml` (with `config.toml.bak`), the window size and position, open tabs and selected dashboards in `state.toml` in the data directory, the event logs (`events-<environment id>.sqlite3`) in the data directory, and the log in `icygui.log` (rotated at 10 MiB, four old files kept) in the log directory. A settings file that can't be read is never replaced silently: the window offers to restore the backup, start fresh (the broken file is kept as `config.toml.unreadable-<time>`), try again, or quit.

Environment variables for development (the `ICYGUI_DEMO_*` ones only affect `--demo`):

| Variable | Effect |
| --- | --- |
| `RUST_LOG=info,icygui=debug,ic_core=debug,ic_api=debug` | The log filter (default `info`, with the GPU and D-Bus crates at `warn`). Start it with a level (`info,`): the targets it doesn't name are otherwise left out entirely, warnings and errors included. |
| `ICYGUI_DEMO_SCENARIO=large` | Serves another `ic_mock` scenario: `prod-cluster` (default), `staging`, `lab`, or `large` (production scale: 2 000 hosts, 30 000 services), to check that loading and scrolling stay smooth. |
| `ICYGUI_DEMO_SEED=7` | Fixes the simulator's seed (the default changes every run), for reproducible screenshots. |
| `ICYGUI_DEMO_FAULT=auth` | Shows a connection failure on purpose: `offline` (reconnecting with a countdown), `auth` (login refused), `tls` (certificate not trusted), `pin-mismatch` (another certificate is pinned: both fingerprints show), `missing-secret`, `misconfigured`, `outage` (the connection is lost 20 s in), `slow` (every answer takes 0.9 s, so the load's progress shows), `frozen` (the simulated Icinga stops checking, so checks are marked late after about two minutes) `partial` (the master answers 503, so the engine connects to the satellite `sat-ams-01` that `prod-cluster` lists as its second URL: a labelled partial view, ENV-12) `satellite-down` (20 s in, both satellites of zone `fra`, `sat-fra-01` and `sat-fra-02`, drop out of the cluster: the zone is cut off, its beats go silent, its checks go late, and the cluster health page and its sidebar dot turn critical: 06c, 16t) `satellite-checks-stopped` (20 s in, `sat-fra-02` drops out and `sat-fra-01`'s checker hangs while it stays connected: *zone fra runs no checks (sat-fra-01 connected)*, 16s) `beat-late` (zone `ams`'s heartbeat comes 13 s late every two minutes: the heartbeat row shows it *1 interval late* for a few seconds, 16a2, and nothing is raised) `no-comments` (the API user may do everything but add comments, so nothing offers to write one), `master-down` (20 s in, `master-02` disconnects: its pinned heartbeat comes back UNKNOWN with Icinga's words and the endpoint and its beat make one line in the alert block, notified after the 2-minute grace, 16r; four minutes later it connects again: *master-02 connected again*), `checks-stopped` (20 s in, the simulated Icinga stops running checks, as a hung checker does: the heartbeats stop, the REST query finds them old, the check rates fall to 0 and the one-minute checks go late, *Icinga runs no checks*, 16a3) or `beat-gone` (20 s in, zone `fra`'s heartbeat is deleted: a finding until its removal is confirmed in the settings). In the demo `prod-cluster`'s zone `fra` has two satellites (`sat-fra-01`, `sat-fra-02`) and heartbeats every 30 s (`vars.icygui_heartbeat`), six as in 16a: one pinned to each master, one per satellite zone, one pinned to each satellite of the HA zone `fra`. With `outage`, *no live data* is raised 2 minutes after the connection is lost, and the server answers again four minutes after the outage began (*prod-cluster live again*). Without a fault, or with `partial`, `slow`, `frozen`, `satellite-down`, `satellite-checks-stopped`, `beat-late`, `no-comments`, `master-down`, `checks-stopped` or `beat-gone`, `prod-cluster` lists its master and its satellite `sat-ams-01`, each served by its own mock; the other faults list the master alone. |
| `ICYGUI_DEMO_DASHBOARD=databases` | Selects a dashboard by name at start. |
| `ICYGUI_DEMO_OPEN=service` | Opens an object once it is loaded: `service` (postgres-replication beside the list, screen 2b), `host` (its host db-prod-03, screen 2c), `tab` (postgres-replication as a tab), an object name (`db-prod-03`, `db-prod-03!postgres-replication`) or `tab:<name>`; `list:downtimes`, `list:comments`, `list:acknowledged` open that list (topic 07). |
| `ICYGUI_DEMO_STORM=20` | Starts the simulator's problem storm (24 services fail at once) every 20 seconds instead of every five minutes: a few desktop notifications, the rest silent, then one summary, for the notification centre. `staging` and `lab` storm three and six times less often (every 15 and 30 minutes without it). |
| `ICYGUI_DEMO_ENVIRONMENTS=1` | How many demo environments run (1 to 11, default 3): `1` shows the app with a single environment (no scope tabs in the notification centre), more than 3 add environments serving `lab` (`dev-cluster`, `qa`, `edge-ams`, …), for the switcher and the centre's `···` overflow. |
| `ICYGUI_DEMO_APPEARANCE=light,compact,clock` | Starts with these appearance settings, comma separated: a theme (`system`, `dark`, `light`), an interface size (`small`, `default`, `large`), a row density (`comfortable`, `compact`) and times in lists (`relative`, `clock`); the rest keep their defaults. The demo saves nothing, so the panel's changes last until it quits. Under Xvfb there is no desktop colour scheme: *follow system* is light there. |
| `ICYGUI_WINDOW_CONTROLS=always` | Draws the window's own close/minimise/maximise buttons (`never`, `auto`): on Linux the default depends on the desktop; screenshots under Xvfb need `always`. |
| `GPUI_X11_SCALE_FACTOR=2` | GPUI's own switch (X11): renders at that scale whatever the display reports; `cargo xtask screenshots` uses 2. |

Actions: `a` acknowledge, `d` downtime, `r` check now and `c` comment act on the marked rows (`x`, shift-click, ctrl/cmd-click, `ctrl-a` / `⌘A`), else on the pane's or the cursor's object; the pane's `···` and the palette also submit passive check results, run commands (after a confirmation), remove downtimes and copy names, filter expressions and output. In the dialogs Tab moves between fields, Enter sends (Shift-Enter for a new line), Escape closes. `--demo` runs every action against its simulated Icinga; against the disposable Docker Icinga only look around (no actions). Results show as toasts in the bottom-right corner; every action is logged at `info` level.

The UI tests in `crates/ic-app/src/ui_tests/` run the real window on GPUI's headless platform (Linux only): keystrokes, clicks and snapshot updates on fixed data (`crates/ic-app/src/fixture/`), and the whole app against the demo's mock through the real core (`ui_tests/live.rs`), no display server needed. `crates/ic-app/tests/background.rs` (Linux) runs the built `icygui` on a private D-Bus session started with `dbus-run-session` (from `dbus`; the test skips with a message when it is missing) with a fake tray host and a fake notification server: the tray's tooltip, icon and menu, a desktop notification and its *Open* button, and a second launch handing over to the first. To see real notifications under Xvfb, run the demo inside `dbus-run-session` with `dunst` (as `spikes/linux-headless.sh` does). With the `ICYGUI_CONTRACT_*` variables of `contract/run-icinga.sh` set, `ui_tests::live::a_real_icinga_loads_read_only` also connects the app to the disposable Icinga (read-only; it refuses any other instance).

## Screenshots and clips (`cargo xtask screenshots`)

The README's images in `docs/screenshots/` are generated from `--demo` (ENV-11), so they stay current with the app. Regenerate them after a visible change and commit the result:

```sh
sudo apt-get install xvfb xdotool ffmpeg mesa-vulkan-drivers   # once
cargo xtask screenshots                        # every still and clip (about 3 minutes)
cargo xtask screenshots --no-gifs              # only the stills
cargo xtask screenshots --only keyboard,palette  # some scenes, by name
```

Linux only. It builds the debug app, starts its own Xvfb on a free display (`:90` and up), and runs `icygui --demo` once per scene:
- with `ICYGUI_DEMO_SEED=7`, `ICYGUI_WINDOW_CONTROLS=always`, `ICYGUI_DEMO_APPEARANCE=dark` (the README shows the dark theme; Xvfb has no desktop colour scheme) and the scene's `ICYGUI_DEMO_*` switches (above);
- with `GPUI_X11_SCALE_FACTOR=2`, so the stills are rendered at twice the size (2880×1800 for the default 1440×900 window) and stay sharp on high-density screens;
- with a private home, XDG directories and runtime directory under `target/screenshots/`, and without `DBUS_SESSION_BUS_ADDRESS`, so nothing reaches the desktop you run it on (no notifications, no tray icon, no keychain);
- drives it with `xdotool` (keys, typing, and two clicks at fixed window coordinates) and records it with `ffmpeg`'s `x11grab`.

Stills are one frame reduced to a 256-colour palette (`palettegen`/`paletteuse` without dithering): 90–180 KiB each. Clips are a lossless recording scaled to 1200 px wide at 10 fps and converted with a palette generated for that clip: 0.2–0.5 MiB each, and the command warns above 3 MiB. Everything together stays around 3 MiB. The work directory, `target/screenshots/`, is deleted afterwards; after a failure it stays, with each scene's `app.log` and Xvfb's log.

The scenes are a table in `xtask/src/screenshots.rs` (`SCENES`): a name (the file name), still or clip, the demo switches, and the steps (`Wait`, `Key`, `Type`, `Click`, `Record`). The demo's data is fixed by the seed, but times (`14m` in state, the clock) follow the wall clock, so two runs differ in those details only. A test (`cargo test -p xtask`) checks that every image the README and the user guide show is made by a scene and exists.

## UI conventions

- **The design rule.** Everything fits the design (`design/project/*.dc.html`): the theme's tokens (`ic_ui_kit::Theme`) and the kit's components, nothing invented beside them. Nothing changes size or position with the state it shows (a count, a status, a hover, a selection, an object's state, an action on its way): slots are fixed (`Button::width` for a button whose label changes, `Button::key_blank` for a key hint that doesn't apply just now), long text is cut short, a menu or card keeps its size while it is open, and a list doesn't move under the pointer (the notification centre holds arrivals while the pointer is over it).
- **Selection** is the selected-row background (`row_selected`), in lists, menus (`MenuItem::selected`: the environment on screen, the connected node) and chips (`Chip::filled`). Check marks only in menus that are lists of choices or toggles (sort, group by, notifications), where every item keeps the check column so the rows line up.
- **No trailing `…` on labels**: buttons, links, menu items, palette commands and dialog texts (*add environment*, *Settings*, *Review certificate*, *delete dashboard*), and the user guide names them the same way. An ellipsis appears only where text is actually cut short for lack of room (a name, an output line, an excerpt), on **progress text**, which says something is under way (`Loading services…`, `Connecting to master-01…`, `ack pending…`, `checking…`, `saving…`, `testing…`, `updating…`, a toast's `Acknowledging …`; the user accepted this, PLAN.md §4.1), in the search fields' placeholders the design shows (`Search dashboards…`), and as "and so on" inside example text (`e.g. AB:CD:…`).
- **Colours and lengths come from the theme**, so the light theme, the interface size and the row density reach every view: colours from `cx.theme()` (never a literal; a state colour as `states.fill` for shapes and `states.text` for words), lengths in design pixels with `ic_ui_kit::px` (scaled to the interface size; `gpui::px` is a disallowed method outside window geometry and test coordinates), a list row's state circle through `ListRow::state` (it follows the row density). Arithmetic against the window's size converts between window and design pixels with `ic_ui_kit::scale()`.
- **Marks in a fixed slot**: every palette row has one in the dot's place (a state dot, the several-objects stack, or a small muted Lucide icon); menu rows with actions keep a slot per action at their right (`ItemAction`), shown or not.

## Lints

Every crate inherits `[workspace.lints]` from the root `Cargo.toml`: `clippy::pedantic`, plus restriction lints such as no `unwrap`/`expect`/`panic` outside tests and no `println!`. CI treats every warning as an error.

`clippy.toml` adds one project rule: `gpui::px` is a disallowed method (lengths follow the interface size through `ic_ui_kit::px`); real pixels take an `#[expect(clippy::disallowed_methods, reason = "…")]`.

When a lint has to be silenced, use `#[expect(clippy::lint_name, reason = "why")]`. `#[allow]` is itself linted, and an `expect` fails once it's no longer needed.

## Layout

See `PLAN.md` §3 for the crate structure and the dependency rules between crates. In short:

- the UI (`ic-app`, `ic-ui-kit`) never talks HTTP,
- the core (`ic-core` and below) never imports GPUI,
- `ic-model`, `ic-filter`, `ic-rules` and `ic-config` stay free of I/O and async.

## Spikes

`spikes/` holds the M0 experiments; results are in `docs/spikes.md`. `spikes/linux-headless.sh` runs the background-mode spike under Xvfb with a private D-Bus session and dunst. CI runs it on every push.
