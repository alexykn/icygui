#!/usr/bin/env python3
"""Calibrates the heartbeat's time budget (PLAN.md §4.2 B2) against a local
Icinga 2 in Docker (never a production one: `heartbeat.sh` starts its own).

It records every result of the heartbeat services on two event streams at
once, as icygui would see them:

- live: every `CheckResult` (the API user `icygui`, like a live stream);
- quiet: state changes plus `CheckResult` for the heartbeats only, through
  the stream's `filter` (the admin user: the filter needs Icinga's
  `filter-expression` permission).

For each beat it notes Icinga's own times (`schedule_start`,
`execution_start`, `execution_end`) and when the line arrived, then works
out what icygui's budget would have done: the scheduling latency
(`execution_start - schedule_start`), the delivery delay (arrival -
`execution_end`, against the smallest seen so far, so the clock offset
between this machine and Icinga cancels out), the allowance
`max(5 s, 3 x p99 over the last 50 beats)` capped at half the interval,
and how often a beat came after its deadline (late) or more than 10 s
after it (a REST query). Usage:

    heartbeat.py URL USER PASSWORD ADMIN ADMIN_PASSWORD SECONDS LABEL OUT.csv [burst]

`burst`: every five minutes, two services of every load host get a
critical passive result (their next active check brings them back): a
burst of state changes on top of the load.
"""
import csv
import http.client
import json
import ssl
import sys
import threading
import time
from urllib.parse import urlparse

URL, USER, PASSWORD, ADMIN, ADMIN_PASSWORD = sys.argv[1:6]
SECONDS = float(sys.argv[6])
LABEL = sys.argv[7]
OUT = sys.argv[8]
BURST = len(sys.argv) > 9 and sys.argv[9] == "burst"
HOST = "icygui-hb"
BEATS = {"beat-10s": 10.0, "beat-30s": 30.0}
BASE = urlparse(URL)
CONTEXT = ssl.create_default_context()
CONTEXT.check_hostname = False
CONTEXT.verify_mode = ssl.CERT_NONE

records = []  # (stream, service, interval, schedule_start, execution_start, execution_end, arrival)
lock = threading.Lock()
stop = time.time() + SECONDS


def connection():
    return http.client.HTTPSConnection(BASE.hostname, BASE.port, context=CONTEXT, timeout=SECONDS + 120)


def auth(user, password):
    import base64

    token = base64.b64encode(f"{user}:{password}".encode()).decode()
    return {"Authorization": f"Basic {token}", "Accept": "application/json", "Content-Type": "application/json"}


def read(stream, user, password, body):
    conn = connection()
    conn.request("POST", "/v1/events", json.dumps(body), auth(user, password))
    response = conn.getresponse()
    if response.status != 200:
        print(f"{stream}: {response.status} {response.read()[:200]!r}", file=sys.stderr)
        return
    while time.time() < stop:
        line = response.readline()
        if not line:
            print(f"{stream}: the stream ended", file=sys.stderr)
            return
        arrival = time.time()
        if b'"CheckResult"' not in line or HOST.encode() not in line:
            continue
        event = json.loads(line)
        if event.get("type") != "CheckResult" or event.get("host") != HOST:
            continue
        service = event.get("service")
        if service not in BEATS:
            continue
        result = event["check_result"]
        with lock:
            records.append(
                (
                    stream,
                    service,
                    BEATS[service],
                    result["schedule_start"],
                    result["execution_start"],
                    result["execution_end"],
                    arrival,
                )
            )


def bursts():
    while time.time() < stop - 60:
        time.sleep(300)
        if time.time() >= stop - 60:
            return
        conn = connection()
        body = {
            "type": "Service",
            "filter": 'match("*.prod.example.com", host.name) && (service.name == "ping4" || service.name == "ssh")',
            "exit_status": 2,
            "plugin_output": "CRITICAL - calibration burst",
        }
        started = time.time()
        conn.request("POST", "/v1/actions/process-check-result", json.dumps(body), auth(ADMIN, ADMIN_PASSWORD))
        answer = conn.getresponse().read()
        count = len(json.loads(answer).get("results", []))
        print(f"burst: {count} state changes in {time.time() - started:.1f} s", file=sys.stderr)


def p(values, q):
    if not values:
        return float("nan")
    values = sorted(values)
    return values[min(len(values) - 1, int(round(q * (len(values) - 1))))]


def simulate(rows, interval):
    """icygui's budget over one stream's beats of one service: lates and queries."""
    lates = queries = 0
    worst = 0.0
    allowances = []
    window = []
    baseline = None
    previous = None
    for _, _, _, ss, es, ee, arrival in rows:
        if previous is not None:
            deadline = previous[0] + interval + previous[1]
            over = arrival - deadline
            worst = max(worst, arrival - (previous[0] + interval))
            if over > 0:
                lates += 1
            if over > 10:
                queries += 1
        delivery = arrival - ee
        baseline = delivery if baseline is None else min(baseline, delivery)
        window.append(max(0.0, es - ss) + (delivery - baseline))
        window = window[-50:]
        allowance = min(max(5.0, 3 * p(window, 0.99)), interval / 2)
        allowances.append(allowance)
        previous = (arrival, allowance)
    return lates, queries, worst, allowances


def main():
    threads = [
        threading.Thread(target=read, args=("live", USER, PASSWORD, {"queue": f"hb-live-{time.time()}", "types": ["CheckResult"]}), daemon=True),
        threading.Thread(
            target=read,
            args=(
                "quiet",
                ADMIN,
                ADMIN_PASSWORD,
                {
                    "queue": f"hb-quiet-{time.time()}",
                    "types": ["CheckResult", "StateChange"],
                    "filter": 'event.type != "CheckResult" || event.host == "icygui-hb" && (event.service == "beat-10s" || event.service == "beat-30s")',
                },
            ),
            daemon=True,
        ),
    ]
    if BURST:
        threads.append(threading.Thread(target=bursts, daemon=True))
    for thread in threads:
        thread.start()
    while time.time() < stop:
        time.sleep(5)
    with open(OUT, "w", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(["label", "stream", "service", "interval", "schedule_start", "execution_start", "execution_end", "arrival"])
        for row in records:
            writer.writerow([LABEL, *row])
    print(f"# {LABEL}: {SECONDS / 60:.0f} minutes")
    print("stream service  beats  latency p50/p99/max (ms)  delivery p50/p99/max (ms)  gap-interval p99/max (s)  allowance min/max (s)  late  queries")
    for stream in ("live", "quiet"):
        for service, interval in BEATS.items():
            rows = sorted((r for r in records if r[0] == stream and r[1] == service), key=lambda r: r[6])
            if len(rows) < 2:
                print(f"{stream:6} {service}: {len(rows)} beats")
                continue
            latency = [max(0.0, r[4] - r[3]) * 1000 for r in rows]
            base = min(r[6] - r[5] for r in rows)
            delivery = [(r[6] - r[5] - base) * 1000 for r in rows]
            gaps = [b[6] - a[6] - interval for a, b in zip(rows, rows[1:])]
            lates, queries, worst, allowances = simulate(rows, interval)
            print(
                f"{stream:6} {service:8} {len(rows):5}  "
                f"{p(latency, .5):6.1f} {p(latency, .99):7.1f} {max(latency):7.1f}    "
                f"{p(delivery, .5):6.1f} {p(delivery, .99):7.1f} {max(delivery):7.1f}    "
                f"{p(gaps, .99):6.2f} {max(gaps):6.2f}    "
                f"{min(allowances):5.1f} {max(allowances):5.1f}    {lates:4} {queries:4}"
            )


main()
