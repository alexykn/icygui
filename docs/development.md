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
