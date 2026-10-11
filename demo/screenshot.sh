#!/usr/bin/env bash
# Screenshots of icygui against the demo cluster (docs/development.md,
# "Screenshots from the demo cluster"), for builders and reviewers. Linux
# with Xvfb, xdotool and ImageMagick's `import`.
#
#   demo/screenshot.sh start [--theme dark|light] [--select GROUP/DASHBOARD] [--via-proxy] [--no-build] [--keep]
#                            [--no-warm] [--recover]
#   demo/screenshot.sh open <palette text>    # ctrl-k, type, enter: a page or dashboard
#   demo/screenshot.sh key <xdotool keys…>     # e.g. key ctrl+k Escape
#   demo/screenshot.sh type <text>
#   demo/screenshot.sh scenario <name> [arg]   # demo/scenario.sh
#   demo/screenshot.sh shot <out.png> [seconds to wait first]
#   demo/screenshot.sh stop
#   demo/screenshot.sh capture <out.png> [start options] [--open TEXT] [--scenario NAME] [--wait S]
#
# `start` starts the cluster if there is none (demo/up.sh) and otherwise
# uses it as it is: it never starts a stopped node or thaws a frozen one,
# since the cluster is shared and a scenario or a cluster test may be in
# effect; it says so and stops instead (--recover runs `demo/scenario.sh
# recover` first). It builds the debug app, writes a throwaway settings
# directory with the demo environment and its dashboards, and starts
# icygui on the X display $DISPLAY_NUMBER (default :93; started if no X
# server answers there) with a private HOME and XDG directories and a D-Bus
# session of its own, in which a throwaway GNOME Keyring holds the demo
# password: icygui reads it from the Secret Service as it does on a
# desktop, and nothing reaches your desktop or keychain (--keep keeps the
# last start's data directory: the event log behind event streams and
# history). Needs dbus-daemon and gnome-keyring-daemon. Once icygui is connected it warms the event streams up (a few
# database services fail and recover: `demo/scenario.sh blips`; --no-warm
# leaves them empty) and gives it $SETTLE seconds (default 10) to load.
# `capture` does start, the scenario, the page, the wait and the shot, then
# stop; scenarios stay in effect until `demo/scenario.sh recover`.
#
# Work files go to $ICYGUI_SHOT_DIR (default target/demo-shots); the app's
# log is $ICYGUI_SHOT_DIR/home/.local/state/icygui/logs/icygui.log, its
# terminal output $ICYGUI_SHOT_DIR/app.log.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
COMPOSE=(docker compose -f "$HERE/docker-compose.yml")
NODES=(master-01 master-02 sat-ams-01 sat-fra-01 sat-fra-02)
STORE_SECRET=$ROOT/target/debug/examples/store_secret
# The demo API user's password (a documented demo value, docs/demo.md) and
# the throwaway keyring's unlock value (a test value: the keyring lives in
# the work directory and holds nothing else).
DEMO_PASSWORD=icygui-demo-password
KEYRING_UNLOCK=icygui-harness
DISPLAY_NUMBER=${DISPLAY_NUMBER:-:93}
WORK=${ICYGUI_SHOT_DIR:-$ROOT/target/demo-shots}
SETTLE=${SETTLE:-10}
APP=$ROOT/target/debug/icygui

die() { echo "screenshot: $*" >&2; exit 1; }

# The icygui window's id, or nothing (at once: no waiting for a window
# that will never come once the app has died).
window() {
  DISPLAY=$DISPLAY_NUMBER timeout 10 xdotool search --onlyvisible --name icygui 2>/dev/null | head -n1
}

# The session bus and keyring of a start: a dbus-daemon of the harness's
# own (its pid in $WORK/bus.pid) with gnome-keyring-daemon's Secret
# Service on it, unlocked, its files in the work directory's home. Prints
# the bus address. The keyring daemon ends with the bus.
session() {
  local out
  command -v dbus-daemon >/dev/null || die "needs dbus-daemon (Debian/Ubuntu: apt install dbus-daemon)"
  command -v gnome-keyring-daemon >/dev/null ||
    die "needs gnome-keyring-daemon (Debian/Ubuntu: apt install --no-install-recommends gnome-keyring)"
  out=$(in_home dbus-daemon --session --fork --print-address=1 --print-pid=1)
  sed -n 2p <<<"$out" >"$WORK/bus.pid"
  BUS=$(sed -n 1p <<<"$out")
  printf '%s' "$KEYRING_UNLOCK" |
    in_home env DBUS_SESSION_BUS_ADDRESS="$BUS" gnome-keyring-daemon --unlock --components=secrets --daemonize >/dev/null
}

# Runs a command with the work directory's home and XDG directories.
in_home() {
  env HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/home/.config" XDG_DATA_HOME="$WORK/home/.local/share" \
    XDG_STATE_HOME="$WORK/home/.local/state" XDG_RUNTIME_DIR="$WORK/home/run" "$@"
}

stop_session() {
  if [ -f "$WORK/bus.pid" ]; then
    kill "$(cat "$WORK/bus.pid")" 2>/dev/null || true
    rm -f "$WORK/bus.pid"
  fi
}

# The cluster for a start: started when there is none; otherwise used as
# it is and only waited for, never restarted (`up` would undo a stopped or
# frozen node, and wait 15 minutes for a frozen one).
cluster() {
  local recover=$1 node id status problems=()
  if [ -z "$("${COMPOSE[@]}" ps -a -q master-01 2>/dev/null)" ]; then
    "$HERE/up.sh" >/dev/null
    return
  fi
  for node in "${NODES[@]}"; do
    id=$("${COMPOSE[@]}" ps -a -q "$node")
    status=$(docker inspect -f '{{.State.Status}}' "$id" 2>/dev/null || echo missing)
    [ "$status" = running ] || problems+=("$node is $status")
  done
  if [ ${#problems[@]} -gt 0 ]; then
    [ "$recover" = 1 ] || die "a scenario or a cluster test is in effect (${problems[*]});" \
      "run demo/scenario.sh recover, or start with --recover"
    "$HERE/scenario.sh" recover >/dev/null
    return
  fi
  local deadline=$(( $(date +%s) + 300 )) waiting
  while :; do
    waiting=()
    for node in "${NODES[@]}"; do
      id=$("${COMPOSE[@]}" ps -q "$node")
      [ "$(docker inspect -f '{{.State.Health.Status}}' "$id" 2>/dev/null)" = healthy ] || waiting+=("$node")
    done
    [ ${#waiting[@]} -eq 0 ] && return
    [ "$(date +%s)" -lt "$deadline" ] || die "not healthy after 5 minutes: ${waiting[*]}"
    sleep 3
  done
}

start() {
  local theme=dark select=overview/problems proxy=() build=1 keep=0 warm=1 recover=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --theme) theme=$2; shift 2 ;;
      --select) select=$2; shift 2 ;;
      --via-proxy) proxy=(--via-proxy); shift ;;
      --no-build) build=0; shift ;;
      --keep) keep=1; shift ;;
      --no-warm) warm=0; shift ;;
      --recover) recover=1; shift ;;
      *) die "start: unknown option $1" ;;
    esac
  done
  stop_app
  cluster "$recover"
  if [ "$build" = 1 ]; then
    (cd "$ROOT" && cargo build -p ic-app --quiet && cargo build -p ic-platform --example store_secret --quiet)
  fi
  [ -x "$APP" ] || die "no $APP; build it with cargo build -p ic-app"
  [ -x "$STORE_SECRET" ] || die "no $STORE_SECRET; build it with cargo build -p ic-platform --example store_secret"

  # A fresh home each time; --keep keeps the data directory (the event
  # log behind the event streams and the panes' history) of the last start.
  if [ "$keep" = 1 ] && [ -d "$WORK/home/.local/share/icygui" ]; then
    rm -rf "${WORK:?}/home/.config" "$WORK/home/.local/state" "$WORK/home/run" "$WORK/home/.local/share/keyrings"
  else
    rm -rf "${WORK:?}/home"
  fi
  mkdir -p "$WORK/home/.config/icygui" "$WORK/home/.local/share/icygui" "$WORK/home/.local/state" "$WORK/home/run"
  chmod 700 "$WORK/home/run"
  (cd "$ROOT" && cargo xtask demo-config --config-dir "$WORK/home/.config/icygui" \
    --data-dir "$WORK/home/.local/share/icygui" \
    --select "$select" --theme "$theme" "${proxy[@]}" >/dev/null)

  # The demo password into the session's keyring, through icygui's own
  # secret store code (the environment's id from environment.toml).
  session
  local id
  id=$(sed -n 's/^id = "\(.*\)"$/\1/p' "$HERE/icygui/environment.toml" | head -n1)
  printf '%s' "$DEMO_PASSWORD" | in_home env DBUS_SESSION_BUS_ADDRESS="$BUS" "$STORE_SECRET" "$id" ||
    die "could not store the demo password in the session's keyring"

  if ! xdotool_ok; then
    Xvfb "$DISPLAY_NUMBER" -screen 0 1600x1000x24 -nolisten tcp >"$WORK/xvfb.log" 2>&1 &
    echo $! >"$WORK/xvfb.pid"
    sleep 1
  fi

  in_home env -u WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS="$BUS" \
    DISPLAY="$DISPLAY_NUMBER" ICYGUI_WINDOW_CONTROLS=always RUST_LOG="${RUST_LOG:-info}" \
    "$APP" >"$WORK/app.log" 2>&1 &
  echo $! >"$WORK/app.pid"

  # The engine's own line once it is connected and its stream is open
  # (ic-core's "connected to Icinga"; not "not connected", "disconnected"
  # or "reconnecting").
  local log="$WORK/home/.local/state/icygui/logs/icygui.log"
  for _ in $(seq 1 120); do
    kill -0 "$(cat "$WORK/app.pid")" 2>/dev/null || die "icygui exited; see $WORK/app.log"
    if grep -qs ': connected to Icinga' "$log" && [ -n "$(window)" ]; then
      if [ "$warm" = 1 ]; then
        sleep 2
        "$HERE/scenario.sh" blips >/dev/null
      fi
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
  stop_session
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
        --via-proxy | --no-build | --keep | --no-warm | --recover) start_args+=("$1"); shift ;;
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
    sed -n '2,36p' "$0" >&2
    exit 2
    ;;
esac
