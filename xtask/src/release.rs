//! Release artifacts: signed and notarized `.dmg` (macOS), `.tar.gz` and
//! `.deb` (Linux), and `SHA256SUMS`.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::{
    APP_NAME, BINARY, Flags, Result, VERSION, bundle, copy, copy_tree, create_dir, remove_dir, run,
    target_dir, write,
};

/// Builds release bundles and packages them into `target/dist`.
pub(crate) fn package(flags: &Flags) -> Result<()> {
    if flags.notarize && !flags.sign.as_deref().is_some_and(bundle::is_developer_id) {
        return Err(
            "--notarize needs --sign with a \"Developer ID Application\" identity".to_owned(),
        );
    }
    let bundle = bundle::bundle(&Flags {
        release: true,
        universal: flags.universal,
        sign: flags.sign.clone(),
        ..Flags::default()
    })?;
    let dist = target_dir().join("dist");
    remove_dir(&dist)?;
    create_dir(&dist)?;
    if cfg!(target_os = "macos") {
        if flags.notarize {
            notarize(&bundle)?;
        }
        let arch = if flags.universal {
            "universal"
        } else {
            env::consts::ARCH
        };
        // The .zip is what install.sh downloads; the .dmg is for manual installs.
        let zip = dist.join(format!("{APP_NAME}-{VERSION}-macos-{arch}.zip"));
        run(Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&bundle)
            .arg(&zip))?;
        println!("package: {}", zip.display());
        let dmg = dist.join(format!("{APP_NAME}-{VERSION}-macos-{arch}.dmg"));
        make_dmg(&bundle, &dmg, flags.sign.as_deref())?;
        if flags.notarize {
            notarize(&dmg)?;
        }
    } else {
        let arch = env::consts::ARCH;
        let tarball = dist.join(format!("{APP_NAME}-{VERSION}-linux-{arch}.tar.gz"));
        run(Command::new("tar")
            .arg("--owner=0")
            .arg("--group=0")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(bundle.parent().unwrap_or(&bundle))
            .arg(APP_NAME))?;
        println!("package: {}", tarball.display());
        deb(&bundle, &dist)?;
    }
    write_checksums(&dist)
}

/// A drag-to-install disk image with an `/Applications` link, signed when
/// an identity is given.
fn make_dmg(app: &Path, dmg: &Path, identity: Option<&str>) -> Result<()> {
    let staging = target_dir().join("dmg-staging");
    remove_dir(&staging)?;
    create_dir(&staging)?;
    // ditto keeps the code signature, extended attributes and the stapled ticket.
    run(Command::new("ditto")
        .arg(app)
        .arg(staging.join(format!("{APP_NAME}.app"))))?;
    symlink_applications(&staging)?;
    run(Command::new("hdiutil")
        .args([
            "create",
            "-volname",
            APP_NAME,
            "-fs",
            "HFS+",
            "-format",
            "UDZO",
            "-ov",
            "-srcfolder",
        ])
        .arg(&staging)
        .arg(dmg))?;
    if let Some(identity) = identity {
        run(Command::new("codesign")
            .args(["--force", "--timestamp", "--sign", identity])
            .arg(dmg))?;
    }
    println!("package: {}", dmg.display());
    Ok(())
}

#[cfg(unix)]
fn symlink_applications(staging: &Path) -> Result<()> {
    std::os::unix::fs::symlink("/Applications", staging.join("Applications"))
        .map_err(|error| format!("linking /Applications: {error}"))
}

#[cfg(not(unix))]
fn symlink_applications(_staging: &Path) -> Result<()> {
    Err("disk images can only be built on macOS".to_owned())
}

/// Submits an `.app` (zipped) or `.dmg` to Apple's notary service, waits for
/// the verdict, and staples the ticket.
///
/// Credentials, in order of preference:
/// - App Store Connect API key: `APPLE_API_KEY_PATH`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER`
/// - Apple ID: `APPLE_ID`, `APPLE_TEAM_ID`, `APPLE_APP_PASSWORD` (app-specific password)
/// - a stored `notarytool` profile: `APPLE_NOTARY_PROFILE`
pub(crate) fn notarize(path: &Path) -> Result<()> {
    let upload = if path.extension().is_some_and(|ext| ext == "app") {
        let zip = path.with_extension("zip");
        run(Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(path)
            .arg(&zip))?;
        zip
    } else {
        path.to_path_buf()
    };
    let mut command = Command::new("xcrun");
    command
        .args(["notarytool", "submit"])
        .arg(&upload)
        .args(["--wait", "--output-format", "json"]);
    command.args(notary_credentials()?);
    eprintln!("» xcrun notarytool submit {} --wait", upload.display());
    let output = command
        .output()
        .map_err(|error| format!("failed to start notarytool: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let verdict: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_default();
    let status = verdict["status"].as_str().unwrap_or("unknown");
    if !output.status.success() || status != "Accepted" {
        let id = verdict["id"].as_str().unwrap_or_default();
        if !id.is_empty() {
            let _ = run(Command::new("xcrun")
                .args(["notarytool", "log", id])
                .args(notary_credentials()?));
        }
        return Err(format!(
            "notarization of {} finished with status {status}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    println!("notarized: {} ({status})", path.display());
    if upload != path {
        fs::remove_file(&upload)
            .map_err(|error| format!("removing {}: {error}", upload.display()))?;
    }
    run(Command::new("xcrun").args(["stapler", "staple"]).arg(path))?;
    run(Command::new("xcrun")
        .args(["stapler", "validate"])
        .arg(path))
}

fn notary_credentials() -> Result<Vec<String>> {
    let var = |name: &str| env::var(name).ok().filter(|value| !value.is_empty());
    if let (Some(key), Some(key_id), Some(issuer)) = (
        var("APPLE_API_KEY_PATH"),
        var("APPLE_API_KEY_ID"),
        var("APPLE_API_ISSUER"),
    ) {
        return Ok(vec![
            "--key".into(),
            key,
            "--key-id".into(),
            key_id,
            "--issuer".into(),
            issuer,
        ]);
    }
    if let (Some(apple_id), Some(team), Some(password)) = (
        var("APPLE_ID"),
        var("APPLE_TEAM_ID"),
        var("APPLE_APP_PASSWORD"),
    ) {
        return Ok(vec![
            "--apple-id".into(),
            apple_id,
            "--team-id".into(),
            team,
            "--password".into(),
            password,
        ]);
    }
    if let Some(profile) = var("APPLE_NOTARY_PROFILE") {
        return Ok(vec!["--keychain-profile".into(), profile]);
    }
    Err(
        "no notarization credentials: set APPLE_API_KEY_PATH/APPLE_API_KEY_ID/APPLE_API_ISSUER, \
         APPLE_ID/APPLE_TEAM_ID/APPLE_APP_PASSWORD or APPLE_NOTARY_PROFILE (see docs/releasing.md)"
            .to_owned(),
    )
}

fn deb(bundle: &Path, dist: &Path) -> Result<()> {
    let deb_arch = match env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    let staging = target_dir().join("deb").join(APP_NAME);
    remove_dir(&staging)?;
    let usr = staging.join("usr");
    copy(
        &bundle.join("bin").join(BINARY),
        &usr.join("bin").join(BINARY),
    )?;
    copy_tree(&bundle.join("share"), &usr.join("share"))?;
    let installed_kib = dir_size(&usr)? / 1024;
    write(
        &staging.join("DEBIAN/control"),
        format!(
            "Package: {APP_NAME}\n\
             Version: {VERSION}\n\
             Section: net\n\
             Priority: optional\n\
             Architecture: {deb_arch}\n\
             Installed-Size: {installed_kib}\n\
             Depends: libc6, libxkbcommon0, libxkbcommon-x11-0, libxcb1, libfontconfig1, libfreetype6, libvulkan1, libwayland-client0\n\
             Recommends: mesa-vulkan-drivers, gnome-shell-extension-appindicator | plasma-workspace\n\
             Maintainer: Alexander Knott\n\
             Homepage: https://github.com/{repo}\n\
             Description: Native desktop client for Icinga 2\n \
             Live problem lists, dashboards, operator actions and native\n \
             notifications for the Icinga 2 REST API.\n",
            repo = crate::DEFAULT_REPO,
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

fn dir_size(path: &Path) -> Result<u64> {
    let mut total = 0;
    let entries =
        fs::read_dir(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("reading {}: {error}", path.display()))?;
        let meta = entry
            .metadata()
            .map_err(|error| format!("reading {}: {error}", entry.path().display()))?;
        total += if meta.is_dir() {
            dir_size(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}

/// SHA-256 of a file as lowercase hex.
pub(crate) fn sha256_hex(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|error| format!("reading {}: {error}", path.display()))?;
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(&bytes) {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Files in `dist` that are release artifacts, sorted by name.
pub(crate) fn artifacts(dist: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = fs::read_dir(dist)
        .map_err(|error| format!("reading {}: {error}", dist.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(APP_NAME) && !name.ends_with(".sha256"))
        })
        .collect();
    files.sort();
    Ok(files)
}

/// Writes `SHA256SUMS` (in `sha256sum -c` format) next to the artifacts.
fn write_checksums(dist: &Path) -> Result<()> {
    let mut sums = String::new();
    for file in artifacts(dist)? {
        let name = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let _ = writeln!(sums, "{}  {name}", sha256_hex(&file)?);
    }
    write(&dist.join("SHA256SUMS"), sums)?;
    println!("wrote {}", dist.join("SHA256SUMS").display());
    Ok(())
}
