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

- **Against a real Icinga:** only the disposable one from `contract/run-icinga.sh`, through `cargo test -p ic-api --test contract` (see `contract/README.md`). The contract tests refuse any other instance before sending a query: the URL must point to this machine and the fixture-only `viewer` user must log in.
- **Load and scale tests** run only against `ic-mock` (its `large` scenario and `MockControl::burst`) or `contract/scale/benchmark.sh`, which starts its own Icinga in Docker on localhost. Never point a load test at a production Icinga. The production-size tests are `#[ignore]`d; `.github/workflows/perf.yml` runs them nightly with `--release`, where they assert docs/performance.md's budgets (its steps are the commands to run them yourself).
- **Against a production Icinga,** the only test is normal use of the app: connect and look around, which costs one lean load like an Icinga Web session.

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
| `RUST_LOG=icygui=debug,ic_core=debug` | The log filter (default `info`). |
| `ICYGUI_DEMO_SCENARIO=large` | Serves another `ic_mock` scenario: `prod-cluster` (default), `staging`, `lab`, or `large` (production scale: 2 000 hosts, 30 000 services), to check that loading and scrolling stay smooth. |
| `ICYGUI_DEMO_SEED=7` | Fixes the simulator's seed (the default changes every run), for reproducible screenshots. |
| `ICYGUI_DEMO_FAULT=auth` | Shows a connection failure on purpose: `offline` (reconnecting with a countdown), `auth` (login refused), `tls` (certificate not trusted), `pin-mismatch` (another certificate is pinned: both fingerprints show), `missing-secret`, `misconfigured`, `outage` (the connection is lost 20 s in), `slow` (every answer takes 0.9 s, so the load's progress shows) `frozen` (the simulated Icinga stops checking, so checks are marked late after about two minutes) or `partial` (the master answers 503, so the engine connects to the satellite `sat-ams-01` that `prod-cluster` lists as its second URL: a labelled partial view, ENV-12). Without a fault, or with `partial`, `slow` or `frozen`, `prod-cluster` lists its master and its satellite, each served by its own mock; the other faults list the master alone. |
| `ICYGUI_DEMO_DASHBOARD=databases` | Selects a dashboard by name at start. |
| `ICYGUI_DEMO_OPEN=service` | Opens an object once it is loaded: `service` (postgres-replication beside the list, screen 2b), `host` (its host db-prod-03, screen 2c), `tab` (postgres-replication as a tab), an object name (`db-prod-03`, `db-prod-03!postgres-replication`) or `tab:<name>`. |
| `ICYGUI_DEMO_STORM=20` | Starts the simulator's problem storm (24 services fail at once) every 20 seconds instead of every five minutes: a few desktop notifications, the rest silent, then one summary, for the notification centre. `staging` and `lab` storm three and six times less often (every 15 and 30 minutes without it). |
| `ICYGUI_DEMO_ENVIRONMENTS=1` | How many demo environments run (1 to 11, default 3): `1` shows the app with a single environment (no scope tabs in the notification centre), more than 3 add environments serving `lab` (`dev-cluster`, `qa`, `edge-ams`, …), for the switcher and the centre's `···` overflow. |
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
- with `ICYGUI_DEMO_SEED=7`, `ICYGUI_WINDOW_CONTROLS=always` and the scene's `ICYGUI_DEMO_*` switches (above);
- with `GPUI_X11_SCALE_FACTOR=2`, so the stills are rendered at twice the size (2880×1800 for the default 1440×900 window) and stay sharp on high-density screens;
- with a private home, XDG directories and runtime directory under `target/screenshots/`, and without `DBUS_SESSION_BUS_ADDRESS`, so nothing reaches the desktop you run it on (no notifications, no tray icon, no keychain);
- drives it with `xdotool` (keys, typing, and two clicks at fixed window coordinates) and records it with `ffmpeg`'s `x11grab`.

Stills are one frame reduced to a 256-colour palette (`palettegen`/`paletteuse` without dithering): 90–180 KiB each. Clips are a lossless recording scaled to 1200 px wide at 10 fps and converted with a palette generated for that clip: 0.2–0.5 MiB each, and the command warns above 3 MiB. Everything together stays around 3 MiB. The work directory, `target/screenshots/`, is deleted afterwards; after a failure it stays, with each scene's `app.log` and Xvfb's log.

The scenes are a table in `xtask/src/screenshots.rs` (`SCENES`): a name (the file name), still or clip, the demo switches, and the steps (`Wait`, `Key`, `Type`, `Click`, `Record`). The demo's data is fixed by the seed, but times (`14m` in state, the clock) follow the wall clock, so two runs differ in those details only. A test (`cargo test -p xtask`) checks that every image the README and the user guide show is made by a scene and exists.

## Lints

Every crate inherits `[workspace.lints]` from the root `Cargo.toml`: `clippy::pedantic`, plus restriction lints such as no `unwrap`/`expect`/`panic` outside tests and no `println!`. CI treats every warning as an error.

When a lint has to be silenced, use `#[expect(clippy::lint_name, reason = "why")]`. `#[allow]` is itself linted, and an `expect` fails once it's no longer needed.

## Layout

See `PLAN.md` §3 for the crate structure and the dependency rules between crates. In short:

- the UI (`ic-app`, `ic-ui-kit`) never talks HTTP,
- the core (`ic-core` and below) never imports GPUI,
- `ic-model`, `ic-filter`, `ic-rules` and `ic-config` stay free of I/O and async.

## Spikes

`spikes/` holds the M0 experiments; results are in `docs/spikes.md`. `spikes/linux-headless.sh` runs the background-mode spike under Xvfb with a private D-Bus session and dunst. CI runs it on every push.
