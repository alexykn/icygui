#!/usr/bin/env bash
# Many icygui clients starting at once against a real Icinga 2 in Docker
# (PERF-09, docs/performance.md): N hosts x 15 services like benchmark.sh,
# then CLIENTS engines (default 20) start together, first as users opening
# the app (no wait), then, on a freshly restarted Icinga, as background
# starts (launch at login: a random wait proportional to the number of
# services). Prints when the clients were connected and the master's peak
# memory. The container and its volume are removed at the end.
# Usage: contract/scale/starts.sh [hosts] [clients]   (default 2000 20)
# Never point the measurement at a production Icinga: it only runs against
# the container this script starts on 127.0.0.1.
set -euo pipefail

HOSTS=${1:-2000}
CLIENTS=${2:-20}
PORT=${ICINGA_PORT:-15667}
NAME=icygui-starts
HERE=$(cd "$(dirname "$0")/.." && pwd)
ROOT=$(cd "$HERE/.." && pwd)
WORK=$(mktemp -d)
cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
  docker volume rm -f "$NAME-data" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

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

B="https://127.0.0.1:$PORT/v1"
start_icinga() {
  docker start "$NAME" >/dev/null
  for _ in $(seq 1 180); do
    curl -sfk -u icygui:icygui-test -H 'Accept: application/json' "$B" >/dev/null && break
    sleep 1
  done
  # Its first checks run within a minute; the counts settle.
  sleep 60
}

measure() { # measure user|background
  if ICYGUI_SCALE_URL="https://127.0.0.1:$PORT" ICYGUI_SCALE_CONTAINER="$NAME" \
    ICYGUI_SCALE_CLIENTS="$CLIENTS" ICYGUI_SCALE_START="$1" \
    CARGO_INCREMENTAL=0 cargo test --manifest-path "$ROOT/Cargo.toml" --locked -p ic-core \
    --test engine starts -- --ignored --nocapture >"$WORK/test.log" 2>&1; then
    grep -E 'starts,|master memory' "$WORK/test.log"
  else
    tail -n 40 "$WORK/test.log"
    return 1
  fi
}

echo "== $HOSTS hosts, $CLIENTS clients"
start_icinga
docker stats --no-stream --format 'icinga idle: memory {{.MemUsage}}' "$NAME"
measure user
docker stop "$NAME" >/dev/null
start_icinga
docker stats --no-stream --format 'icinga idle: memory {{.MemUsage}}' "$NAME"
measure background
