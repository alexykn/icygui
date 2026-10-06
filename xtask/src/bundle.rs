//! App bundles: macOS `.app` (universal binary, Developer ID or ad-hoc
//! signature with the hardened runtime) and the Linux install tree.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{
    APP_ID, APP_NAME, BINARY, Flags, Result, VERSION, cargo, copy, create_dir, normalise_modes,
    remove_dir, root, run, target_dir, write,
};

/// The licence and the notices for what the app bundles (the font, the
/// icons, the Rust crates), shipped with every bundle.
const LEGAL_FILES: [&str; 2] = ["LICENSE", "THIRD_PARTY_NOTICES.md"];

const MACOS_TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

/// The file next to the bundle that says how it was built (see [`stamp`]).
const STAMP: &str = "build-stamp";

/// Builds and bundles the app; returns the `.app` (macOS) or the install
/// tree root (Linux).
pub(crate) fn bundle(flags: &Flags) -> Result<PathBuf> {
    if !cfg!(target_os = "macos") && (flags.universal || flags.sign.is_some()) {
        return Err("--universal and --sign are macOS-only".to_owned());
    }
    let out = target_dir().join("bundle");
    // A failed build leaves no stamp vouching for an older bundle.
    let stamp_path = out.join(STAMP);
    match fs::remove_file(&stamp_path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(format!("removing {}: {error}", stamp_path.display()));
        }
        _ => {}
    }
    let binary = if flags.universal {
        build_universal()?
    } else {
        build(flags.release, None)?
    };
    check_version(&binary)?;
    let bundle = if cfg!(target_os = "macos") {
        bundle_macos(&binary, &out, flags.sign.as_deref())?
    } else {
        bundle_linux(&binary, &out)?
    };
    // `--universal` always builds release slices.
    write(
        &stamp_path,
        stamp(flags.release || flags.universal, flags.universal),
    )?;
    Ok(bundle)
}

/// How a bundle was built: `release`, `release universal` or `debug`.
fn stamp(release: bool, universal: bool) -> String {
    let profile = if release { "release" } else { "debug" };
    if universal {
        format!("{profile} universal\n")
    } else {
        format!("{profile}\n")
    }
}

/// The release bundle an earlier `bundle --release` left in
/// `target/bundle` (universal with `--universal`), re-signed with `--sign`'s
/// identity: `package --prebuilt` packages without building. The release
/// workflow builds in a step without secrets and signs in one that
/// compiles nothing, so no build script or proc macro of the dependency
/// tree runs where the signing identity and the notarization key are.
pub(crate) fn prebuilt(flags: &Flags) -> Result<PathBuf> {
    prebuilt_in(&target_dir().join("bundle"), flags)
}

/// [`prebuilt`] for the bundle directory `out`.
pub(crate) fn prebuilt_in(out: &Path, flags: &Flags) -> Result<PathBuf> {
    if !cfg!(target_os = "macos") && (flags.universal || flags.sign.is_some()) {
        return Err("--universal and --sign are macOS-only".to_owned());
    }
    let wanted = stamp(true, flags.universal);
    let found = fs::read_to_string(out.join(STAMP)).unwrap_or_default();
    if found != wanted {
        let found = match found.trim() {
            "" => "no finished build".to_owned(),
            other => format!("a {other} build"),
        };
        let universal = if flags.universal { " --universal" } else { "" };
        return Err(format!(
            "{} holds {found}, not a {}: run `cargo xtask bundle --release{universal}` first",
            out.display(),
            wanted.trim()
        ));
    }
    if cfg!(target_os = "macos") {
        let app = out.join(format!("{APP_NAME}.app"));
        check_version(&app.join("Contents/MacOS").join(BINARY))?;
        if let Some(identity) = flags.sign.as_deref() {
            sign_app(&app, Some(identity))?;
        }
        Ok(app)
    } else {
        let tree = out.join(APP_NAME);
        check_version(&tree.join("bin").join(BINARY))?;
        Ok(tree)
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

/// REL-05, REL-06: the binary answers `--version` without a display (CI and
/// the Homebrew formula test run it headless) and reports the workspace
/// version, which the release tag must match.
fn check_version(binary: &Path) -> Result<()> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|error| format!("running {} --version: {error}", binary.display()))?;
    let printed = String::from_utf8_lossy(&output.stdout);
    let expected = version_line();
    if output.status.success() && printed.trim_end() == expected {
        eprintln!("» {} --version: {expected}", binary.display());
        Ok(())
    } else {
        Err(format!(
            "{} --version printed {:?} ({}), expected {expected:?}",
            binary.display(),
            printed.trim_end(),
            output.status
        ))
    }
}

/// What `icygui --version` prints.
fn version_line() -> String {
    format!("{BINARY} {VERSION}")
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
    // REL-02: both slices made it into the universal binary.
    run(Command::new("lipo")
        .arg(&out)
        .args(["-verify_arch", "arm64", "x86_64"]))?;
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
    for notice in LEGAL_FILES {
        copy(
            &root().join(notice),
            &contents.join("Resources").join(notice),
        )?;
    }
    write(&contents.join("Info.plist"), info_plist())?;
    write(&contents.join("PkgInfo"), "APPL????")?;
    sign_app(&app, identity)?;
    println!("bundle: {}", app.display());
    Ok(app)
}

/// Signs the executable, then the bundle, with the hardened runtime.
/// Without an identity the signature is ad hoc: valid on every Mac
/// (Apple silicon refuses unsigned code) but not tied to a developer.
/// `install.sh` re-signs ad-hoc builds with a per-machine identity.
fn sign_app(app: &Path, identity: Option<&str>) -> Result<()> {
    let entitlements = root().join("packaging/macos/entitlements.plist");
    let sign = |path: &Path| {
        let mut command = Command::new("codesign");
        command
            .args(["--force", "--options", "runtime", "--entitlements"])
            .arg(&entitlements);
        match identity {
            // Apple's timestamp service only countersigns Apple-issued
            // certificates; notarization requires the timestamp.
            Some(identity) if is_developer_id(identity) => {
                command.args(["--timestamp", "--sign", identity])
            }
            Some(identity) => command.args(["--timestamp=none", "--sign", identity]),
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

/// Whether `identity` names an Apple Developer ID certificate (the only kind
/// that can be notarized).
pub(crate) fn is_developer_id(identity: &str) -> bool {
    identity.starts_with("Developer ID Application")
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

/// The menu entry. `exec` is the program as the `Exec` key wants it: the
/// bare name for packages that install into `PATH` (`/usr/bin`), or an
/// [`exec_path`] for installs below the home directory, which desktop
/// sessions often don't have on `PATH`.
pub(crate) fn desktop_entry(exec: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={APP_NAME}\n\
         GenericName=Icinga 2 client\n\
         Comment=Monitor Icinga 2 environments\n\
         Exec={exec}\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=Network;Monitor;System;\n\
         Keywords=icinga;monitoring;alerts;\n\
         StartupWMClass={APP_ID}\n"
    )
}

/// An absolute program path for a desktop entry's `Exec` key, quoted as
/// the Desktop Entry Specification asks when it holds a space or another
/// reserved character. `%` (field codes) can't be expressed reliably and is
/// refused.
pub(crate) fn exec_path(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))?;
    if path.contains('%') || path.chars().any(char::is_control) {
        return Err(format!("{path:?} can't be used in a desktop entry"));
    }
    let reserved = |c: char| " \t\"'\\><~|&;$*?#()`".contains(c);
    if !path.contains(reserved) {
        return Ok(path.to_owned());
    }
    let mut quoted = String::from('"');
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    // The desktop file's own string escaping doubles each backslash.
    Ok(quoted.replace('\\', "\\\\"))
}

pub(crate) fn bundle_linux(binary: &Path, out: &Path) -> Result<PathBuf> {
    let tree = out.join(APP_NAME);
    remove_dir(&tree)?;
    copy(binary, &tree.join("bin").join(BINARY))?;
    write(
        &tree.join(format!("share/applications/{APP_ID}.desktop")),
        desktop_entry(BINARY),
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
    for notice in LEGAL_FILES {
        copy(
            &root().join(notice),
            &tree.join(format!("share/doc/{APP_NAME}")).join(notice),
        )?;
    }
    normalise_modes(&tree)?;
    println!("bundle: {}", tree.display());
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `package --prebuilt` takes only a finished release bundle that
    /// reports the workspace version, and builds nothing.
    #[test]
    #[cfg(target_os = "linux")]
    fn prebuilt_takes_only_a_finished_release_bundle() {
        use std::os::unix::fs::PermissionsExt as _;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(
            std::env::temp_dir().join(format!("icygui-xtask-prebuilt-{}", std::process::id())),
        );
        let fake = scratch.0.join("fake-binary");
        write(&fake, format!("#!/bin/sh\necho '{BINARY} {VERSION}'\n")).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let out = scratch.0.join("bundle");
        let tree = bundle_linux(&fake, &out).unwrap();
        let release = Flags::default();

        let error = prebuilt_in(&out, &release).unwrap_err();
        assert!(error.contains("no finished build"), "{error}");
        assert!(error.contains("cargo xtask bundle --release"), "{error}");
        write(&out.join(STAMP), stamp(false, false)).unwrap();
        let error = prebuilt_in(&out, &release).unwrap_err();
        assert!(error.contains("a debug build"), "{error}");

        write(&out.join(STAMP), stamp(true, false)).unwrap();
        assert_eq!(prebuilt_in(&out, &release).unwrap(), tree);
        let universal = Flags {
            universal: true,
            ..Flags::default()
        };
        assert!(prebuilt_in(&out, &universal).is_err(), "macOS-only");

        // A binary that reports another version isn't packaged.
        write(
            &tree.join("bin").join(BINARY),
            "#!/bin/sh\necho 'icygui 0.0.0'\n",
        )
        .unwrap();
        let error = prebuilt_in(&out, &release).unwrap_err();
        assert!(error.contains("--version printed"), "{error}");
    }

    #[test]
    fn desktop_entry_names_the_app_and_its_icon() {
        let entry = desktop_entry(BINARY);
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=icygui\n"));
        assert!(entry.contains("\nIcon=io.github.alexykn.icygui\n"));
        // GPUI sets the window's app id (Wayland) and class (X11) to the
        // app id, which ties the window to this entry.
        assert!(entry.contains("\nStartupWMClass=io.github.alexykn.icygui\n"));
        assert!(entry.lines().all(|line| !line.ends_with(' ')));
    }

    #[test]
    fn exec_paths_are_quoted_when_needed() {
        assert_eq!(
            exec_path(Path::new("/home/me/.local/bin/icygui")).unwrap(),
            "/home/me/.local/bin/icygui"
        );
        assert_eq!(
            exec_path(Path::new("/home/my user/.local/bin/icygui")).unwrap(),
            r#""/home/my user/.local/bin/icygui""#
        );
        // Quoting escapes `$`; the desktop file's string escaping then
        // doubles the backslash.
        assert_eq!(
            exec_path(Path::new("/opt/a$b/icygui")).unwrap(),
            r#""/opt/a\\$b/icygui""#
        );
        assert!(exec_path(Path::new("/opt/100%/icygui")).is_err());
    }
}
