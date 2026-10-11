#!/usr/bin/env bash
# Many icygui clients on one master (PLAN.md §4.2, "Many clients on one
# master"; docs/performance.md, "Fifty clients on one master"): real
# ic-core engines (ic-core's ignored `fleet` test, one process) against
# the demo cluster's master-01 at the user's size, while the master is
# measured from outside: its cgroup's CPU time and memory, its answer time
# to another client's small query and to an Icinga Web style command
# (each every second), its JSON-RPC queues and HTTP clients. The cluster's
# containers run on some cores and everything on the client side on the
# others (cpuset), so the clients' CPU doesn't distort the master's.
#
#   ICYGUI_DEMO_SCALE_HOSTS=2000 demo/up.sh        # the cluster, 33 000 services
#   contract/scale/fleet.sh <scenario> [out dir]  # default contract/scale/fleet-data/<scenario>
#   contract/scale/fleet_analyse.py <out dir>     # the tables
#
# Scenarios (each line of `run_<scenario>` below is the schedule):
#   smoke    a few clients, a small storm: checks the harness (5 minutes)
#   steady   0, 1, 10, 25, 50 clients (about a tenth live), 30 minutes at
#            50, a 1 500/s problem storm to the 50 engines (about 75
#            minutes; on a 4-core, 15 GB machine the clients' backlog after
#            the storm runs out of memory)
#   events   50 clients (5 live), a 750/s storm, all 50 live for a while
#            (about 20 minutes)
#   restart  50 fresh clients (a tenth live; FLEET_CLIENTS), a restart of
#            master-01, then a reload (restart-process) (about 30 minutes)
#   fit      no engines: 0, 10, 25 and 50 bare event streams (curl, every
#            type, read by fleet_streams.py, which times the marker's
#            results: the master's own delivery delay), plus one quiet
#            stream throughout, at rest and under the same storm
#            (FLEET_STORM a second, default 1 500, for FLEET_STORM_S,
#            default 60), each step until master-01 is back at rest, for
#            the master's CPU per stream (25 to 40 minutes)
#   morning  on a freshly restarted master: 50 windows opening within 3
#            minutes (Start::User), then, restarted again, 50 starts at
#            login at once (Start::Background) (about 30 minutes)
#   quiet_reconcile  12 quiet clients reconciling every 5 minutes (quiet
#            mode's 30-minute floor lowered for the run) for 25 minutes:
#            what quiet reconciles find (about 30 minutes)
#   reconcile  15 live clients reconciling every 5 minutes (the shortest
#            fixed interval the settings allow at this size; quiet mode
#            keeps 10 quiet ones at 30) for 40 minutes: what the periodic
#            reconciles find that no event announced (found.csv), and their
#            overlap (about 45 minutes)
#
# Environment: FLEET_CLUSTER_CPUS (0,1) and FLEET_CLIENT_CPUS (2,3) split
# the cores; FLEET_M1_CPUS and FLEET_OTHERS_CPUS (both FLEET_CLUSTER_CPUS
# by default) give master-01 cores of its own and the rest of the cluster
# others (`fit` on a dedicated master-01: FLEET_M1_CPUS=0,1
# FLEET_OTHERS_CPUS=2 FLEET_CLIENT_CPUS=3). docker's port proxies
# (docker-proxy, outside the containers' cgroups) are pinned to the client
# cores and sampled (procs.csv); FLEET_RAW_DIRECT=1 has `fit`'s bare
# streams connect to master-01's address on the compose network instead,
# past the proxy. FLEET_BIN skips the build (the engine test binary). Nothing
# else should run meanwhile (a shared CPU skews everything), and the build
# happens before anything is measured.
#
# Only ever the disposable demo cluster of demo/docker-compose.yml on this
# machine: before anything it checks that the URL is this machine, the
# fixture-only `viewer` logs in, the node is master-01 and the compose
# project runs it (the harness checks the same again). It restarts
# master-01 and floods it; never point it at a production Icinga. The
# passwords below are the demo's documented demo values (docs/demo.md).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
SCENARIO=${1:?usage: fleet.sh smoke|steady|events|restart|fit|morning|reconcile|quiet_reconcile [out dir]}
OUT=${2:-$HERE/fleet-data/$SCENARIO}
URL1=https://127.0.0.1:${ICYGUI_DEMO_PORT_1:-5665}
URL2=https://127.0.0.1:${ICYGUI_DEMO_PORT_2:-5666}
COMPOSE=(docker compose -f "$ROOT/demo/docker-compose.yml")
CLUSTER_CPUS=${FLEET_CLUSTER_CPUS:-0,1}
CLIENT_CPUS=${FLEET_CLIENT_CPUS:-2,3}
M1_CPUS=${FLEET_M1_CPUS:-$CLUSTER_CPUS}
OTHERS_CPUS=${FLEET_OTHERS_CPUS:-$CLUSTER_CPUS}
MARKER=${FLEET_MARKER:-storage-1999.prod.example.com!smart}
CLIENT_USER=icygui-demo
CLIENT_PASSWORD=icygui-demo-password
ADMIN_USER=demo-admin
ADMIN_PASSWORD=demo-admin-password
MASTER=icygui-demo-master-01-1

mkdir -p "$OUT"
: >"$OUT/actions.csv"
rm -f "$OUT"/procs-*.pid
CA="$OUT/ca.crt"
PIDS=()
RAW_PIDS=()

now() { date +%s.%N | cut -c1-14; }
# A line in actions.csv: what this script did to the cluster, and when.
action() { printf '%s,%s\n' "$(now)" "$*" >>"$OUT/actions.csv"; echo "fleet.sh: $*" >&2; }

api() { # api <user:password> <method> <url path> [json body]
  curl -sS --noproxy "*" --cacert "$CA" --resolve "master-01:${ICYGUI_DEMO_PORT_1:-5665}:127.0.0.1" \
    -u "$1" -H 'Accept: application/json' -X "$2" \
    "https://master-01:${ICYGUI_DEMO_PORT_1:-5665}$3" ${4:+-d "$4"}
}

refuse() { echo "fleet.sh: not the disposable demo cluster of demo/docker-compose.yml: $*" >&2; exit 1; }

guard() {
  "${COMPOSE[@]}" ps -q master-01 | grep -q . || refuse "its compose project runs no master-01"
  "${COMPOSE[@]}" exec -T master-01 cat /var/lib/icinga2/certs/ca.crt >"$CA"
  api viewer:viewer-test GET /v1 | grep -q '"user":"viewer"' || refuse "its fixture-only viewer can't log in"
  api viewer:viewer-test GET /v1/status/IcingaApplication | grep -q '"node_name":"master-01"' ||
    refuse "the node at $URL1 isn't master-01"
}

pin_cluster() {
  local id name cpus pid
  for id in $("${COMPOSE[@]}" ps -q); do
    name=$(docker inspect -f '{{.Name}}' "$id")
    cpus=$OTHERS_CPUS
    [ "$name" != "/$MASTER" ] || cpus=$M1_CPUS
    docker update --cpuset-cpus "$cpus" "$id" >/dev/null
    action "cpuset ${name#/} $(docker inspect -f '{{.HostConfig.CpusetCpus}}' "$id")"
  done
  # docker's userland proxies carry every request to the published ports;
  # they inherit dockerd's affinity (every core). They belong to the
  # client side.
  for pid in $(pgrep -x docker-proxy || true); do
    taskset -a -p -c "$CLIENT_CPUS" "$pid" >/dev/null
    action "docker-proxy $pid on cpus $(taskset -p -c "$pid" | sed 's/.*: //'): $(tr '\0' ' ' <"/proc/$pid/cmdline" | grep -o -- '-host-port [0-9]*')"
  done
  action "cluster: master-01 on cpus $M1_CPUS, the rest on cpus $OTHERS_CPUS; clients on cpus $CLIENT_CPUS"
}

build() {
  if [ -n "${FLEET_BIN:-}" ]; then
    BIN=$FLEET_BIN
    return
  fi
  # Release, without LTO (a quick build; the clients only need to keep up).
  BIN=$(cd "$ROOT" && CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_INCREMENTAL=0 cargo test --release --locked -p ic-core --test engine --no-run \
    --message-format=json 2>/dev/null |
    python3 -Ic 'import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] == "engine":
        print(m["executable"])' | tail -n 1)
  [ -x "$BIN" ] || { echo "fleet.sh: the engine test binary didn't build" >&2; exit 1; }
}

# The marker service flips every second (the web probe): in a downtime, so
# its flips notify nobody (Icinga, the clients' rules), and handled.
marker_downtime() {
  local depth host=${MARKER%%!*} service=${MARKER#*!}
  depth=$(api "$ADMIN_USER:$ADMIN_PASSWORD" GET "/v1/objects/services?service=$(python3 -Ic 'import sys, urllib.parse; print(urllib.parse.quote(sys.argv[1]))' "$MARKER")&attrs=downtime_depth" |
    python3 -Ic 'import json, sys; print(int(json.load(sys.stdin)["results"][0]["attrs"]["downtime_depth"]))')
  if [ "$depth" = 0 ]; then
    api "$ADMIN_USER:$ADMIN_PASSWORD" POST /v1/actions/schedule-downtime \
      "{\"type\":\"Service\",\"filter\":\"host.name==\\\"$host\\\" && service.name==\\\"$service\\\"\",\"start_time\":$(date +%s),\"end_time\":$(($(date +%s) + 86400)),\"fixed\":true,\"author\":\"fleet.sh\",\"comment\":\"fleet marker: flips every second, notifies nobody\"}" >/dev/null
    action "marker downtime scheduled"
  fi
}

start_side() {
  rm -f "$OUT/fleet.pid" "$OUT/control"
  export FLEET_STATUS_USER=$CLIENT_USER FLEET_STATUS_PASSWORD=$CLIENT_PASSWORD
  taskset -c "$CLIENT_CPUS" python3 -I "$HERE/fleet_sample.py" "$OUT" "$URL1" "$URL2" "$CA" &
  PIDS+=($!)
  # The probes preempt the clients (real-time priority): what they time is
  # the master's answer, not this machine's queue.
  FLEET_PROBE_USER=$CLIENT_USER FLEET_PROBE_PASSWORD=$CLIENT_PASSWORD \
    chrt -f 20 taskset -c "$CLIENT_CPUS" python3 -I "$HERE/fleet_probe.py" byname "$URL1" "$CA" >"$OUT/probe-byname.csv" &
  PIDS+=($!)
  FLEET_PROBE_USER=$ADMIN_USER FLEET_PROBE_PASSWORD=$ADMIN_PASSWORD FLEET_MARKER=$MARKER \
    chrt -f 20 taskset -c "$CLIENT_CPUS" python3 -I "$HERE/fleet_probe.py" web "$URL1" "$CA" >"$OUT/probe-web.csv" &
  PIDS+=($!)
  mkfifo "$OUT/control"
  # Fifty engines in one process: glibc's per-thread arenas (150 runtime
  # threads) would hold gigabytes of freed memory; two arenas keep the
  # process near what the engines use. It changes nothing they ask of the
  # master.
  MALLOC_ARENA_MAX=2 ICYGUI_FLEET_URL=$URL1 ICYGUI_FLEET_USER=$CLIENT_USER ICYGUI_FLEET_PASSWORD=$CLIENT_PASSWORD \
    ICYGUI_FLEET_CONTROL="$OUT/control" ICYGUI_FLEET_OUT="$OUT" ICYGUI_FLEET_MARKER=$MARKER \
    ICYGUI_FLEET_RECONCILE_S=${FLEET_RECONCILE_S:-0} ICYGUI_FLEET_QUIET_RECONCILE_S=${FLEET_QUIET_RECONCILE_S:-} \
    taskset -c "$CLIENT_CPUS" "$BIN" fleet::fleet --exact --ignored --nocapture --test-threads 1 \
    >"$OUT/fleet.log" 2>&1 &
  FLEET=$!
  echo "$FLEET" >"$OUT/fleet.pid"
  # Should this machine run out of memory, the clients go, not Icinga.
  echo 800 >"/proc/$FLEET/oom_score_adj" 2>/dev/null || true
  # Read-write, so opening doesn't wait for the fleet to open it.
  exec 3<>"$OUT/control"
  action "started: fleet pid $FLEET"
}

stop_side() {
  set +e
  raw_stop
  [ -z "${QUIET_PID:-}" ] || kill "$QUIET_PID" 2>/dev/null
  if [ -n "${FLEET:-}" ]; then
    echo quit >&3
    exec 3>&-
    wait "$FLEET"
  fi
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null; done
  wait 2>/dev/null
  rm -f "$OUT/control" "$OUT/fleet.pid" "$CA"
  set -e
}
trap stop_side EXIT

fleet() { echo "$*" >&3; }
mark() { fleet mark "$*"; action "mark $*"; }
pause() { sleep "$1"; }

# Waits until `n` engines have their first load complete (at most `t` s).
loaded() { local n; n=$(grep -c ',complete,' "$OUT/timeline.csv" 2>/dev/null) || true; echo "${n:-0}"; }

# Waits until `n` engines (counted over the whole run) have their first
# load complete, at most `t` s.
wait_loaded() {
  local n=$1 t=${2:-900} start=$SECONDS
  until [ "$(loaded)" -ge "$n" ]; do
    kill -0 "$FLEET" 2>/dev/null || { echo "fleet.sh: the fleet ended; see $OUT/fleet.log" >&2; exit 1; }
    [ $((SECONDS - start)) -lt "$t" ] || { action "only $(loaded) of $n loaded after $t s"; return 0; }
    sleep 2
  done
  action "$n loaded after $((SECONDS - start)) s"
}

master_answers() {
  local start=$SECONDS
  until api "$CLIENT_USER:$CLIENT_PASSWORD" GET /v1 2>/dev/null | grep -q '"user"'; do
    [ $((SECONDS - start)) -lt 600 ] || { action "master-01 didn't answer within 600 s"; return 1; }
    sleep 1
  done
}

# Restarts master-01's Icinga (the container: a stop and a start). The
# demo's seed, which re-checks the whole cluster with full object queries
# every 5 s after each start (demo/icinga/seed.py), is stopped at once: a
# real master has nothing like it, and it would add to the measured load.
restart_master() {
  action "restart master-01"
  docker restart -t 20 "$MASTER" >/dev/null
  local i
  for i in $(seq 1 100); do
    docker exec "$MASTER" pkill -f seed.py 2>/dev/null && break
    sleep 0.2
  done
  action "restarted master-01 (seed stopped: $i)"
  master_answers
  action "master-01 answers"
  # cpuset survives a restart; set again in case the container was recreated.
  docker update --cpuset-cpus "$M1_CPUS" "$MASTER" >/dev/null
}

# A config reload as a deploy does it (Director, `icinga2 api` packages):
# `restart-process` through the API.
reload_master() {
  action "reload master-01"
  api "$ADMIN_USER:$ADMIN_PASSWORD" POST /v1/actions/restart-process >/dev/null || true
  sleep 2
  master_answers
  action "master-01 answers after the reload"
}

storm() { # storm <rate> <seconds>: through master-02, as checks there would bring it
  action "storm $1/s for $2 s"
  FLEET_STORM_USER=$ADMIN_USER FLEET_STORM_PASSWORD=$ADMIN_PASSWORD \
    taskset -c "$CLIENT_CPUS" python3 -I "$HERE/fleet_storm.py" "$URL2" "$CA" "$1" "$2" \
    >>"$OUT/storm.csv" 2>>"$OUT/storm.log"
  action "storm over: $(tail -n 1 "$OUT/storm.log")"
}

# Bare event streams (fleet_streams.py: each read by curl, the marker's
# results timed as they arrive): raw_start <live> [quiet] [tag]. With
# FLEET_RAW_DIRECT=1 they connect to master-01's address on the compose
# network (from docker inspect of the project's own master-01, which the
# guard checked), past docker's port proxy.
raw_start() {
  local host=127.0.0.1 tag=${3:-s$1}
  if [ "${FLEET_RAW_DIRECT:-0}" = 1 ]; then
    host=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}' "$MASTER" | awk '{print $1}')
  fi
  FLEET_STREAM_USER=$CLIENT_USER FLEET_STREAM_PASSWORD=$CLIENT_PASSWORD \
    taskset -c "$CLIENT_CPUS" python3 -I "$HERE/fleet_streams.py" "$OUT" "$tag" "$1" "${2:-0}" \
    "$host" "${ICYGUI_DEMO_PORT_1:-5665}" "$CA" &
  RAW_PIDS+=($!)
  action "raw streams $tag: $1 live, ${2:-0} quiet, via $host"
}

raw_stop() {
  local pid
  for pid in "${RAW_PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${RAW_PIDS[@]}"; do wait "$pid" 2>/dev/null || true; done
  RAW_PIDS=()
  action "raw streams: 0"
}

# Waits until master-01 is back at its rest after a storm: its CPU over the
# last 20 s (cgroups.csv) within max(0.05, 25 %) of `rest` cores, at least
# `min` and at most `max` seconds.
settle() { # settle <rest cores> <min s> <max s>
  local start=$SECONDS now
  sleep "$2"
  until python3 -I - "$OUT/cgroups.csv" "$1" <<'PY'
import sys
rows = [l.split(",") for l in open(sys.argv[1]) if ",master-01," in l][-21:]
(t0, c0), (t1, c1) = (float(rows[0][0]), int(rows[0][2])), (float(rows[-1][0]), int(rows[-1][2]))
cores, rest = (c1 - c0) / 1e9 / (t1 - t0), float(sys.argv[2])
sys.exit(0 if cores <= rest + max(0.05, 0.25 * rest) else 1)
PY
  do
    now=$((SECONDS - start))
    [ "$now" -lt "$3" ] || { action "not settled after $now s"; return 0; }
    sleep 5
  done
  action "settled after $((SECONDS - start)) s"
}

# master-01's mean cores over the last <s> seconds (cgroups.csv).
m1_cores() {
  python3 -I - "$OUT/cgroups.csv" "$1" <<'PY'
import sys
rows = [l.split(",") for l in open(sys.argv[1]) if ",master-01," in l][-(int(sys.argv[2]) + 1):]
print(f"{(int(rows[-1][2]) - int(rows[0][2])) / 1e9 / (float(rows[-1][0]) - float(rows[0][0])):.3f}")
PY
}

run_smoke() {
  mark smoke-rest; pause 60
  fleet add 2 live user; fleet add 3 quiet background
  wait_loaded 5 300
  mark smoke-5; pause 120
  mark smoke-storm; storm 1000 10; pause 30
  mark smoke-end
}

run_steady() {
  mark A0; pause 300
  fleet add 1 live user
  wait_loaded 1; mark A1; pause 600
  fleet add 1 live user; fleet add 8 quiet background
  wait_loaded 10; mark A10; pause 600
  fleet add 1 live user; fleet add 14 quiet background
  wait_loaded 25; mark A25; pause 600
  fleet add 2 live user; fleet add 23 quiet background
  wait_loaded 50; mark A50; pause 1800
  # Fifty engines on the two client cores fall behind a storm of 1 500
  # state changes a second (about 82 000 events a second to apply): their
  # backlog filled this machine's memory and the client process was
  # killed a minute after the storm (2026-10-11). `events` goes on from
  # here with 750 a second, and `fit` floods bare streams at 5 000.
  mark D50; storm 1500 30
  mark D50-after; pause 120
  mark end
}

run_events() {
  fleet add 5 live user; fleet add 45 quiet background
  wait_loaded 50; mark E50; pause 300
  mark D50; storm 750 30
  mark D50-after; pause 120
  fleet mode 45 live
  pause 30; mark A50-live; pause 300
  fleet mode 45 quiet
  pause 30
  mark end
}

# The restart and the reload on fresh clients: after a storm the client
# process keeps the memory its backlog took (about 10 GB for fifty
# engines). Even fresh, fifty engines reloading at once need about 11 GB
# here (each reload about 100 MB on top of an engine's 125): on this
# 15 GB machine the client process ran out of memory two minutes into the
# reload wave (2026-10-11), so FLEET_CLIENTS (default 50) sets how many.
run_restart() {
  local n=${FLEET_CLIENTS:-50} live
  live=$(((n + 9) / 10))
  fleet add "$live" live user; fleet add $((n - live)) quiet background
  wait_loaded "$n"; mark "R$n"; pause 180
  mark B-restart; restart_master
  mark B-restart-up; pause 600
  mark B-reload; reload_master
  mark B-reload-up; pause 600
  mark end
}

# FLEET_STORM (default 1500 a second, 60 s): what master-02 can feed
# master-01 here without master-01 falling behind; at 5 000 a second
# (2026-10-11, `fit-5000`) master-02 managed 2 200 to 3 500 and master-01
# was still writing the storm to 25 and 50 streams minutes later, so each
# step's rest was its predecessor's tail. Each step therefore waits after
# its storm until master-01 is back at its rest (settle: at least 60 s, at
# most FLEET_SETTLE_MAX, default 900), and the analysis counts the storm's
# CPU from its start to the end of that wait. One quiet stream (the types
# a window in the tray subscribes to) runs through every step, the live
# streams are the step's: both time the marker's results as they arrive
# (marker.csv), the master's own delivery delay with no client behind it.
run_fit() {
  local n rate=${FLEET_STORM:-1500} seconds=${FLEET_STORM_S:-60} rest
  raw_start 0 1 quiet
  QUIET_PID=${RAW_PIDS[0]}
  RAW_PIDS=()
  for n in 0 10 25 50; do
    [ "$n" = 0 ] || raw_start "$n" 0 "s$n"
    pause 30
    mark "F$n-rest"; pause 60
    rest=$(m1_cores 60)
    action "F$n rest: master-01 $rest cores"
    mark "F$n-storm"; storm "$rate" "$seconds"; mark "F$n-after"
    settle "$rest" 60 "${FLEET_SETTLE_MAX:-900}"
    mark "F$n-settled"
    [ "$n" = 0 ] || raw_stop
    pause 15
  done
  kill "$QUIET_PID" 2>/dev/null || true
  wait "$QUIET_PID" 2>/dev/null || true
  mark end
}

run_morning() {
  restart_master; pause 240
  mark C-user-rest; pause 60
  mark C-user; fleet add 50 live user 180
  wait_loaded 50; pause 60
  fleet mode 45 quiet
  mark C-user-done; pause 120
  fleet stop all; pause 15
  restart_master; pause 240
  mark C-bg-rest; pause 60
  mark C-bg; fleet add 50 quiet background
  wait_loaded 100; pause 60
  mark C-bg-done; pause 120
  fleet stop all; pause 15
  mark end
}

run_reconcile() {
  mark R-load; fleet add 15 live user; fleet add 10 quiet user
  wait_loaded 25 600
  mark R; pause 2400
  mark end
}

# Quiet engines reconcile at least every 30 minutes: for this scenario
# only, the floor is lowered to 5 minutes (a tuning knob of the engine, not
# a setting), so a short run sees what their reconciles find.
run_quiet_reconcile() {
  mark Q-load; fleet add 12 quiet user
  wait_loaded 12 600
  mark Q; pause 1500
  mark end
}

case "$SCENARIO" in
  reconcile) export FLEET_RECONCILE_S=${FLEET_RECONCILE_S:-300} ;;
  quiet_reconcile) export FLEET_RECONCILE_S=${FLEET_RECONCILE_S:-300} FLEET_QUIET_RECONCILE_S=${FLEET_QUIET_RECONCILE_S:-300} ;;
esac
guard
build
pin_cluster
marker_downtime
start_side
"run_$SCENARIO"
