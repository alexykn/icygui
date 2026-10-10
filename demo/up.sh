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
#   ICYGUI_DEMO_TIMEOUT  seconds to wait (default 900)
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
COMPOSE=(docker compose -f "$HERE/docker-compose.yml")
NODES=(master-01 master-02 sat-ams-01 sat-fra-01 sat-fra-02)
TIMEOUT=${ICYGUI_DEMO_TIMEOUT:-900}

"${COMPOSE[@]}" up -d >&2

# `up --wait` gives up at once on a node that was unhealthy before (a peer
# was stopped by a scenario), so wait here instead.
deadline=$(( $(date +%s) + TIMEOUT ))
while :; do
  waiting=()
  for node in "${NODES[@]}"; do
    id=$("${COMPOSE[@]}" ps -q "$node")
    health=$(docker inspect -f '{{.State.Health.Status}}' "$id" 2>/dev/null || echo missing)
    [ "$health" = healthy ] || waiting+=("$node:$health")
  done
  [ ${#waiting[@]} -eq 0 ] && break
  if [ "$(date +%s)" -ge "$deadline" ]; then
    echo "up.sh: not healthy after ${TIMEOUT}s: ${waiting[*]}" >&2
    for item in "${waiting[@]}"; do
      node=${item%%:*}
      echo "--- $node" >&2
      "${COMPOSE[@]}" logs --tail 30 "$node" >&2 || true
    done
    exit 1
  fi
  sleep 3
done

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
