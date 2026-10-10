#!/usr/bin/env bash
# Screenshots of icygui against the demo cluster (docs/development.md,
# "Screenshots from the demo cluster"), for builders and reviewers. Linux
# with Xvfb, xdotool and ImageMagick's `import`.
#
#   demo/screenshot.sh start [--theme dark|light] [--select GROUP/DASHBOARD] [--via-proxy] [--no-build] [--keep]
#   demo/screenshot.sh open <palette text>    # ctrl-k, type, enter: a page or dashboard
#   demo/screenshot.sh key <xdotool keys…>     # e.g. key ctrl+k Escape
#   demo/screenshot.sh type <text>
#   demo/screenshot.sh scenario <name> [arg]   # demo/scenario.sh
#   demo/screenshot.sh shot <out.png> [seconds to wait first]
#   demo/screenshot.sh stop
#   demo/screenshot.sh capture <out.png> [start options] [--open TEXT] [--scenario NAME] [--wait S]
#
# `start` brings the cluster up (idempotent, demo/up.sh), builds the debug
# app, writes a throwaway settings directory with the demo environment, its
# dashboards and the demo password (plain file, ICYGUI_DEV_SECRETS_DIR:
# development only), and starts icygui on the X display $DISPLAY_NUMBER
# (default :93; started if no X server answers there) with a private
# HOME, XDG directories and no D-Bus session, so nothing reaches your
# desktop or keychain (--keep keeps the last start's data directory: the
# event log behind event streams and history). It returns once icygui is
# connected and has had $SETTLE seconds (default 10) to load. `capture` does start, the scenario,
# the page, the wait and the shot, then stop; scenarios stay in effect until
# `demo/scenario.sh recover`.
#
# Work files go to $ICYGUI_SHOT_DIR (default target/demo-shots); the app's
# log is $ICYGUI_SHOT_DIR/home/.local/state/icygui/logs/icygui.log, its
# terminal output $ICYGUI_SHOT_DIR/app.log. Start the app before a scenario:
# `start` runs demo/up.sh, which starts stopped nodes again.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
DISPLAY_NUMBER=${DISPLAY_NUMBER:-:93}
WORK=${ICYGUI_SHOT_DIR:-$ROOT/target/demo-shots}
SETTLE=${SETTLE:-10}
APP=$ROOT/target/debug/icygui

die() { echo "screenshot: $*" >&2; exit 1; }

window() {
  DISPLAY=$DISPLAY_NUMBER xdotool search --sync --onlyvisible --name icygui 2>/dev/null | head -n1
}

start() {
  local theme=dark select=overview/problems proxy=() build=1 keep=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --theme) theme=$2; shift 2 ;;
      --select) select=$2; shift 2 ;;
      --via-proxy) proxy=(--via-proxy); shift ;;
      --no-build) build=0; shift ;;
      --keep) keep=1; shift ;;
      *) die "start: unknown option $1" ;;
    esac
  done
  stop_app
  "$HERE/up.sh" >/dev/null
  if [ "$build" = 1 ]; then
    (cd "$ROOT" && cargo build -p ic-app --quiet)
  fi
  [ -x "$APP" ] || die "no $APP; build it with cargo build -p ic-app"

  # A fresh home each time; --keep keeps the data directory (the event
  # log behind the event streams and the panes' history) of the last start.
  if [ "$keep" = 1 ] && [ -d "$WORK/home/.local/share/icygui" ]; then
    rm -rf "${WORK:?}/home/.config" "$WORK/home/.local/state" "$WORK/home/run" "$WORK/home/secrets"
  else
    rm -rf "${WORK:?}/home"
  fi
  mkdir -p "$WORK/home/.config/icygui" "$WORK/home/.local/share/icygui" "$WORK/home/.local/state" "$WORK/home/run"
  chmod 700 "$WORK/home/run"
  (cd "$ROOT" && cargo xtask demo-config --config-dir "$WORK/home/.config/icygui" \
    --data-dir "$WORK/home/.local/share/icygui" --secrets-dir "$WORK/home/secrets" \
    --select "$select" --theme "$theme" "${proxy[@]}" >/dev/null)

  if ! xdotool_ok; then
    Xvfb "$DISPLAY_NUMBER" -screen 0 1600x1000x24 -nolisten tcp >"$WORK/xvfb.log" 2>&1 &
    echo $! >"$WORK/xvfb.pid"
    sleep 1
  fi

  env -u DBUS_SESSION_BUS_ADDRESS -u WAYLAND_DISPLAY \
    DISPLAY="$DISPLAY_NUMBER" HOME="$WORK/home" \
    XDG_CONFIG_HOME="$WORK/home/.config" XDG_DATA_HOME="$WORK/home/.local/share" \
    XDG_STATE_HOME="$WORK/home/.local/state" XDG_RUNTIME_DIR="$WORK/home/run" \
    ICYGUI_DEV_SECRETS_DIR="$WORK/home/secrets" ICYGUI_WINDOW_CONTROLS=always \
    RUST_LOG="${RUST_LOG:-info}" \
    "$APP" >"$WORK/app.log" 2>&1 &
  echo $! >"$WORK/app.pid"

  local log="$WORK/home/.local/state/icygui/logs/icygui.log"
  for _ in $(seq 1 120); do
    kill -0 "$(cat "$WORK/app.pid")" 2>/dev/null || die "icygui exited; see $WORK/app.log"
    if grep -qs "connected" "$log" "$WORK/app.log" && [ -n "$(window)" ]; then
      sleep "$SETTLE"
      echo "icygui is up on $DISPLAY_NUMBER (window $(window)); log: $log"
      return
    fi
    sleep 1
  done
  die "icygui did not connect within 2 minutes; see $log"
}

# Whether an X server answers on the display.
xdotool_ok() { DISPLAY=$DISPLAY_NUMBER xdotool getdisplaygeometry >/dev/null 2>&1; }

stop_app() {
  if [ -f "$WORK/app.pid" ]; then
    kill "$(cat "$WORK/app.pid")" 2>/dev/null || true
    sleep 1
    kill -9 "$(cat "$WORK/app.pid")" 2>/dev/null || true
    rm -f "$WORK/app.pid"
  fi
}

keys() {
  local id
  id=$(window)
  [ -n "$id" ] || die "no icygui window on $DISPLAY_NUMBER (start it first)"
  DISPLAY=$DISPLAY_NUMBER xdotool windowactivate --sync "$id" 2>/dev/null || true
  DISPLAY=$DISPLAY_NUMBER xdotool key --window "$id" --delay 80 "$@" 2>/dev/null
}

type_text() {
  local id
  id=$(window)
  [ -n "$id" ] || die "no icygui window on $DISPLAY_NUMBER (start it first)"
  DISPLAY=$DISPLAY_NUMBER xdotool type --window "$id" --delay 40 "$1" 2>/dev/null
}

open_page() {
  keys ctrl+k
  sleep 0.6
  type_text "$1"
  sleep 0.6
  keys Return
  sleep 1.5
}

shot() {
  local out=$1 wait=${2:-0} id
  sleep "$wait"
  id=$(window)
  [ -n "$id" ] || die "no icygui window on $DISPLAY_NUMBER (start it first)"
  mkdir -p "$(dirname "$out")"
  DISPLAY=$DISPLAY_NUMBER import -window "$id" "$out"
  echo "$out"
}

mkdir -p "$WORK"
command=${1:-}
[ $# -gt 0 ] && shift
case "$command" in
  start) start "$@" ;;
  open) open_page "$*" ;;
  key) keys "$@" ;;
  type) type_text "$*" ;;
  scenario) "$HERE/scenario.sh" "$@" ;;
  shot) shot "$@" ;;
  stop)
    stop_app
    if [ -f "$WORK/xvfb.pid" ]; then
      kill "$(cat "$WORK/xvfb.pid")" 2>/dev/null || true
      rm -f "$WORK/xvfb.pid"
    fi
    ;;
  capture)
    out=${1:?capture needs an output file}
    shift
    start_args=() page="" scenario="" wait=0
    while [ $# -gt 0 ]; do
      case "$1" in
        --open) page=$2; shift 2 ;;
        --scenario) scenario=$2; shift 2 ;;
        --wait) wait=$2; shift 2 ;;
        --via-proxy | --no-build | --keep) start_args+=("$1"); shift ;;
        *) start_args+=("$1" "$2"); shift 2 ;;
      esac
    done
    start "${start_args[@]}"
    [ -n "$scenario" ] && "$HERE/scenario.sh" "$scenario" >/dev/null
    [ -n "$page" ] && open_page "$page"
    shot "$out" "$wait"
    stop_app
    ;;
  *)
    sed -n '2,25p' "$0" >&2
    exit 2
    ;;
esac
