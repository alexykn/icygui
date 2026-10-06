# Releasing

A release is one tag push: `.github/workflows/release.yml` then runs these steps:
1. Build the macOS app as a universal binary (Apple silicon + Intel).
2. Build the Linux packages (x86_64 and aarch64).
3. Publish everything as a GitHub Release with `SHA256SUMS` and build-provenance attestations.

Users install and update with `install.sh`.

## How macOS builds are signed

There is no paid Apple Developer ID yet, so releases are **ad-hoc signed and not notarized**:

- Apple silicon only runs signed code. The ad-hoc signature satisfies that on every Mac, at no cost.
- **`install.sh` is the supported way to install on macOS.**
  - It downloads with `curl`, so the app carries no quarantine flag and Gatekeeper doesn't block the first launch.
  - It verifies the download against `SHA256SUMS`.
  - It re-signs the app with a self-signed identity it creates once per Mac (`icygui local code signing`). That gives the app a stable identity, so macOS remembers across updates that icygui may read its keychain entries. With a plain ad-hoc signature, macOS would ask again after every update.
  - Everything it uses ships with macOS (`codesign`, `security`, `ditto`, `/usr/bin/openssl`). No Xcode or Command Line Tools are needed.
- **Manual downloads** of the `.dmg` from a browser are quarantined. Users then open the app once via System Settings → Privacy & Security → *Open Anyway*, or run `xattr -dr com.apple.quarantine /Applications/icygui.app`.

### Later: Developer ID and notarization

The pipeline already supports it. Once you have an Apple Developer Program membership:

1. **Developer ID certificate.** In Xcode, open Settings → Accounts → Manage Certificates → + → *Developer ID Application*. Export it from Keychain Access as a `.p12` with its private key and a password.
2. **Notarization key.** In App Store Connect, go to Users and Access → Integrations → App Store Connect API → Team Keys and generate a key with the *Developer* role. Download the `.p8` and note the Key ID and Issuer ID.
3. **Repository secrets** (Settings → Secrets and variables → Actions):

   | Secret | Value |
   |---|---|
   | `MACOS_CERTIFICATE` | `base64 -i DeveloperID.p12` |
   | `MACOS_CERTIFICATE_PASSWORD` | the `.p12` password |
   | `MACOS_SIGNING_IDENTITY` | e.g. `Developer ID Application: Alexander Knott (TEAMID1234)` (`security find-identity -v -p codesigning`) |
   | `APPLE_API_KEY` | `base64 -i AuthKey_XXXXXXXXXX.p8` |
   | `APPLE_API_KEY_ID` | the key ID |
   | `APPLE_API_ISSUER` | the issuer ID |

From the next tag on, the app and the `.dmg` are signed with the hardened runtime, notarized and stapled. `install.sh` then keeps Apple's signature instead of re-signing, and browser downloads open without warnings.

### Optional: Homebrew tap

A personal tap needs no Developer ID either:

1. Create a public repository `alexykn/homebrew-tap`, or any `homebrew-*` name, and set the Actions variable `HOMEBREW_TAP_REPO`.
2. Add a fine-grained token with *Contents: read and write* on that repository as the secret `HOMEBREW_TAP_TOKEN`.

The release workflow then writes `Casks/icygui.rb` and `Formula/icygui.rb` (`cargo xtask homebrew`). While builds aren't notarized, the cask clears the quarantine flag after installing and says so in its caveats. Users run `brew install --cask alexykn/tap/icygui`.

### Optional: GPG-signed checksums

Set `GPG_PRIVATE_KEY` (ASCII-armoured) and `GPG_PASSPHRASE`; the release then includes `SHA256SUMS.asc`.

## Cutting a release

1. Set `version` in the root `Cargo.toml` (`[workspace.package]`; every crate inherits it), then run `cargo update -w` so `Cargo.lock` follows.
2. Commit, then `git tag v<version>` and `git push --tags`.
3. Watch the *Release* workflow. It fails early if the tag and the workspace version differ.

To rebuild an existing tag, for example after adding the Developer ID secrets, run the workflow by hand (*Actions → Release → Run workflow*) with that tag. If the GitHub Release already exists, its files are replaced (`gh release upload --clobber`); otherwise it is created.

A version with a pre-release part (`0.2.0-rc.1`, tag `v0.2.0-rc.1`) is published as a GitHub pre-release and doesn't update the Homebrew tap. `install.sh` installs it only when asked: `--version 0.2.0-rc.1`.

## What the pipeline checks

Every push and pull request (`ci.yml`, on Linux and macOS):

- `cargo fmt`, clippy with `-D warnings`, the tests, and `cargo-deny` (advisories, licences, sources);
- `cargo xtask bundle` (ad-hoc signed `.app` on macOS), which also runs the binary's `--version` and compares it with the workspace version;
- `icygui --version` and `--help` with no display (REL-06);
- Linux: `desktop-file-validate` on the menu entry, `shellcheck` on the scripts, and `install.sh` end to end against a local release (install, absolute `Exec`, icons, `--uninstall --purge`).

`xtask`'s tests also check that the logo files draw the same mark, that the committed icons, `.icns` and banner are what `cargo xtask icons` renders, the layout and ownership of the `.tar.gz` and `.deb` (built from a stand-in binary), `SHA256SUMS`, and the Homebrew cask and formula. `ic-platform`'s tests check that the tray icon's shapes match the mark SVG.

The contract tests (`contract.yml`) run against Icinga in Docker nightly, on pull requests and pushes to `main` that change `ic-api`, `ic-model`, `contract/` or `Cargo.lock`, and by hand with another image tag. The performance budgets (`perf.yml`, docs/performance.md) run nightly and by hand, in release builds against `ic-mock`'s `large` scenario.

A release (`release.yml`) additionally checks:

- the tag against the workspace version;
- macOS: both slices in the universal binary (`lipo -verify_arch arm64 x86_64`), a valid signature with the hardened runtime flag, and `--version`; with the Developer ID secrets, also `spctl` (Gatekeeper) and the stapled ticket on the `.dmg`;
- Linux: the `.deb`'s control fields and contents, and the tarball listing, printed in the log.

### Only CI or a Mac can confirm

These can't be built or tested on the Linux development machine. Check them on the first release:

- The universal macOS build, ad-hoc signing with the hardened runtime, the `.dmg`, and `notarytool` and `stapler` once the Developer ID secrets exist (the steps above fail the job if anything is off).
- The aarch64 Linux packages (built on GitHub's `ubuntu-24.04-arm` runner).
- `install.sh` on macOS: the local signing identity (`security`, `codesign`, `/usr/bin/openssl`), quitting a running instance, `/Applications` versus `~/Applications`. If `codesign` won't use the self-signed identity, the script keeps an ad-hoc signature and says so. Check with `codesign -dv /Applications/icygui.app 2>&1 | grep Authority`.
- The Homebrew tap: `brew install --cask alexykn/tap/icygui` and `brew uninstall --zap --cask icygui` on a Mac, and `brew install alexykn/tap/icygui` and `brew test icygui` on Linux. The cask and formula are generated and syntax-checked here, but not run.
- The build-provenance attestations (`gh attestation verify`) and the optional `SHA256SUMS.asc`.

## Verifying a release

```sh
sha256sum -c SHA256SUMS                                  # any platform
gh attestation verify icygui_0.1.0_amd64.deb --repo alexykn/icygui
codesign --verify --strict --verbose=2 /Applications/icygui.app
codesign -dv /Applications/icygui.app 2>&1 | grep Authority   # "icygui local code signing" after install.sh
```

## Local builds

```sh
cargo xtask icons                    # after editing assets/logo/*.svg
cargo xtask bundle --release         # ad-hoc signed target/bundle/icygui.app (notifications need the bundle)
cargo xtask package                  # Linux: target/dist/*.tar.gz, *.deb, SHA256SUMS
```

Test the installer against local artifacts without publishing:

```sh
mkdir -p /tmp/rel/v0.1.0 && cp target/dist/* /tmp/rel/v0.1.0/
(cd /tmp/rel && python3 -m http.server 8765) &
ICYGUI_DOWNLOAD_URL=http://127.0.0.1:8765 ./install.sh --version 0.1.0
```

## Notes

- **Logo:** the sources are `assets/logo/icygui.svg` (app icon on Apple's icon grid), `icygui-mark.svg` and `banner.svg`. Every PNG, the `.icns` and `banner.png` are generated by `cargo xtask icons`; commit the regenerated files.
- **macOS 26 icon style:** the app ships a classic `.icns`. A layered Icon Composer icon (`.icon`, compiled with Xcode 26's `actool`) can be added later for the "Liquid Glass" look.
- **Packages:** the Linux install tree is `bin/icygui`, `share/applications/io.github.alexykn.icygui.desktop`, the hicolor icons (16–512 px and scalable) and `share/doc/icygui/` (`LICENSE`, `THIRD_PARTY_NOTICES.md`). The `.tar.gz` holds it under `icygui/`, and the `.deb` holds it below `/usr`. Both are owned by root, with directories and the binary at 0755 and other files at 0644 whatever the build's umask. The `.deb` relies on dpkg triggers to refresh menus and icon caches; it has no maintainer scripts. `install.sh`, `cargo xtask install` and the Homebrew formula point the menu entry's `Exec` at the binary's absolute path, because desktop sessions often lack `~/.local/bin` on `PATH`.
- **Uninstalling:** `install.sh --uninstall` also removes the login item (LaunchAgent or XDG autostart entry). The cask's `uninstall` only quits the app and leaves the login item to `zap`, because Homebrew runs `uninstall` on every upgrade and a removed login item would silently turn off launch at login. Without `zap`, the leftover login item points at a missing app, and launchd skips it.
- **Notices:** every bundle carries `LICENSE` and `THIRD_PARTY_NOTICES.md` (the bundled font and icons, and how the Rust crates' licences are checked): `icygui.app/Contents/Resources/` on macOS, `share/doc/icygui/` in the Linux tree, `.tar.gz` and `.deb`.
- **Entitlements:** `packaging/macos/entitlements.plist` is deliberately empty. The app needs no hardened-runtime exceptions.
