#!/usr/bin/env bash
# Starts a real Icinga 2 master in Docker with the icygui contract-test
# fixtures, waits for its API and prints the environment variables the
# contract tests read. Usage: contract/run-icinga.sh [image-tag]
#   ICINGA_PORT (default 15665), ICINGA_CONTAINER (default icygui-contract)
set -euo pipefail

TAG=${1:-2.15}
PORT=${ICINGA_PORT:-15665}
NAME=${ICINGA_CONTAINER:-icygui-contract}
HERE=$(cd "$(dirname "$0")" && pwd)

docker rm -f "$NAME" >/dev/null 2>&1 || true
docker volume rm -f "$NAME-data" >/dev/null 2>&1 || true
docker volume create "$NAME-data" >/dev/null

# First start runs the master setup (CA, certificates, API feature).
docker run -d --name "$NAME" -h icinga-master -e ICINGA_MASTER=1 \
  -p "127.0.0.1:$PORT:5665" -v "$NAME-data:/data" "icinga/icinga2:$TAG" >/dev/null
for _ in $(seq 1 60); do
  docker exec "$NAME" test -f /data/etc/icinga2/features-enabled/api.conf 2>/dev/null && break
  sleep 1
done
docker stop "$NAME" >/dev/null

# Install the fixtures with a helper container on the same volume.
docker run --rm -v "$NAME-data:/data" -v "$HERE/icinga:/fixtures:ro" --entrypoint sh \
  "icinga/icinga2:$TAG" -c '
    cp /fixtures/icygui-test.conf /data/etc/icinga2/conf.d/icygui-test.conf
    cp /fixtures/services.conf /data/etc/icinga2/conf.d/services.conf
    cp /fixtures/api.conf /data/etc/icinga2/features-enabled/api.conf
    icinga2 daemon -C >/dev/null'
docker start "$NAME" >/dev/null

for _ in $(seq 1 90); do
  if curl -sfk -u icygui:icygui-test -H 'Accept: application/json' "https://127.0.0.1:$PORT/v1" >/dev/null; then
    CA=$(mktemp)
    docker exec "$NAME" cat /var/lib/icinga2/certs/ca.crt >"$CA"
    echo "ICYGUI_CONTRACT_URL=https://127.0.0.1:$PORT"
    echo "ICYGUI_CONTRACT_USER=icygui"
    echo "ICYGUI_CONTRACT_PASSWORD=icygui-test"
    echo "ICYGUI_CONTRACT_ADMIN_USER=root"
    echo "ICYGUI_CONTRACT_ADMIN_PASSWORD=$(docker exec "$NAME" sed -n 's/.*password = "\(.*\)".*/\1/p' /data/etc/icinga2/conf.d/api-users.conf | head -n1)"
    echo "ICYGUI_CONTRACT_CA_FILE=$CA"
    echo "ICYGUI_CONTRACT_SERVER_NAME=icinga-master"
    exit 0
  fi
  sleep 1
done
echo "Icinga API did not come up; logs:" >&2
docker logs "$NAME" 2>&1 | tail -40 >&2
exit 1
