//! App bundles: macOS `.app` (universal binary, Developer ID or ad-hoc
//! signature with the hardened runtime) and the Linux install tree.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{
    APP_ID, APP_NAME, BINARY, Flags, Result, VERSION, cargo, copy, create_dir, remove_dir, root,
    run, target_dir, write,
};

const MACOS_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

/// Builds and bundles the app; returns the `.app` (macOS) or the install
/// tree root (Linux).
pub(crate) fn bundle(flags: &Flags) -> Result<PathBuf> {
    let binary = if flags.universal {
        build_universal()?
    } else {
        build(flags.release, None)?
    };
    let out = target_dir().join("bundle");
    if cfg!(target_os = "macos") {
        bundle_macos(&binary, &out, flags.sign.as_deref())
    } else {
        if flags.universal || flags.sign.is_some() {
            return Err("--universal and --sign are macOS-only".to_owned());
        }
        bundle_linux(&binary, &out)
    }
}

fn build(release: bool, target: Option<&str>) -> Result<PathBuf> {
    let mut command = cargo();
    command
        .current_dir(root())
        .args(["build", "--locked", "-p", "ic-app"]);
    if release {
        command.arg("--release");
    }
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    run(&mut command)?;
    let mut dir = target_dir();
    if let Some(target) = target {
        dir = dir.join(target);
    }
    Ok(dir
        .join(if release { "release" } else { "debug" })
        .join(BINARY))
}

/// Release builds for Apple silicon and Intel, merged with `lipo`.
fn build_universal() -> Result<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Err("--universal is macOS-only".to_owned());
    }
    let mut slices = Vec::new();
    for target in MACOS_TARGETS {
        slices.push(build(true, Some(target))?);
    }
    let out = target_dir().join("universal/release").join(BINARY);
    create_dir(out.parent().unwrap_or(&out))?;
    run(Command::new("lipo")
        .arg("-create")
        .arg("-output")
        .arg(&out)
        .args(&slices))?;
    Ok(out)
}

fn bundle_macos(binary: &Path, out: &Path, identity: Option<&str>) -> Result<PathBuf> {
    let app = out.join(format!("{APP_NAME}.app"));
    remove_dir(&app)?;
    let contents = app.join("Contents");
    copy(binary, &contents.join("MacOS").join(BINARY))?;
    copy(
        &root().join(format!("assets/icons/{APP_NAME}.icns")),
        &contents.join("Resources").join(format!("{APP_NAME}.icns")),
    )?;
    write(&contents.join("Info.plist"), info_plist())?;
    write(&contents.join("PkgInfo"), "APPL????")?;
    sign_app(&app, identity)?;
    println!("bundle: {}", app.display());
    Ok(app)
}

/// Signs the executable, then the bundle, with the hardened runtime.
/// Without an identity the signature is ad hoc (local use only).
fn sign_app(app: &Path, identity: Option<&str>) -> Result<()> {
    let entitlements = root().join("packaging/macos/entitlements.plist");
    let sign = |path: &Path| {
        let mut command = Command::new("codesign");
        command
            .args(["--force", "--options", "runtime", "--entitlements"])
            .arg(&entitlements);
        match identity {
            Some(identity) => command.args(["--timestamp", "--sign", identity]),
            None => command.args(["--sign", "-"]),
        };
        run(command.arg(path))
    };
    sign(&app.join("Contents/MacOS").join(BINARY))?;
    sign(app)?;
    run(Command::new("codesign")
        .args(["--verify", "--deep", "--strict", "--verbose=2"])
        .arg(app))
}

pub(crate) fn info_plist() -> String {
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
  <key>NSHumanReadableCopyright</key><string>© Alexander Knott. MIT licence.</string>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
</dict>
</plist>
"#
    )
}

pub(crate) fn desktop_entry() -> String {
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
    remove_dir(&tree)?;
    copy(binary, &tree.join("bin").join(BINARY))?;
    write(
        &tree.join(format!("share/applications/{APP_ID}.desktop")),
        desktop_entry(),
    )?;
    for size in [16, 32, 64, 128, 256, 512] {
        copy(
            &root().join(format!("assets/icons/{APP_NAME}-{size}.png")),
            &tree.join(format!(
                "share/icons/hicolor/{size}x{size}/apps/{APP_ID}.png"
            )),
        )?;
    }
    copy(
        &root().join("assets/logo/icygui.svg"),
        &tree.join(format!("share/icons/hicolor/scalable/apps/{APP_ID}.svg")),
    )?;
    copy(
        &root().join("LICENSE"),
        &tree.join(format!("share/doc/{APP_NAME}/LICENSE")),
    )?;
    println!("bundle: {}", tree.display());
    Ok(tree)
}
