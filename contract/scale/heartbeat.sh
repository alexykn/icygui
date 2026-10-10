#!/usr/bin/env bash
# Calibrates the heartbeat's time budget (docs/performance.md, *Heartbeat
# time budget*) against a real Icinga 2 in a Docker container of its own,
# never a production one: a 10 s and a 30 s heartbeat (always-OK `dummy`
# checks), recorded on a live and a quiet (filtered) event stream.
#   idle: the heartbeats alone, MINUTES (default 30) minutes;
#   load: plus HOSTS (default 200) hosts x 15 services checked every 30 s
#         (about 100 checks a second) and, every five minutes, a burst of
#         2 x HOSTS state changes.
# Usage: contract/scale/heartbeat.sh [idle|load|both] (default both).
# The raw beats go to $OUT (default ./heartbeat-<phase>.csv).
set -euo pipefail

PHASES=${1:-both}
MINUTES=${MINUTES:-30}
HOSTS=${HOSTS:-200}
PORT=${ICINGA_PORT:-15667}
NAME=icygui-heartbeat
HERE=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
OUT=${OUT:-.}
trap 'rm -rf "$WORK"; docker rm -f "$NAME" >/dev/null 2>&1 || true; docker volume rm -f "$NAME-data" >/dev/null 2>&1 || true' EXIT

cat >"$WORK/heartbeat.conf" <<'CONF'
/* The heartbeats being calibrated: always OK, a new output each run. */
object Host "icygui-hb" {
  check_command = "dummy"
  check_interval = 60s
  vars.dummy_text = "heartbeat host"
}
template Service "icygui-heartbeat" {
  host_name = "icygui-hb"
  check_command = "dummy"
  max_check_attempts = 1
  vars.dummy_state = 0
  vars.dummy_text = {{ "icygui heartbeat " + get_time() }}
  vars.icygui_heartbeat = true
}
object Service "beat-10s" {
  import "icygui-heartbeat"
  check_interval = 10s
  retry_interval = 10s
}
object Service "beat-30s" {
  import "icygui-heartbeat"
  check_interval = 30s
  retry_interval = 30s
}
CONF
python3 "$HERE/scale/generate.py" "$HOSTS" \
  | sed -e 's/check_interval = 300s/check_interval = 30s/' -e 's/retry_interval = 60s/retry_interval = 15s/' \
  >"$WORK/load.conf"
# The helper container runs as Icinga's user.
chmod 755 "$WORK"
chmod 644 "$WORK"/*.conf

start() { # start <with load: 0|1>
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker volume rm -f "$NAME-data" >/dev/null 2>&1 || true
  docker volume create "$NAME-data" >/dev/null
  docker run -d --name "$NAME" -h icinga-master -e ICINGA_MASTER=1 \
    -p "127.0.0.1:$PORT:5665" -v "$NAME-data:/data" icinga/icinga2:2.15 >/dev/null
  for _ in $(seq 1 60); do
    docker exec "$NAME" test -f /data/etc/icinga2/features-enabled/api.conf 2>/dev/null && break
    sleep 1
  done
  docker stop "$NAME" >/dev/null
  docker run --rm -v "$NAME-data:/data" -v "$HERE/icinga:/fixtures:ro" -v "$WORK:/work:ro" \
    --entrypoint sh icinga/icinga2:2.15 -c "
      cp /fixtures/icygui-test.conf /fixtures/icygui-groups.conf /fixtures/services.conf /work/heartbeat.conf /data/etc/icinga2/conf.d/
      cp /fixtures/api.conf /data/etc/icinga2/features-enabled/api.conf
      if [ $1 = 1 ]; then cp /work/load.conf /data/etc/icinga2/conf.d/load.conf; fi"
  docker start "$NAME" >/dev/null
  for _ in $(seq 1 120); do
    curl -sfk --noproxy '*' -u icygui:icygui-test -H 'Accept: application/json' "https://127.0.0.1:$PORT/v1" >/dev/null && break
    sleep 1
  done
  # Every check's first run lands within a minute of the start.
  sleep 70
  # The image may rewrite the admin's password while starting: read it
  # once the admin can log in with it.
  for _ in $(seq 1 30); do
    ADMIN=$(docker exec "$NAME" sed -n 's/.*password = "\(.*\)".*/\1/p' /data/etc/icinga2/conf.d/api-users.conf | head -n1)
    curl -sfk --noproxy '*' -u "root:$ADMIN" -H 'Accept: application/json' "https://127.0.0.1:$PORT/v1" >/dev/null && break
    sleep 2
  done
}

run() { # run <phase>
  local load=0 burst=""
  if [ "$1" = load ]; then load=1; burst=burst; fi
  start "$load"
  python3 "$HERE/scale/heartbeat.py" "https://127.0.0.1:$PORT" icygui icygui-test root "$ADMIN" \
    "$((MINUTES * 60))" "$1" "$OUT/heartbeat-$1.csv" $burst
  docker stats --no-stream --format "icinga ($1): cpu {{.CPUPerc}}, memory {{.MemUsage}}" "$NAME"
}

case "$PHASES" in
  idle | load) run "$PHASES" ;;
  both) run idle; run load ;;
  *) echo "usage: $0 [idle|load|both]" >&2; exit 2 ;;
esac
