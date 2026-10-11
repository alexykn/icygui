#!/usr/bin/env bash
# Runs the background-mode spike headless on Linux and checks its log.
#
# Needs: Xvfb, a software Vulkan driver (mesa-vulkan-drivers), dbus,
# dunst (notification server), gdbus. Builds nothing; run
# `cargo build -p spikes --bin background` first.
#
# What it drives from the outside, like a user would:
#   1. the notification's "Acknowledge" button (dunst context menu),
#   2. the tray menu's "Open" item (dbusmenu call on the StatusNotifierItem),
# and then a second run with GPUI's default quit mode for comparison.
set -euo pipefail

BIN=${BIN:-target/debug/background}

if [[ "${1:-}" != --inner ]]; then
  WORK=$(mktemp -d)
  export WORK
  export DISPLAY=:${DISPLAY_NUM:-98}
  export XDG_RUNTIME_DIR=$WORK/xdg
  mkdir -m 700 "$XDG_RUNTIME_DIR"
  unset WAYLAND_DISPLAY
  Xvfb "$DISPLAY" -screen 0 1280x800x24 &>/dev/null &
  XVFB=$!
  trap 'kill $XVFB 2>/dev/null || true' EXIT
  sleep 1
  dbus-run-session -- "$0" --inner
  exit $?
fi

LOG=$WORK/spike.log
pass=0
fail=0
check() { # check <description> <pattern> [log]
  if grep -qE -- "$2" "${3:-$LOG}"; then
    echo "PASS  $1"
    pass=$((pass + 1))
  else
    echo "FAIL  $1"
    fail=$((fail + 1))
  fi
}
wait_for() { # wait_for <pattern> [seconds]
  for _ in $(seq 1 $((${2:-15} * 10))); do
    grep -qE -- "$1" "$LOG" && return 0
    sleep 0.1
  done
  echo "timed out waiting for: $1" >&2
  echo "--- spike log ---" >&2; cat "$LOG" >&2 || true
  echo "--- dunst log ---" >&2; cat "$WORK/dunst.log" >&2 || true
  return 1
}

# dunst opens its context menu through `dmenu`; this one picks "Acknowledge".
cat >"$WORK/pick-ack.sh" <<'EOF'
#!/bin/sh
tee "$WORK/dmenu-input.txt" | grep -m1 'Acknowledge'
EOF
chmod +x "$WORK/pick-ack.sh"
printf '[global]\n    dmenu = %s\n' "$WORK/pick-ack.sh" >"$WORK/dunstrc"
dunst -config "$WORK/dunstrc" &>"$WORK/dunst.log" &
# Wait until dunst owns the notification name on the bus instead of guessing
# a delay: on a slow runner a fixed sleep lost the race and the spike's
# notification went nowhere.
for _ in $(seq 1 100); do
  gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus \
    --method org.freedesktop.DBus.NameHasOwner org.freedesktop.Notifications 2>/dev/null |
    grep -q true && break
  sleep 0.1
done

echo "== run 1: QuitMode::Explicit =="
NO_COLOR=1 "$BIN" --auto 12 >"$LOG" 2>&1 &
SPIKE=$!

wait_for 'notification posted'
sleep 0.5
dunstctl context # → pick-ack.sh → ActionInvoked("Acknowledge")
wait_for 'notification response received' 5 || true

item=$(gdbus call --session --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.ListNames |
  grep -oE "org\.kde\.StatusNotifierItem-[0-9]+-[0-9]+" | head -n1 || true)
if [[ -n "$item" ]]; then
  echo "tray item: $item"
  layout=$(gdbus call --session --dest "$item" --object-path /MenuBar \
    --method com.canonical.dbusmenu.GetLayout -- 0 -1 '@as []')
  open_id=$(grep -oE "\(([0-9]+), \{[^}]*'label': <'Open'>" <<<"$layout" |
    grep -oE '^\([0-9]+' | tr -d '(' || true)
  echo "menu item 'Open' id: ${open_id:-not found}"
  if [[ -n "$open_id" ]]; then
    gdbus call --session --dest "$item" --object-path /MenuBar \
      --method com.canonical.dbusmenu.Event -- "$open_id" clicked '<0>' 0 >/dev/null
  fi
fi
wait_for 'window opened reason="tray"' 5 || true
wait "$SPIKE" || true

check "tray icon created" 'tray icon created'
check "window closed, no windows left" 'window closed open_windows=0'
check "app alive after its last window closed" 'alive after last window closed open_windows=0'
check "notification posted with no window open" 'notification posted reason="timeline"'
check "notification action reached the app" 'notification response received tag=spike-background action=Some\("ack"\)'
check "tray menu click reached the app" 'tray menu clicked id=open'
check "window reopened from the tray" 'window opened reason="tray"'
check "app quit on request" 'timeline done, quitting'
check "dunst showed the Acknowledge action" 'Acknowledge' "$WORK/dmenu-input.txt"

echo "== run 2: QuitMode::Default (expected to quit with the last window on Linux) =="
NO_COLOR=1 "$BIN" --auto 6 --quit-mode default >"$LOG.default" 2>&1 || true
if grep -q 'alive after last window closed' "$LOG.default"; then
  echo "NOTE  default quit mode kept the app alive"
else
  echo "NOTE  default quit mode quit after the last window closed"
fi

echo "== $pass passed, $fail failed (logs in $WORK) =="
[[ $fail -eq 0 ]]
