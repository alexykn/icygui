#!/usr/bin/env bash
# Starts the demo cluster (demo/docker-compose.yml) if it isn't running,
# waits until every node is healthy (connected, seeded), and prints the
# environment variables of the tests that run against it:
#
#   set -a; eval "$(demo/up.sh)"; set +a
#   cargo test -p ic-api --test contract                               # the contract tests
#   cargo test -p ic-core --test engine cluster:: -- --test-threads=1  # the cluster tests
#
# Idempotent: a running cluster is only waited for (stopped nodes are
# started again). The CA certificate goes to $ICYGUI_DEMO_CA (default:
# target/demo-ca.crt in this repository, replaced in one step, so every
# call reuses the one file). The passwords printed are the demo's
# documented demo values.
#   ICYGUI_DEMO_TIMEOUT  seconds to wait (default 1200: a slow 2-core CI
#                        runner takes about 2 minutes, the seed gives up
#                        its own waits after 10)
#
# Healthy means every node is connected and seeded, and every object with
# active checks has been checked at least once: Icinga 2.15 sometimes
# leaves first checks unrun in the HA zone fra on a loaded machine (their
# `next_check` passes, nothing runs them). The seed reschedules those
# after its wait; if some are still unchecked here, up.sh names them,
# reschedules them again (every minute) and waits, and fails naming them
# when the timeout ends: it never says healthy while one never ran (the
# contract tests would fail with `never checked`).
#
# While it waits it says every minute what each node still lacks (its
# health check's output: the endpoints not connected, or the seed's
# latest line). It stops at once when a node exits or the seed failed,
# and on any failure prints `docker compose ps`, each node's health check
# and last log lines and the seed's log, as GitHub annotations too when
# it runs in GitHub Actions (the job's log isn't always at hand), so a
# failed start explains itself.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
COMPOSE=(docker compose -f "$HERE/docker-compose.yml")
NODES=(master-01 master-02 sat-ams-01 sat-fra-01 sat-fra-02)
TIMEOUT=${ICYGUI_DEMO_TIMEOUT:-1200}

# A node's latest health check output, on one line.
health_output() {
  docker inspect -f '{{json .State.Health}}' "$1" 2>/dev/null |
    python3 -Ic 'import json, sys
health = json.load(sys.stdin) or {}
log = health.get("Log") or []
lines = log[-1]["Output"].splitlines() if log else []
print(" | ".join(line.strip() for line in lines if line.strip()))' 2>/dev/null || true
}

# Why the start failed, for a person and for GitHub: `docker compose ps`,
# then each node's health check and last log lines, then the seed's log.
explain() {
  local why=$1 node id state output lines notable
  echo "up.sh: $why" >&2
  "${COMPOSE[@]}" ps -a >&2 || true
  for node in "${NODES[@]}"; do
    id=$("${COMPOSE[@]}" ps -a -q "$node" 2>/dev/null || true)
    state=$(docker inspect -f '{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}} exit {{.State.ExitCode}}' "$id" 2>/dev/null || echo missing)
    output=$(health_output "$id")
    lines=$("${COMPOSE[@]}" logs --no-color --tail 40 "$node" 2>&1 || true)
    {
      echo "--- $node: $state"
      echo "    health check: ${output:-none}"
      printf '%s\n' "$lines" | tail -n 40
    } >&2
    if [ "${GITHUB_ACTIONS:-}" = true ] && [ "$state" != "running healthy exit 0" ]; then
      # An annotation per node that isn't healthy: its health check and
      # its last lines that aren't routine (`information/`). Workflow
      # commands take newlines as %0A (and % itself escaped first).
      notable=$(printf '%s\n' "$lines" | grep -v 'information/' | tail -n 15 || true)
      [ -n "$notable" ] || notable=$(printf '%s\n' "$lines" | tail -n 5)
      printf '::error title=demo cluster: %s (%s)::%s\n' "$node" "$state" \
        "$(printf 'health check: %s\n%s' "${output:-none}" "$notable" |
          sed -e 's/%/%25/g' -e 's/\r/%0D/g' | awk 'BEGIN { ORS = "%0A" } { print }')"
    fi
  done
  echo "--- master-01: /var/log/icinga2/seed.log" >&2
  "${COMPOSE[@]}" exec -T master-01 tail -n 40 /var/log/icinga2/seed.log >&2 2>/dev/null || true
  if [ "${GITHUB_ACTIONS:-}" = true ]; then
    printf '::error title=demo cluster: %s::%s\n' "start failed" "$why"
  fi
}

"${COMPOSE[@]}" up -d >&2

# `up --wait` gives up at once on a node that was unhealthy before (a peer
# was stopped by a scenario), so wait here instead.
started=$(date +%s)
deadline=$(( started + TIMEOUT ))
said=$started
while :; do
  waiting=()
  for node in "${NODES[@]}"; do
    id=$("${COMPOSE[@]}" ps -a -q "$node")
    status=$(docker inspect -f '{{.State.Status}}' "$id" 2>/dev/null || echo missing)
    health=$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "$id" 2>/dev/null || echo missing)
    case "$status" in
      exited | dead | missing)
        explain "$node is $status (it won't become healthy)"
        exit 1
        ;;
    esac
    if [ "$node" = master-01 ] && health_output "$id" | grep -q '^seeding failed'; then
      explain "the seed failed on master-01"
      exit 1
    fi
    [ "$health" = healthy ] || waiting+=("$node:$health")
  done
  [ ${#waiting[@]} -eq 0 ] && break
  now=$(date +%s)
  if [ "$now" -ge "$deadline" ]; then
    explain "not healthy after ${TIMEOUT}s: ${waiting[*]}"
    exit 1
  fi
  if [ $(( now - said )) -ge 60 ]; then
    said=$now
    for item in "${waiting[@]}"; do
      node=${item%%:*}
      echo "up.sh: $(( now - started ))s, $item: $(health_output "$("${COMPOSE[@]}" ps -a -q "$node")")" >&2
    done
  fi
  sleep 3
done

# never_checked [--reschedule]: one line from the seed's helper on
# master-01, the count first (`0`: every check ran).
never_checked() {
  "${COMPOSE[@]}" exec -T master-01 python3 -I /demo/icinga/seed.py --never-checked "$@" </dev/null 2>&1 |
    tail -n 1 || true
}

# Connected and seeded is not checked: wait until every object with active
# checks ran once, rescheduling what didn't (see the top).
rescheduled=0
while :; do
  line=$(never_checked)
  [ "$line" = 0 ] && break
  now=$(date +%s)
  if [ "$now" -ge "$deadline" ]; then
    explain "not healthy after ${TIMEOUT}s: ${line:-the never-checked query gave no answer}"
    exit 1
  fi
  if [ $(( now - rescheduled )) -ge 60 ] && [ "${line#\?}" = "$line" ]; then
    rescheduled=$now
    echo "up.sh: $(( now - started ))s, $(never_checked --reschedule) (forced, they run now)" >&2
  fi
  sleep 5
done
echo "up.sh: healthy after $(( $(date +%s) - started ))s (connected, seeded, every check ran)" >&2

CA=${ICYGUI_DEMO_CA:-$HERE/../target/demo-ca.crt}
mkdir -p "$(dirname "$CA")"
CA=$(cd "$(dirname "$CA")" && pwd)/$(basename "$CA")
staged=$(mktemp "$CA.XXXXXX")
trap 'rm -f "$staged"' EXIT
"${COMPOSE[@]}" exec -T master-01 cat /var/lib/icinga2/certs/ca.crt >"$staged"
mv -f "$staged" "$CA"
cat <<EOF
ICYGUI_CONTRACT_URL=https://127.0.0.1:${ICYGUI_DEMO_PORT_1:-5665}
ICYGUI_CONTRACT_USER=icygui
ICYGUI_CONTRACT_PASSWORD=icygui-test
ICYGUI_CONTRACT_ADMIN_USER=demo-admin
ICYGUI_CONTRACT_ADMIN_PASSWORD=demo-admin-password
ICYGUI_CONTRACT_CA_FILE=$CA
ICYGUI_CONTRACT_SERVER_NAME=master-01
ICYGUI_CLUSTER_USER=icygui-demo
ICYGUI_CLUSTER_PASSWORD=icygui-demo-password
ICYGUI_CLUSTER_URL_2=https://127.0.0.1:${ICYGUI_DEMO_PORT_2:-5666}
ICYGUI_CLUSTER_SERVER_NAME_2=master-02
ICYGUI_CLUSTER_SCENARIO=$HERE/scenario.sh
EOF
