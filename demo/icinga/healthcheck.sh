#!/bin/sh
# The demo nodes' health check (docker-compose.yml): healthy once the
# node's API answers and it is connected to every endpoint it talks to
# (its zone's, its parent's and, on a master, its child zones'). On
# master-01 also once the seed (seed.py) is done, so `up --wait` returns
# a cluster with its acknowledgements, comments and downtimes in place.
#
# Sized for a slow 2-core CI runner, where Icinga may take seconds to
# answer while it starts (docker-compose.yml gives the check 15 s). Its
# output (`docker inspect`'s health log, demo/up.sh prints it when a start
# fails) says what is missing: the endpoints not connected, or the seed's
# latest line.
set -eu
curl -fsSk --max-time 10 -u healthcheck:healthcheck -H 'Accept: application/json' \
  https://localhost:5665/v1/status/ApiListener |
  python3 -Ic 'import json, sys
api = json.load(sys.stdin)["results"][0]["status"]["api"]
missing = api["not_conn_endpoints"]
if missing:
    sys.exit("not connected to " + ", ".join(missing))'
if [ "$(hostname)" = master-01 ] && [ ! -f /tmp/seeded ]; then
  if [ -f /tmp/seeded.failed ]; then
    echo "seeding failed:"
    tail -n 15 /var/log/icinga2/seed.log 2>/dev/null || true
  else
    echo "seeding: $(tail -n 1 /var/log/icinga2/seed.log 2>/dev/null || true)"
  fi
  exit 1
fi
