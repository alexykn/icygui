#!/usr/bin/env bash
# Reproduces the numbers in docs/performance.md against a real Icinga 2 in
# Docker: N hosts x 15 services, load sizes/times, and a forced re-check burst
# on the event stream. Usage: contract/scale/benchmark.sh [hosts] (default 2000)
set -euo pipefail

HOSTS=${1:-2000}
PORT=${ICINGA_PORT:-15666}
NAME=icygui-scale
HERE=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

python3 "$HERE/scale/generate.py" "$HOSTS" >"$WORK/scale.conf"
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
docker run --rm -v "$NAME-data:/data" -v "$HERE/icinga:/fixtures:ro" -v "$WORK/scale.conf:/scale.conf:ro" \
  --entrypoint sh icinga/icinga2:2.15 -c '
    cp /fixtures/icygui-test.conf /fixtures/services.conf /data/etc/icinga2/conf.d/
    cp /fixtures/api.conf /data/etc/icinga2/features-enabled/api.conf
    cp /scale.conf /data/etc/icinga2/conf.d/scale.conf'
docker start "$NAME" >/dev/null
B="https://127.0.0.1:$PORT/v1"
for _ in $(seq 1 120); do
  curl -sfk -u icygui:icygui-test -H 'Accept: application/json' "$B" >/dev/null && break
  sleep 1
done
ADMIN=$(docker exec "$NAME" sed -n 's/.*password = "\(.*\)".*/\1/p' /data/etc/icinga2/conf.d/api-users.conf | head -n1)

# Burst: force every check while recording the event stream.
timeout 60 curl -sk -N -u icygui:icygui-test -H 'Accept: application/json' -X POST "$B/events" \
  -d '{"queue":"bench","types":["CheckResult","StateChange"]}' >"$WORK/events.ndjson" &
sleep 2
for type in Service Host; do
  curl -sk -u "root:$ADMIN" -H 'Accept: application/json' -X POST "$B/actions/reschedule-check" \
    -d "{\"type\":\"$type\",\"force\":true}" >/dev/null
done
sleep 10
echo "burst: $(wc -l <"$WORK/events.ndjson") events, $(($(wc -c <"$WORK/events.ndjson") / 1000000)) MB within ~10 s"
sleep 5

query() { # query <label> <type> <body>
  local start end
  start=$(date +%s.%N)
  curl -sk -u icygui:icygui-test -H 'Accept: application/json' -X POST -H 'X-HTTP-Method-Override: GET' \
    "$B/objects/$2" -d "$3" -o "$WORK/out.json"
  end=$(date +%s.%N)
  python3 -c "
import json, os, sys
n = len(json.load(open('$WORK/out.json'))['results'])
print(f'$1: {n} objects, {os.path.getsize(\"$WORK/out.json\") / 1e6:.1f} MB, {$end - $start:.2f} s')"
}
LEAN='["__name","host_name","display_name","state","state_type","last_state_change","last_hard_state_change","last_check","next_check","check_attempt","max_check_attempts","acknowledgement","acknowledgement_expiry","downtime_depth","flapping","last_reachable","check_interval","retry_interval","groups","vars"]'
query "hosts (full)" hosts '{}'
query "services (all attributes)" services '{}'
query "services (lean)" services "{\"attrs\":$LEAN}"
docker stats --no-stream --format 'icinga: cpu {{.CPUPerc}}, memory {{.MemUsage}}' "$NAME"
