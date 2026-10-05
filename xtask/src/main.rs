//! Project automation (`cargo xtask <task>`).
//!
//! - `icons`: regenerate the app icon PNGs and `.icns` in `assets/icons/`
//! - `bundle [--release]`: macOS `.app` (ad-hoc signed) or a Linux install tree
//! - `package`: release bundle plus `.dmg` (macOS) or `.tar.gz` + `.deb` (Linux)
//! - `install`: Linux only, installs the bundle into `~/.local`
//! - `mock [args…]`: start the mock Icinga environments (`icinga-mock`)

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "xtask is a command-line tool that reports progress"
)]

mod icon;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const APP_ID: &str = "io.github.alexykn.icygui";
const APP_NAME: &str = "icygui";
const BINARY: &str = "icygui";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const ICON_SIZES: [u32; 7] = [16, 32, 64, 128, 256, 512, 1024];

type Result<T, E = String> = std::result::Result<T, E>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("icons") => icons(),
        Some("bundle") => bundle(args.iter().any(|a| a == "--release")).map(|_| ()),
        Some("package") => package(),
        Some("install") => install(),
        Some("mock") => mock(&args[1..]),
        _ => {
            eprintln!(
                "usage: cargo xtask <icons | bundle [--release] | package | install | mock [args…]>"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn run(command: &mut Command) -> Result<()> {
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

fn create_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|error| format!("creating {}: {error}", path.display()))
}

fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent() {
        create_dir(parent)?;
    }
    fs::write(path, contents).map_err(|error| format!("writing {}: {error}", path.display()))
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        create_dir(parent)?;
    }
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|error| format!("copying {} to {}: {error}", from.display(), to.display()))
}

fn icons() -> Result<()> {
    let dir = root().join("assets/icons");
    create_dir(&dir)?;
    for size in ICON_SIZES {
        let path = dir.join(format!("{APP_NAME}-{size}.png"));
        write(&path, icon::png(size)?)?;
        println!("wrote {}", path.display());
    }
    let icns = dir.join(format!("{APP_NAME}.icns"));
    write(&icns, icon::icns()?)?;
    println!("wrote {}", icns.display());
    Ok(())
}

fn build(release: bool) -> Result<PathBuf> {
    let mut command = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    command
        .current_dir(root())
        .args(["build", "--locked", "-p", "ic-app"]);
    if release {
        command.arg("--release");
    }
    run(&mut command)?;
    let target_dir =
        env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), PathBuf::from);
    Ok(target_dir
        .join(if release { "release" } else { "debug" })
        .join(BINARY))
}

fn bundle(release: bool) -> Result<PathBuf> {
    let binary = build(release)?;
    let out = root().join("target/bundle");
    if cfg!(target_os = "macos") {
        bundle_macos(&binary, &out)
    } else {
        bundle_linux(&binary, &out)
    }
}

fn bundle_macos(binary: &Path, out: &Path) -> Result<PathBuf> {
    let app = out.join(format!("{APP_NAME}.app"));
    if app.exists() {
        fs::remove_dir_all(&app).map_err(|error| format!("removing old bundle: {error}"))?;
    }
    let contents = app.join("Contents");
    copy(binary, &contents.join("MacOS").join(BINARY))?;
    copy(
        &root().join(format!("assets/icons/{APP_NAME}.icns")),
        &contents.join("Resources").join(format!("{APP_NAME}.icns")),
    )?;
    write(&contents.join("Info.plist"), info_plist())?;
    write(&contents.join("PkgInfo"), "APPL????")?;
    // Ad-hoc signature: enough for local use and for notification permission.
    run(Command::new("codesign")
        .args(["--force", "--deep", "--sign", "-"])
        .arg(&app))?;
    println!("bundle: {}", app.display());
    Ok(app)
}

fn info_plist() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>{APP_NAME}</string>
  <key>CFBundleExecutable</key><string>{BINARY}</string>
  <key>CFBundleIconFile</key><string>{APP_NAME}.icns</string>
  <key>CFBundleIdentifier</key><string>{APP_ID}</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>{APP_NAME}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{VERSION}</string>
  <key>CFBundleVersion</key><string>{VERSION}</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
"#
    )
}

fn desktop_entry() -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={APP_NAME}\n\
         GenericName=Icinga 2 client\n\
         Comment=Monitor Icinga 2 environments\n\
         Exec={BINARY}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Network;Monitor;System;\n\
         Keywords=icinga;monitoring;alerts;\n\
         StartupWMClass={APP_ID}\n"
    )
}

fn bundle_linux(binary: &Path, out: &Path) -> Result<PathBuf> {
    let tree = out.join(APP_NAME);
    if tree.exists() {
        fs::remove_dir_all(&tree).map_err(|error| format!("removing old bundle: {error}"))?;
    }
    copy(binary, &tree.join("bin").join(BINARY))?;
    write(
        &tree.join(format!("share/applications/{APP_ID}.desktop")),
        desktop_entry(),
    )?;
    for size in ICON_SIZES.into_iter().filter(|size| *size <= 512) {
        copy(
            &root().join(format!("assets/icons/{APP_NAME}-{size}.png")),
            &tree.join(format!(
                "share/icons/hicolor/{size}x{size}/apps/{APP_ID}.png"
            )),
        )?;
    }
    println!("bundle: {}", tree.display());
    Ok(tree)
}

fn package() -> Result<()> {
    let bundle = bundle(true)?;
    let dist = root().join("target/dist");
    create_dir(&dist)?;
    let arch = env::consts::ARCH;
    if cfg!(target_os = "macos") {
        let dmg = dist.join(format!("{APP_NAME}-{VERSION}-macos-{arch}.dmg"));
        run(Command::new("hdiutil")
            .args([
                "create",
                "-volname",
                APP_NAME,
                "-ov",
                "-format",
                "UDZO",
                "-srcfolder",
            ])
            .arg(&bundle)
            .arg(&dmg))?;
        println!("package: {}", dmg.display());
        return Ok(());
    }
    let tarball = dist.join(format!("{APP_NAME}-{VERSION}-linux-{arch}.tar.gz"));
    run(Command::new("tar")
        .arg("-czf")
        .arg(&tarball)
        .arg("-C")
        .arg(bundle.parent().unwrap_or(&bundle))
        .arg(APP_NAME))?;
    println!("package: {}", tarball.display());
    deb(&bundle, &dist)
}

fn deb(bundle: &Path, dist: &Path) -> Result<()> {
    let deb_arch = match env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let staging = root().join("target/deb").join(APP_NAME);
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|error| format!("removing old staging: {error}"))?;
    }
    let usr = staging.join("usr");
    copy(
        &bundle.join("bin").join(BINARY),
        &usr.join("bin").join(BINARY),
    )?;
    copy_tree(&bundle.join("share"), &usr.join("share"))?;
    write(
        &staging.join("DEBIAN/control"),
        format!(
            "Package: {APP_NAME}\n\
             Version: {VERSION}\n\
             Section: net\n\
             Priority: optional\n\
             Architecture: {deb_arch}\n\
             Depends: libxkbcommon0, libxkbcommon-x11-0, libxcb1, libfontconfig1, libfreetype6, libvulkan1, libwayland-client0\n\
             Recommends: mesa-vulkan-drivers, gnome-shell-extension-appindicator | plasma-workspace\n\
             Maintainer: Alexander Knott\n\
             Description: Native desktop client for Icinga 2\n \
             Live problem lists, dashboards, operator actions and native notifications\n \
             for the Icinga 2 REST API.\n"
        ),
    )?;
    let deb = dist.join(format!("{APP_NAME}_{VERSION}_{deb_arch}.deb"));
    run(Command::new("dpkg-deb")
        .args(["--root-owner-group", "--build"])
        .arg(&staging)
        .arg(&deb))?;
    println!("package: {}", deb.display());
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
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

fn install() -> Result<()> {
    if cfg!(target_os = "macos") {
        return Err(
            "on macOS, run `cargo xtask bundle --release` and drag the .app into /Applications"
                .to_owned(),
        );
    }
    let bundle = bundle(true)?;
    let home = env::var_os("HOME").ok_or("HOME is not set")?;
    let prefix = PathBuf::from(home).join(".local");
    copy(
        &bundle.join("bin").join(BINARY),
        &prefix.join("bin").join(BINARY),
    )?;
    copy_tree(&bundle.join("share"), &prefix.join("share"))?;
    println!("installed into {}", prefix.display());
    Ok(())
}

fn mock(extra: &[String]) -> Result<()> {
    let mut command = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
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
