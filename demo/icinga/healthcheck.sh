#!/bin/sh
# The demo nodes' health check (docker-compose.yml): healthy once the
# node's API answers and it is connected to every endpoint it talks to
# (its zone's, its parent's and, on a master, its child zones'). On
# master-01 also once the seed (seed.py) is done, so `up --wait` returns
# a cluster with its acknowledgements, comments and downtimes in place.
set -eu
curl -fsSk --max-time 3 -u healthcheck:healthcheck -H 'Accept: application/json' \
  https://localhost:5665/v1/status/ApiListener |
  python3 -Ic 'import json, sys
api = json.load(sys.stdin)["results"][0]["status"]["api"]
missing = api["not_conn_endpoints"]
if missing:
    sys.exit("not connected to " + ", ".join(missing))'
if [ "$(hostname)" = master-01 ] && [ ! -f /tmp/seeded ]; then
  echo "seeding (see /var/log/icinga2/seed.log)"
  exit 1
fi
