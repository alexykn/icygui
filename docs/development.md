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
```

The first build compiles GPUI and takes a few minutes. Dependencies are built with `opt-level = 2` even in dev builds, so the UI stays smooth.

## Tests against Icinga, and load

- **Against a real Icinga:** only the disposable one from `contract/run-icinga.sh`, through `cargo test -p ic-api --test contract` (see `contract/README.md`). The contract tests refuse any other instance before sending a query: the URL must point to this machine and the fixture-only `viewer` user must log in.
- **Load and scale tests** run only against `ic-mock` (its `large` scenario and `MockControl::burst`) or `contract/scale/benchmark.sh`, which starts its own Icinga in Docker on localhost. Never point a load test at a production Icinga.
- **Against a production Icinga,** the only test is normal use of the app: connect and look around, which costs one lean load like an Icinga Web session.

## Running the app

- `cargo run -p ic-app` connects to the active environment of the settings file (the first one if none is marked). Without environments the window shows the onboarding form: name, URL, login, TLS, *test connection* and *connect*. More environments are added, edited and switched from the connection status in the sidebar's footer (or the command palette, `ctrl-k` / `⌘K`). Passwords go to the system keychain (Secret Service on Linux, Keychain on macOS); without one, saving a password login says so and keeps the form open.
- `cargo run -p ic-app -- --demo` runs the whole app against a simulated Icinga in the same process: an `ic_mock::MockServer` with the `prod-cluster` scenario (the design's sample data in a 150-host estate) and its simulator running (checks at their intervals, new problems, recoveries, flapping, outages, downtimes, a problem storm every five minutes), through the real `ic-core`. Actions work against it. The demo also has a `staging` and a `lab` environment to switch to (their scenarios start on first use). Nothing is saved, the keychain isn't touched, and the event logs live in a temporary directory.
- Dashboards: the group header's `+` and `···`, a dashboard row's `···` or right click, and the footer's `+` hold the commands (new, rename, duplicate, move, mute, delete, import, export). `ctrl-n` / `⌘N` creates a dashboard; the editor previews the filter live, `ctrl-s` / `⌘S` saves and Escape discards. Without a desktop file chooser (no xdg-desktop-portal), import and export ask for the file's path in a small dialog.
- `--version` and `--help` print and exit without opening a window.

Files (from `ic_config::Paths`): the settings in `config.toml` (with `config.toml.bak`), the window size and position, open tabs and selected dashboards in `state.toml` in the data directory, the event logs (`events-<environment id>.sqlite3`) in the data directory, and the log in `icygui.log` (rotated at 10 MiB, four old files kept) in the log directory. A settings file that can't be read is never replaced silently: the window offers to restore the backup, start fresh (the broken file is kept as `config.toml.unreadable-<time>`), try again, or quit.

Environment variables for development (the `ICYGUI_DEMO_*` ones only affect `--demo`):

| Variable | Effect |
| --- | --- |
| `RUST_LOG=icygui=debug,ic_core=debug` | The log filter (default `info`). |
| `ICYGUI_DEMO_SCENARIO=large` | Serves another `ic_mock` scenario: `prod-cluster` (default), `staging`, `lab`, or `large` (production scale: 2 000 hosts, 30 000 services), to check that loading and scrolling stay smooth. |
| `ICYGUI_DEMO_SEED=7` | Fixes the simulator's seed (the default changes every run), for reproducible screenshots. |
| `ICYGUI_DEMO_FAULT=auth` | Shows a connection failure on purpose: `offline` (reconnecting with a countdown), `auth` (login refused), `tls` (certificate not trusted), `missing-secret`, `misconfigured`, `outage` (the connection is lost 20 s in), `slow` (every answer takes 0.9 s, so the load's progress shows) or `frozen` (the simulated Icinga stops checking, so checks are marked late after about two minutes). |
| `ICYGUI_DEMO_DASHBOARD=databases` | Selects a dashboard by name at start. |
| `ICYGUI_DEMO_OPEN=service` | Opens an object once it is loaded: `service` (postgres-replication beside the list, screen 2b), `host` (its host db-prod-03, screen 2c), `tab` (postgres-replication as a tab), an object name (`db-prod-03`, `db-prod-03!postgres-replication`) or `tab:<name>`. |
| `ICYGUI_WINDOW_CONTROLS=always` | Draws the window's own close/minimise/maximise buttons (`never`, `auto`): on Linux the default depends on the desktop; screenshots under Xvfb need `always`. |

Action buttons and their keys (acknowledge, downtime, check now, comment) are checked against the API user's permissions and logged at `info` level; their dialogs come with the actions.

The UI tests in `crates/ic-app/src/ui_tests/` run the real window on GPUI's headless platform (Linux only): keystrokes, clicks and snapshot updates on fixed data (`crates/ic-app/src/fixture/`), and the whole app against the demo's mock through the real core (`ui_tests/live.rs`), no display server needed. With the `ICYGUI_CONTRACT_*` variables of `contract/run-icinga.sh` set, `ui_tests::live::a_real_icinga_loads_read_only` also connects the app to the disposable Icinga (read-only; it refuses any other instance).

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
