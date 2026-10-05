#!/usr/bin/env bash
# icygui installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/alexykn/icygui/main/install.sh | bash
#
# Options (pass after `bash -s --` when piping):
#   --version X.Y.Z   install a specific release (default: latest)
#   --uninstall       remove icygui (keeps settings; add --purge to remove them too)
#   --purge           with --uninstall: also remove settings, logs and caches
#   --prefix DIR      Linux: install below DIR instead of ~/.local
#   --no-sign         macOS: keep the release's ad-hoc signature
#
# Environment:
#   ICYGUI_REPO           GitHub repository (default alexykn/icygui)
#   ICYGUI_VERSION        same as --version
#   ICYGUI_DOWNLOAD_URL   base URL for release assets, for mirrors and tests
#                         (default https://github.com/$ICYGUI_REPO/releases/download)
#
# What it does:
#   macOS: downloads the universal app, verifies it against the release's
#     SHA256SUMS, installs it into /Applications (or ~/Applications), and
#     re-signs it with a self-signed identity created once on this Mac. The
#     stable identity lets macOS remember that icygui may read its keychain
#     entries across updates. Nothing else is needed: codesign, security,
#     ditto and openssl ship with macOS (no Xcode or Command Line Tools).
#   Linux: downloads the tarball for this CPU, verifies it, and installs the
#     binary, desktop entry and icons below ~/.local (no sudo).
#
# The release is not notarized by Apple (no paid Developer ID yet). Files
# downloaded with curl carry no quarantine flag, so Gatekeeper does not
# block the first launch.

set -euo pipefail

APP_ID="io.github.alexykn.icygui"
APP_NAME="icygui"
SIGN_IDENTITY="icygui local code signing"

# ---------------------------------------------------------------- output

if [ -t 2 ]; then
  BOLD=$'\033[1m' DIM=$'\033[2m' RED=$'\033[31m' YELLOW=$'\033[33m' GREEN=$'\033[32m' RESET=$'\033[0m'
else
  BOLD="" DIM="" RED="" YELLOW="" GREEN="" RESET=""
fi

say() { printf '%s\n' "${BOLD}==>${RESET} $*" >&2; }
note() { printf '%s\n' "    ${DIM}$*${RESET}" >&2; }
warn() { printf '%s\n' "${YELLOW}warning:${RESET} $*" >&2; }
die() {
  printf '%s\n' "${RED}error:${RESET} $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not installed"
}

# ---------------------------------------------------------------- helpers

TMP_DIR=""
cleanup() { if [ -n "$TMP_DIR" ]; then rm -rf "$TMP_DIR"; fi; }
trap cleanup EXIT

make_tmp() {
  TMP_DIR=$(mktemp -d)
}

download() { # download <url> <file>
  local url=$1 file=$2
  local -a args=(-fL --retry 3 --retry-delay 2 --silent --show-error -o "$file")
  case $url in
    https://*) args+=(--proto '=https' --tlsv1.2) ;;
  esac
  curl "${args[@]}" "$url" || die "download failed: $url"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

verify() { # verify <file> <asset name> <SHA256SUMS>
  local file=$1 asset=$2 sums=$3 expected actual
  expected=$(awk -v name="$asset" '$2 == name || $2 == "*" name {print $1; exit}' "$sums")
  [ -n "$expected" ] || die "$asset is not listed in the release's SHA256SUMS"
  actual=$(sha256_of "$file")
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset (expected $expected, got $actual)"
  note "checksum verified ($actual)"
}

latest_version() {
  local api="https://api.github.com/repos/$REPO/releases/latest" tag
  tag=$(curl -fsSL --retry 3 -H 'Accept: application/vnd.github+json' "$api" 2>/dev/null |
    sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1) || true
  [ -n "$tag" ] || die "could not find the latest release of $REPO (pass --version X.Y.Z)"
  printf '%s\n' "${tag#v}"
}

# ---------------------------------------------------------------- macOS

macos_quit_running() {
  if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
    say "Quitting the running $APP_NAME"
    osascript -e "tell application id \"$APP_ID\" to quit" >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do
      pgrep -x "$APP_NAME" >/dev/null 2>&1 || return 0
      sleep 1
    done
    pkill -x "$APP_NAME" || true
  fi
}

macos_app_dir() {
  if [ -w /Applications ]; then
    printf '%s\n' /Applications
  else
    mkdir -p "$HOME/Applications"
    printf '%s\n' "$HOME/Applications"
  fi
}

macos_has_identity() {
  security find-identity -p codesigning 2>/dev/null | grep -F "\"$SIGN_IDENTITY\"" >/dev/null
}

macos_create_identity() {
  say "Creating a local code-signing identity (\"$SIGN_IDENTITY\")"
  note "It never leaves this Mac and only signs icygui, so macOS keeps"
  note "icygui's keychain access across updates."
  local dir pass
  dir=$(mktemp -d)
  pass=$(/usr/bin/openssl rand -hex 16)
  cat >"$dir/cert.cnf" <<EOF
[ req ]
distinguished_name = dn
prompt = no
x509_extensions = ext
[ dn ]
CN = $SIGN_IDENTITY
[ ext ]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
EOF
  # /usr/bin/openssl (LibreSSL) writes PKCS#12 files that `security` can import;
  # a Homebrew OpenSSL 3 earlier in PATH would not.
  /usr/bin/openssl req -x509 -newkey rsa:3072 -sha256 -days 3650 -nodes \
    -keyout "$dir/key.pem" -out "$dir/cert.pem" -config "$dir/cert.cnf" >/dev/null 2>&1 ||
    { rm -rf "$dir"; return 1; }
  /usr/bin/openssl pkcs12 -export -inkey "$dir/key.pem" -in "$dir/cert.pem" \
    -name "$SIGN_IDENTITY" -out "$dir/identity.p12" -passout "pass:$pass" >/dev/null 2>&1 ||
    { rm -rf "$dir"; return 1; }
  if ! security import "$dir/identity.p12" -P "$pass" -f pkcs12 -T /usr/bin/codesign >/dev/null 2>&1; then
    rm -rf "$dir"
    return 1
  fi
  rm -rf "$dir"
}

macos_codesign() { # macos_codesign <identity> <app>
  local identity=$1 app=$2
  codesign --force --options runtime --timestamp=none --sign "$identity" \
    "$app/Contents/MacOS/$APP_NAME" >/dev/null 2>&1 &&
    codesign --force --options runtime --timestamp=none --sign "$identity" "$app" >/dev/null 2>&1 &&
    codesign --verify --strict "$app" >/dev/null 2>&1
}

macos_sign() { # macos_sign <app>
  local app=$1
  if ! macos_has_identity && ! macos_create_identity; then
    warn "could not create the local signing identity; keeping the ad-hoc signature"
    return 0
  fi
  say "Signing $APP_NAME with \"$SIGN_IDENTITY\""
  note "macOS may ask once whether codesign may use this key: choose \"Always Allow\"."
  if macos_codesign "$SIGN_IDENTITY" "$app"; then
    return 0
  fi
  warn "signing with the local identity failed; using an ad-hoc signature instead"
  note "icygui works the same, but macOS may ask again for keychain access after updates."
  macos_codesign - "$app" || die "ad-hoc signing failed"
}

macos_install() {
  need curl
  need ditto
  need codesign
  local asset="$APP_NAME-$VERSION-macos-universal.zip" dir app_dir
  make_tmp
  dir=$TMP_DIR
  say "Downloading $APP_NAME $VERSION for macOS"
  download "$BASE_URL/v$VERSION/$asset" "$dir/$asset"
  download "$BASE_URL/v$VERSION/SHA256SUMS" "$dir/SHA256SUMS"
  verify "$dir/$asset" "$asset" "$dir/SHA256SUMS"
  ditto -x -k "$dir/$asset" "$dir/unpacked"
  [ -d "$dir/unpacked/$APP_NAME.app" ] || die "the download does not contain $APP_NAME.app"

  macos_quit_running
  app_dir=$(macos_app_dir)
  say "Installing into $app_dir"
  rm -rf "${app_dir:?}/$APP_NAME.app"
  ditto "$dir/unpacked/$APP_NAME.app" "$app_dir/$APP_NAME.app"
  xattr -dr com.apple.quarantine "$app_dir/$APP_NAME.app" 2>/dev/null || true
  if [ "$SIGN" = 1 ]; then
    macos_sign "$app_dir/$APP_NAME.app"
  fi
  say "${GREEN}Installed $APP_NAME $VERSION${RESET} → $app_dir/$APP_NAME.app"
  note "Start it from Launchpad or with: open -a $APP_NAME"
}

macos_uninstall() {
  macos_quit_running
  local launch_agent="$HOME/Library/LaunchAgents/$APP_ID.plist"
  if [ -f "$launch_agent" ]; then
    launchctl bootout "gui/$(id -u)" "$launch_agent" >/dev/null 2>&1 || true
    rm -f "$launch_agent"
  fi
  rm -rf "/Applications/$APP_NAME.app" "$HOME/Applications/$APP_NAME.app"
  if [ "$PURGE" = 1 ]; then
    rm -rf "$HOME/Library/Application Support/$APP_ID" "$HOME/Library/Caches/$APP_ID" \
      "$HOME/Library/Logs/$APP_NAME" "$HOME/Library/Preferences/$APP_ID.plist" \
      "$HOME/Library/Saved Application State/$APP_ID.savedState"
    if macos_has_identity; then
      security delete-identity -c "$SIGN_IDENTITY" >/dev/null 2>&1 || true
    fi
    note "Saved passwords stay in your keychain; remove them in Keychain Access (search \"$APP_ID\")."
  fi
  say "${GREEN}Removed $APP_NAME${RESET}"
}

# ---------------------------------------------------------------- Linux

linux_arch() {
  case $(uname -m) in
    x86_64 | amd64) printf '%s\n' x86_64 ;;
    aarch64 | arm64) printf '%s\n' aarch64 ;;
    *) die "no prebuilt $APP_NAME for $(uname -m); build from source (see docs/development.md)" ;;
  esac
}

linux_refresh_desktop() {
  local share=$1
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$share/applications" >/dev/null 2>&1 || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$share/icons/hicolor" >/dev/null 2>&1 || true
  fi
}

linux_check_libraries() {
  local binary=$1 missing
  command -v ldd >/dev/null 2>&1 || return 0
  missing=$(ldd "$binary" 2>/dev/null | awk '/not found/ {print $1}' | tr '\n' ' ')
  [ -z "$missing" ] && return 0
  warn "missing system libraries: $missing"
  if command -v apt-get >/dev/null 2>&1; then
    note "sudo apt-get install libxkbcommon0 libxkbcommon-x11-0 libxcb1 libfontconfig1 libfreetype6 libvulkan1 libwayland-client0"
  elif command -v dnf >/dev/null 2>&1; then
    note "sudo dnf install libxkbcommon libxkbcommon-x11 libxcb fontconfig freetype vulkan-loader libwayland-client"
  elif command -v pacman >/dev/null 2>&1; then
    note "sudo pacman -S libxkbcommon libxkbcommon-x11 libxcb fontconfig freetype2 vulkan-icd-loader wayland"
  else
    note "install xkbcommon (+x11), xcb, fontconfig, freetype, the Vulkan loader and libwayland-client"
  fi
}

linux_install() {
  need curl
  need tar
  local arch asset dir share
  arch=$(linux_arch)
  asset="$APP_NAME-$VERSION-linux-$arch.tar.gz"
  make_tmp
  dir=$TMP_DIR
  say "Downloading $APP_NAME $VERSION for Linux ($arch)"
  download "$BASE_URL/v$VERSION/$asset" "$dir/$asset"
  download "$BASE_URL/v$VERSION/SHA256SUMS" "$dir/SHA256SUMS"
  verify "$dir/$asset" "$asset" "$dir/SHA256SUMS"
  tar -xzf "$dir/$asset" -C "$dir"
  [ -x "$dir/$APP_NAME/bin/$APP_NAME" ] || die "the download does not contain bin/$APP_NAME"

  mkdir -p "$PREFIX/bin" 2>/dev/null || true
  [ -w "$PREFIX/bin" ] || die "cannot write to $PREFIX/bin (choose another --prefix, or run with sudo for a system prefix)"
  share="$PREFIX/share"
  say "Installing into $PREFIX"
  install -m 0755 "$dir/$APP_NAME/bin/$APP_NAME" "$PREFIX/bin/$APP_NAME"
  (cd "$dir/$APP_NAME/share" && find . -type f) | while IFS= read -r file; do
    mkdir -p "$share/$(dirname "$file")"
    install -m 0644 "$dir/$APP_NAME/share/$file" "$share/$file"
  done
  # Desktop sessions often don't have ~/.local/bin on PATH: use an absolute Exec.
  local desktop="$share/applications/$APP_ID.desktop"
  if [ -f "$desktop" ]; then
    sed -i.bak "s|^Exec=.*|Exec=$PREFIX/bin/$APP_NAME|" "$desktop" && rm -f "$desktop.bak"
  fi
  linux_refresh_desktop "$share"
  linux_check_libraries "$PREFIX/bin/$APP_NAME"
  say "${GREEN}Installed $APP_NAME $VERSION${RESET} → $PREFIX/bin/$APP_NAME"
  case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) note "$PREFIX/bin is not on your PATH; start icygui from your app launcher or add it to PATH." ;;
  esac
}

linux_uninstall() {
  local share="$PREFIX/share" size
  pkill -x "$APP_NAME" >/dev/null 2>&1 || true
  rm -f "$PREFIX/bin/$APP_NAME" "$share/applications/$APP_ID.desktop" \
    "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$APP_ID.desktop"
  for size in 16 32 64 128 256 512; do
    rm -f "$share/icons/hicolor/${size}x${size}/apps/$APP_ID.png"
  done
  rm -f "$share/icons/hicolor/scalable/apps/$APP_ID.svg"
  rm -rf "$share/doc/$APP_NAME"
  linux_refresh_desktop "$share"
  if [ "$PURGE" = 1 ]; then
    rm -rf "${XDG_CONFIG_HOME:-$HOME/.config}/$APP_NAME" "${XDG_DATA_HOME:-$HOME/.local/share}/$APP_NAME" \
      "${XDG_CACHE_HOME:-$HOME/.cache}/$APP_NAME" "${XDG_STATE_HOME:-$HOME/.local/state}/$APP_NAME"
    note "Saved passwords stay in your keyring; remove them with your keyring manager (search \"$APP_ID\")."
  fi
  say "${GREEN}Removed $APP_NAME${RESET}"
}

# ---------------------------------------------------------------- main

usage() {
  cat <<'USAGE'
icygui installer for macOS and Linux

  curl -fsSL https://raw.githubusercontent.com/alexykn/icygui/main/install.sh | bash
  curl -fsSL .../install.sh | bash -s -- [options]

Options:
  --version X.Y.Z   install a specific release (default: latest)
  --uninstall       remove icygui (keeps settings; add --purge to remove them too)
  --purge           with --uninstall: also remove settings, logs and caches
  --prefix DIR      Linux: install below DIR instead of ~/.local
  --no-sign         macOS: keep the release's ad-hoc signature
  -h, --help        show this help
USAGE
}


main() {
  REPO=${ICYGUI_REPO:-alexykn/icygui}
  BASE_URL=${ICYGUI_DOWNLOAD_URL:-https://github.com/$REPO/releases/download}
  VERSION=${ICYGUI_VERSION:-}
  PREFIX="$HOME/.local"
  UNINSTALL=0
  PURGE=0
  SIGN=1

  while [ $# -gt 0 ]; do
    case $1 in
      --version)
        [ $# -ge 2 ] || die "--version needs a value"
        VERSION=${2#v}
        shift 2
        ;;
      --prefix)
        [ $# -ge 2 ] || die "--prefix needs a value"
        PREFIX=$2
        shift 2
        ;;
      --uninstall) UNINSTALL=1 && shift ;;
      --purge) PURGE=1 && shift ;;
      --no-sign) SIGN=0 && shift ;;
      -h | --help)
        usage
        exit 0
        ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
  done

  local os
  os=$(uname -s)
  case $os in
    Darwin)
      if [ "$UNINSTALL" = 1 ]; then macos_uninstall; return; fi
      [ -n "$VERSION" ] || VERSION=$(latest_version)
      macos_install
      ;;
    Linux)
      if [ "$UNINSTALL" = 1 ]; then linux_uninstall; return; fi
      [ -n "$VERSION" ] || VERSION=$(latest_version)
      linux_install
      ;;
    *) die "unsupported system: $os (icygui supports macOS and Linux)" ;;
  esac
}

# Everything runs from main, so a truncated download never executes a
# partial script.
main "$@"
