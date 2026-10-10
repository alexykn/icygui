//! Project automation (`cargo xtask <task>`).
//!
//! - `icons`: render the logo SVGs into icon PNGs, the `.icns` and the banner
//! - `render <svg> <png> <size>`: render one SVG to a square PNG
//! - `bundle [--release] [--universal] [--sign IDENTITY]`: macOS `.app` or a
//!   Linux install tree in `target/bundle`
//! - `package [--universal] [--sign IDENTITY] [--notarize] [--prebuilt]`:
//!   release artifacts in `target/dist` (macOS `.zip` + `.dmg`; Linux
//!   `.tar.gz` + `.deb`) plus `SHA256SUMS`; with `--prebuilt`, from the
//!   bundle `bundle --release` left, without building (the release
//!   workflow signs where nothing compiles)
//! - `notarize <path>`: notarize and staple an `.app` or `.dmg`
//! - `homebrew --dist DIR --out TAP_DIR [--repo OWNER/NAME] [--notarized]`:
//!   write the Homebrew cask (macOS) and formula (Linux) for the artifacts
//!   in `DIR`; without `--notarized` the cask clears the quarantine flag
//! - `version`: print the workspace version (CI compares it with the tag)
//! - `install`: Linux only, install into `~/.local`
//! - `mock [args…]`: start the mock Icinga environments (`icinga-mock`)
//! - `screenshots [--only NAME,…] [--no-gifs]`: Linux only, the README's
//!   screenshots and clips from `icygui --demo` under Xvfb, into
//!   `docs/screenshots`
//! - `demo-config [--config-dir DIR] [--ca FILE] …`:
//!   write the demo cluster's environment and dashboards (`demo/icygui`)
//!   into icygui's settings ([`demo`])
//!
//! Signing and notarization read their secrets from the environment; see
//! `docs/releasing.md`.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "xtask is a command-line tool that reports progress"
)]

mod bundle;
mod demo;
mod homebrew;
mod icon;
mod release;
mod screenshots;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub(crate) const APP_ID: &str = "io.github.alexykn.icygui";
pub(crate) const APP_NAME: &str = "icygui";
pub(crate) const BINARY: &str = "icygui";
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const DEFAULT_REPO: &str = "alexykn/icygui";
pub(crate) const ICON_SIZES: [u32; 7] = [16, 32, 64, 128, 256, 512, 1024];

pub(crate) type Result<T, E = String> = std::result::Result<T, E>;

/// Flags shared by the commands. Unknown flags are errors.
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per command-line switch"
)]
pub(crate) struct Flags {
    pub(crate) release: bool,
    pub(crate) universal: bool,
    pub(crate) sign: Option<String>,
    pub(crate) notarize: bool,
    /// `package` from the bundle in `target/bundle`, building nothing.
    pub(crate) prebuilt: bool,
    pub(crate) dist: Option<PathBuf>,
    pub(crate) out: Option<PathBuf>,
    pub(crate) repo: Option<String>,
    pub(crate) notarized: bool,
    pub(crate) only: Option<String>,
    pub(crate) no_gifs: bool,
    pub(crate) positional: Vec<String>,
}

fn parse_flags(args: &[String]) -> Result<Flags> {
    let mut flags = Flags::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--release" => flags.release = true,
            "--universal" => flags.universal = true,
            "--notarize" => flags.notarize = true,
            "--notarized" => flags.notarized = true,
            "--prebuilt" => flags.prebuilt = true,
            "--sign" => flags.sign = Some(value("--sign")?),
            "--dist" => flags.dist = Some(PathBuf::from(value("--dist")?)),
            "--out" => flags.out = Some(PathBuf::from(value("--out")?)),
            "--repo" => flags.repo = Some(value("--repo")?),
            "--only" => flags.only = Some(value("--only")?),
            "--no-gifs" => flags.no_gifs = true,
            other if other.starts_with("--") => return Err(format!("unknown flag {other}")),
            other => flags.positional.push(other.to_owned()),
        }
    }
    // An empty identity (an unset CI secret) means "not signing".
    if flags.sign.as_deref().is_some_and(str::is_empty) {
        flags.sign = None;
    }
    Ok(flags)
}

const USAGE: &str = "usage: cargo xtask <command>
  icons                                   render logo, icons, icns, banner
  render <svg> <png> <size>               render one SVG
  bundle [--release] [--universal] [--sign IDENTITY]
  package [--universal] [--sign IDENTITY] [--notarize] [--prebuilt]
  notarize <app-or-dmg>
  homebrew --dist DIR --out TAP_DIR [--repo OWNER/NAME] [--notarized]
  version
  install                                 (Linux) install into ~/.local
  mock [args…]                            start mock Icinga environments
  screenshots [--only NAME,…] [--no-gifs] (Linux) README images from --demo
  demo-config [--config-dir DIR] [--data-dir DIR] [--ca FILE] [--via-proxy]
              [--select GROUP/DASHBOARD] [--theme dark|light|system]
                                          the demo cluster's environment and dashboards";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((command, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if command == "demo-config" {
        // Its own arguments: the shared flags don't apply.
        return match demo::demo_config(&root(), rest) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let result = parse_flags(rest).and_then(|flags| match command.as_str() {
        "icons" => icons(),
        "render" => render_command(&flags.positional),
        "bundle" => bundle::bundle(&flags).map(|_| ()),
        "package" => release::package(&flags),
        "notarize" => match flags.positional.as_slice() {
            [path] => release::notarize(Path::new(path)),
            _ => Err("usage: cargo xtask notarize <app-or-dmg>".to_owned()),
        },
        "homebrew" => homebrew::write_tap(&flags),
        "version" => {
            println!("{VERSION}");
            Ok(())
        }
        "install" => install(),
        "mock" => mock(rest),
        "screenshots" => screenshots::screenshots(&flags),
        _ => Err(format!("unknown command {command}\n{USAGE}")),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn render_command(args: &[String]) -> Result<()> {
    let [svg, png, size] = args else {
        return Err("usage: cargo xtask render <in.svg> <out.png> <size>".to_owned());
    };
    let size: u32 = size.parse().map_err(|_| format!("bad size: {size}"))?;
    let source = fs::read(svg).map_err(|error| format!("reading {svg}: {error}"))?;
    write(Path::new(png), icon::render_svg(&source, size)?)
}

pub(crate) fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

pub(crate) fn target_dir() -> PathBuf {
    env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), PathBuf::from)
}

pub(crate) fn cargo() -> Command {
    Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()))
}

pub(crate) fn run(command: &mut Command) -> Result<()> {
    eprintln!("» {command:?}");
    let status = command
        .status()
        .map_err(|error| format!("failed to start {command:?}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{command:?} failed with {status}"))
    }
}

pub(crate) fn create_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|error| format!("creating {}: {error}", path.display()))
}

pub(crate) fn remove_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path)
            .map_err(|error| format!("removing {}: {error}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }
    fs::write(path, contents).map_err(|error| format!("writing {}: {error}", path.display()))
}

pub(crate) fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        create_dir(parent)?;
    }
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|error| format!("copying {} to {}: {error}", from.display(), to.display()))
}

pub(crate) fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    let entries =
        fs::read_dir(from).map_err(|error| format!("reading {}: {error}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("reading {}: {error}", from.display()))?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            copy(&entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Gives a package tree the usual modes whatever the umask: directories
/// 0755, executables 0755, other files 0644 (dpkg refuses a control
/// directory that isn't 0755, and a 0700 directory would lock users out).
#[cfg(unix)]
pub(crate) fn normalise_modes(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = fs::symlink_metadata(path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    let mode = if meta.is_dir() || meta.permissions().mode() & 0o111 != 0 {
        0o755
    } else {
        0o644
    };
    if !meta.file_type().is_symlink() {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("setting the mode of {}: {error}", path.display()))?;
    }
    if meta.is_dir() {
        let entries =
            fs::read_dir(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
        for entry in entries {
            let entry = entry.map_err(|error| format!("reading {}: {error}", path.display()))?;
            normalise_modes(&entry.path())?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn normalise_modes(_path: &Path) -> Result<()> {
    Ok(())
}

fn icons() -> Result<()> {
    let root = root();
    let svg_path = root.join(icon::ICON_SVG);
    let svg =
        fs::read(&svg_path).map_err(|error| format!("reading {}: {error}", svg_path.display()))?;
    let dir = root.join("assets/icons");
    create_dir(&dir)?;
    for size in ICON_SIZES {
        let path = dir.join(format!("{APP_NAME}-{size}.png"));
        write(&path, icon::render_svg(&svg, size)?)?;
        println!("wrote {}", path.display());
    }
    let icns = dir.join(format!("{APP_NAME}.icns"));
    write(&icns, icon::icns(&svg)?)?;
    println!("wrote {}", icns.display());

    let banner_svg = root.join("assets/logo/banner.svg");
    let banner = fs::read(&banner_svg)
        .map_err(|error| format!("reading {}: {error}", banner_svg.display()))?;
    let banner_png = root.join("assets/logo/banner.png");
    write(
        &banner_png,
        icon::render_svg_wide(&banner, 1280, &root.join("crates/ic-ui-kit/fonts"))?,
    )?;
    println!("wrote {}", banner_png.display());
    Ok(())
}

fn install() -> Result<()> {
    if cfg!(target_os = "macos") {
        return Err(
            "on macOS, install with Homebrew (`brew install --cask alexykn/tap/icygui`) or run \
             `cargo xtask bundle --release` and move target/bundle/icygui.app to /Applications"
                .to_owned(),
        );
    }
    let bundle = bundle::bundle(&Flags {
        release: true,
        ..Flags::default()
    })?;
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    let prefix = PathBuf::from(home).join(".local");
    let binary = prefix.join("bin").join(BINARY);
    copy(&bundle.join("bin").join(BINARY), &binary)?;
    let share = prefix.join("share");
    copy_tree(&bundle.join("share"), &share)?;
    // Desktop sessions often don't have ~/.local/bin on PATH: the menu
    // entry names the binary by its absolute path (as install.sh does).
    write(
        &share.join(format!("applications/{APP_ID}.desktop")),
        bundle::desktop_entry(&bundle::exec_path(&binary)?),
    )?;
    // Best effort, as in install.sh: menus and icon themes pick the files
    // up sooner where these tools exist.
    let _ = Command::new("update-desktop-database")
        .arg(share.join("applications"))
        .status();
    let _ = Command::new("gtk-update-icon-cache")
        .args(["-q", "-t"])
        .arg(share.join("icons/hicolor"))
        .status();
    println!("installed into {}", prefix.display());
    Ok(())
}

fn mock(extra: &[String]) -> Result<()> {
    let mut command = cargo();
    command
        .current_dir(root())
        .args(["run", "-p", "ic-mock", "--bin", "icinga-mock", "--"]);
    if extra.is_empty() {
        command.args([
            "--env",
            "prod-cluster:5665",
            "--env",
            "staging:5666",
            "--env",
            "lab:5667",
            "--tls",
            "ca",
            "--cert-dir",
            "target/mock-certs",
        ]);
    } else {
        command.args(extra);
    }
    run(&mut command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn parses_flags() {
        let flags = parse_flags(&args(&["--universal", "--sign", "Developer ID", "x"])).unwrap();
        assert!(flags.universal);
        assert_eq!(flags.sign.as_deref(), Some("Developer ID"));
        assert_eq!(flags.positional, ["x"]);
        assert!(parse_flags(&args(&["--nope"])).is_err());
        assert!(parse_flags(&args(&["--sign"])).is_err());
    }

    #[test]
    fn prebuilt_is_a_flag() {
        let flags = parse_flags(&args(&["--prebuilt", "--universal"])).unwrap();
        assert!(flags.prebuilt);
        assert!(flags.universal);
        assert!(!parse_flags(&args(&[])).unwrap().prebuilt);
    }

    #[test]
    fn empty_identity_means_unsigned() {
        assert_eq!(parse_flags(&args(&["--sign", ""])).unwrap().sign, None);
    }
}
