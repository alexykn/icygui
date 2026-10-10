#!/usr/bin/env bash
# Breaks the demo cluster (demo/docker-compose.yml) in one of the ways
# icygui must notice, and says what icygui should show; `recover` mends
# everything. Only ever touches the containers of this compose project.
#
#   demo/scenario.sh list
#   demo/scenario.sh <scenario> [argument]
#   demo/scenario.sh recover
#
# The API calls go through `docker compose exec` with the demo's admin
# user (a documented demo value, docs/demo.md), so nothing needs to be
# installed on this machine besides Docker.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
COMPOSE=(docker compose -f "$HERE/docker-compose.yml")
ADMIN=demo-admin:demo-admin-password

usage() {
  cat <<'EOF'
usage: demo/scenario.sh <scenario> [argument] | recover | list

  satellite-down        stop sat-fra-01 (zone fra keeps sat-fra-02)
  zone-cut-off          stop sat-fra-01 and sat-fra-02 (zone fra has no endpoint)
  master-down           stop master-02
  frozen-master         freeze master-01 (docker pause): its connections stay
                        open and say nothing
  checks-stopped [zone] stop every active check (in one zone: master, ams, fra)
  beat-late [seconds]   hold zone ams's heartbeat back once (12 s by default)
  problem-storm [count] a burst of hard CRITICAL results (40 services by default)
  blips [count]         a few database services fail and recover within seconds
                        (4 by default): lines for the event streams; demo/screenshot.sh
                        start runs it
  dead-network          the proxy in front of master-01 (port 5667) passes
                        nothing any more while connections stay open
  recover               undo all of the above, wait until the cluster is healthy
                        and no check is overdue (ICYGUI_DEMO_SETTLE seconds at most,
                        default 300)
EOF
}

say() { printf '%s\n' "$@"; }

# A master that runs (not stopped, not frozen): master-01, else master-02.
master() {
  local node
  for node in master-01 master-02; do
    if [ "$(docker inspect -f '{{.State.Status}}' "$(cid "$node")" 2>/dev/null)" = running ]; then
      echo "$node"
      return
    fi
  done
  echo "scenario: neither master is running" >&2
  return 1
}

# api METHOD PATH [JSON]: an API call as the admin, through a running
# master. METHOD QUERY is a POST that Icinga reads as a GET (a query with
# a body).
api() {
  local node method=$1 override=()
  node=$(master)
  if [ "$method" = QUERY ]; then
    method=POST override=(-H 'X-HTTP-Method-Override: GET')
  fi
  "${COMPOSE[@]}" exec -T "$node" curl -fsS --max-time 60 \
    --cacert /var/lib/icinga2/certs/ca.crt -u "$ADMIN" "${override[@]}" \
    -H 'Accept: application/json' -H 'Content-Type: application/json' \
    -X "$method" ${3:+-d "$3"} "https://$node:5665$2" </dev/null
}

# py CODE: Python in a running master (reads stdin), so this machine needs
# nothing but Docker.
py() { "${COMPOSE[@]}" exec -T "$(master)" python3 -Ic "$1"; }

cid() { "${COMPOSE[@]}" ps -a -q "$1"; }

# overdue: how many checks with active checks on are past Icinga's own
# next_update (overdue, icygui's late), as one number.
overdue() {
  local kind total=0 count
  for kind in hosts services; do
    count=$(api QUERY "/v1/objects/$kind" '{ "attrs": ["next_update", "enable_active_checks"] }' |
      py "import json, sys, time
now = time.time()
print(sum(1 for r in json.load(sys.stdin)['results']
          if r['attrs']['enable_active_checks'] and 0 < r['attrs']['next_update'] < now))") || count=0
    total=$((total + count))
  done
  echo "$total"
}

require_up() {
  if [ -z "$(cid master-01)" ]; then
    echo "scenario: the demo is not running; start it with" >&2
    echo "  docker compose -f demo/docker-compose.yml up -d --wait" >&2
    exit 1
  fi
}

scenario=${1:-}
case "$scenario" in
  "" | -h | --help | help | list)
    usage
    ;;

  satellite-down)
    require_up
    "${COMPOSE[@]}" stop sat-fra-01
    say "sat-fra-01 is stopped. icygui shows, within a minute or two:" \
      "- health: zone fra degraded (yellow), sat-fra-01 disconnected; sat-fra-02 runs zone fra's checks" \
      "- its pinned beat icygui-hb-fra!beat-sat-fra-01 comes back UNKNOWN from sat-fra-02:" \
      "  heartbeat sat-fra-01 dead: Remote Icinga instance 'sat-fra-01' is not connected to 'sat-fra-02'" \
      "  (one line with the endpoint, notified after the 2-minute grace); the zone beat stays on time" \
      "Undo: demo/scenario.sh recover"
    ;;

  zone-cut-off)
    require_up
    "${COMPOSE[@]}" stop sat-fra-01 sat-fra-02
    say "sat-fra-01 and sat-fra-02 are stopped. icygui shows:" \
      "- health: zone fra cut off (red), both endpoints disconnected; cluster-zone on the sat-fra hosts CRITICAL" \
      "- the zone fra beats go silent, so one alert: zone fra: sat-fra-01 and sat-fra-02 disconnected, heartbeat lost" \
      "- fra's checks turn late as their intervals pass (nobody runs them; the masters don't take over)" \
      "Undo: demo/scenario.sh recover"
    ;;

  master-down)
    require_up
    "${COMPOSE[@]}" stop master-02
    say "master-02 is stopped. icygui (connected to master-01) shows:" \
      "- health: master-02 disconnected, the master zone degraded; master-01 runs every master check" \
      "- the pinned beat icygui-hb-master!beat-master-02 UNKNOWN from master-01:" \
      "  heartbeat master-02 dead: Remote Icinga instance 'master-02' is not connected to 'master-01'" \
      "  (one line with the endpoint, notified after the 2-minute grace)" \
      "Undo: demo/scenario.sh recover"
    ;;

  frozen-master)
    require_up
    "${COMPOSE[@]}" pause master-01
    say "master-01 is frozen: connections to it stay open and say nothing. icygui shows:" \
      "- after about 2 minutes without a line: event stream stalled, reconnecting" \
      "- the reconnect fails over to master-02 (the footer names it); master-02 reports master-01" \
      "  disconnected and its pinned beat UNKNOWN: heartbeat master-01 dead: Remote Icinga instance" \
      "  'master-01' is not connected to 'master-02'" \
      "Undo: demo/scenario.sh recover"
    ;;

  checks-stopped)
    require_up
    zone=${2:-}
    if [ -n "$zone" ]; then
      case "$zone" in master | ams | fra) ;; *) echo "scenario: unknown zone $zone" >&2; exit 2 ;; esac
      filter="\"filter\": \"host.zone == \\\"$zone\\\"\","
      sfilter="\"filter\": \"service.zone == \\\"$zone\\\"\","
    else
      filter="" sfilter=""
    fi
    api POST /v1/objects/hosts "{ $filter \"attrs\": { \"enable_active_checks\": false } }" >/dev/null
    api POST /v1/objects/services "{ $sfilter \"attrs\": { \"enable_active_checks\": false } }" >/dev/null
    if [ -n "$zone" ]; then
      case "$zone" in
        master) endpoints="master-01 and master-02" ;;
        ams) endpoints=sat-ams-01 ;;
        fra) endpoints="sat-fra-01 and sat-fra-02" ;;
      esac
      say "Active checks are off in zone $zone (its endpoints stay connected). icygui shows:" \
        "- its heartbeats stop; the REST query finds their last check old:" \
        "  zone $zone runs no checks ($endpoints connected)" \
        "- the zone's checks turn late as their intervals pass"
    else
      say "Active checks are off everywhere, as with a hung checker. icygui shows:" \
        "- every heartbeat stops; the REST query finds them old: Icinga runs no checks" \
        "- the check rates on the health page fall to 0, the one-minute checks turn late"
    fi
    say "Undo: demo/scenario.sh recover"
    ;;

  beat-late)
    require_up
    seconds=${2:-12}
    # The next beat, due one interval (30 s) after the last, comes later.
    next=$(api QUERY /v1/objects/services \
      '{ "services": ["icygui-hb-ams!beat"], "attrs": ["last_check", "check_interval"] }' |
      py "import json, sys, time
attrs = json.load(sys.stdin)['results'][0]['attrs']
print(int(max(attrs['last_check'] + attrs['check_interval'], time.time()) + $seconds))")
    api POST /v1/actions/reschedule-check \
      "{ \"type\": \"Service\", \"service\": \"icygui-hb-ams!beat\", \"next_check\": $next }" >/dev/null
    say "Zone ams's next heartbeat (every 30 s) comes ${seconds} s late. icygui shows:" \
      "- the heartbeat row: zone ams late (yellow) for a few seconds, then on time again" \
      "- with the default 12 s nothing is raised: the beat is in before the REST query (10 s" \
      "  after the deadline) would look; with 25 s or more the query finds its last check old:" \
      "  zone ams runs no checks (sat-ams-01 connected), until the beat comes" \
      "Nothing to undo (one beat)."
    ;;

  problem-storm)
    require_up
    count=${2:-40}
    # One process in a master sends them all, so they land within a second
    # or two: three results each, past max_check_attempts, a hard state.
    sent=$(py "
import base64, http.client, json, random, socket, ssl
node = socket.gethostname()
auth = 'Basic ' + base64.b64encode(b'$ADMIN').decode()
context = ssl.create_default_context(cafile='/var/lib/icinga2/certs/ca.crt')
def call(path, body, query=False):
    connection = http.client.HTTPSConnection(node, 5665, context=context, timeout=60)
    headers = {'Authorization': auth, 'Accept': 'application/json', 'Content-Type': 'application/json'}
    if query:
        headers['X-HTTP-Method-Override'] = 'GET'
    connection.request('POST', path, json.dumps(body), headers)
    answer = json.loads(connection.getresponse().read())
    connection.close()
    return answer
names = sorted(r['name'] for r in call('/v1/objects/services', {'attrs': ['name'], 'filter':
    'service.state == 0 && service.vars.demo && service.vars.demo.profile == \"ok\" && host.vars.env == \"prod\"'
    ' && service.name in [\"http\", \"api-health\", \"kubelet\", \"k8s-pods\", \"pg-connections\"]'},
    query=True)['results'])
# A spread over the estate (web, API, databases, Kubernetes), not the
# alphabetically first.
names = random.Random(1).sample(names, min($count, len(names)))
for _ in range(3):
    for name in names:
        call('/v1/actions/process-check-result', {'type': 'Service', 'service': name, 'exit_status': 2,
             'plugin_output': 'CRITICAL - connection refused (problem storm)'})
print(len(names))
" </dev/null)
    say "$sent services went CRITICAL (hard) at once. icygui shows:" \
      "- a few desktop notifications, then one storm summary (the rest in the notification centre)" \
      "- the services recover with their next active check, within about a minute" \
      "Nothing to undo."
    ;;

  blips)
    require_up
    count=${2:-4}
    # Connection checks of the databases: half of them WARNING, half
    # CRITICAL, hard (three results each), and five seconds later OK again:
    # state changes and recoveries that the event streams (the databases
    # dashboard's "db events") show.
    sent=$(py "
import base64, http.client, json, random, socket, ssl, time
node = socket.gethostname()
auth = 'Basic ' + base64.b64encode(b'$ADMIN').decode()
context = ssl.create_default_context(cafile='/var/lib/icinga2/certs/ca.crt')
def call(path, body, query=False):
    connection = http.client.HTTPSConnection(node, 5665, context=context, timeout=60)
    headers = {'Authorization': auth, 'Accept': 'application/json', 'Content-Type': 'application/json'}
    if query:
        headers['X-HTTP-Method-Override'] = 'GET'
    connection.request('POST', path, json.dumps(body), headers)
    answer = json.loads(connection.getresponse().read())
    connection.close()
    return answer
names = sorted(r['name'] for r in call('/v1/objects/services', {'attrs': ['name'], 'filter':
    'service.state == 0 && service.vars.demo && service.vars.demo.profile == \"ok\" && host.vars.env == \"prod\"'
    ' && service.name in [\"pg-connections\", \"pg-slow-queries\", \"pgbouncer\", \"mysql-connections\",'
    ' \"mongodb-connections\", \"redis-connections\"]'
    ' && !service.acknowledgement && service.downtime_depth == 0'}, query=True)['results'])
names = random.Random(7).sample(names, min($count, len(names)))
outputs = {1: 'WARNING - answers take 1.8 s (demo blip)', 2: 'CRITICAL - connection refused (demo blip)'}
for _ in range(3):
    for index, name in enumerate(names):
        state = 1 + index % 2
        call('/v1/actions/process-check-result', {'type': 'Service', 'service': name,
             'exit_status': state, 'plugin_output': outputs[state]})
time.sleep(5)
for name in names:
    call('/v1/actions/process-check-result', {'type': 'Service', 'service': name, 'exit_status': 0,
         'plugin_output': 'OK - back to normal (demo blip)'})
print(len(names))
" </dev/null)
    say "$sent database services failed and recovered five seconds later. icygui shows:" \
      "- their problems and recoveries in the event streams (overview/databases' db events)" \
      "Nothing to undo."
    ;;

  dead-network)
    require_up
    "${COMPOSE[@]}" exec -T proxy touch /tmp/dead
    say "The proxy at https://127.0.0.1:5667 (master-01 behind it) passes nothing; connections stay open." \
      "An environment that connects through it (demo/screenshot.sh --via-proxy writes one) shows:" \
      "- after about 2 minutes without a line: event stream stalled, reconnecting" \
      "- the reconnect through the proxy hangs until its timeout, then fails over to master-02" \
      "  (or, without a second URL: no live data for <n>m, states may be outdated)" \
      "Undo: demo/scenario.sh recover"
    ;;

  recover)
    require_up
    "${COMPOSE[@]}" unpause master-01 master-02 sat-ams-01 sat-fra-01 sat-fra-02 2>/dev/null || true
    "${COMPOSE[@]}" exec -T proxy rm -f /tmp/dead 2>/dev/null || true
    "$HERE/up.sh" >/dev/null
    # Only the objects checks-stopped turned off: restoring an attribute
    # makes Icinga reschedule the object's check.
    for kind in host service; do
      api POST "/v1/objects/${kind}s" "{ \"filter\": \"$kind.enable_active_checks == false\", \"attrs\": {}, \"restore_attrs\": [\"enable_active_checks\"] }" >/dev/null || true
    done
    # Connected is not caught up: after a frozen master or a cut-off zone
    # the checks that were due meanwhile are overdue (Icinga's own
    # next_update, which icygui calls late) until they have run again.
    settle=${ICYGUI_DEMO_SETTLE:-300}
    deadline=$(( $(date +%s) + settle ))
    while :; do
      late=$(overdue)
      [ "$late" = 0 ] && break
      checks="$late checks are"
      [ "$late" = 1 ] && checks="1 check is"
      if [ "$(date +%s)" -ge "$deadline" ]; then
        say "After ${settle}s $checks still overdue (demo/scenario.sh recover again, or look at" \
          "docker compose -f demo/docker-compose.yml logs)." >&2
        break
      fi
      echo "recover: $checks overdue, waiting for them to run" >&2
      sleep 5
    done
    say "Recovered: every node runs and is connected, every check is active and none is overdue," \
      "the proxy passes again. Icinga's latency figures (the health page's checks view) settle" \
      "as the checks that ran late run again, within their intervals."
    ;;

  *)
    usage >&2
    exit 2
    ;;
esac
