#!/usr/bin/env python3
"""The tables of a fleet run (contract/scale/fleet.sh; docs/performance.md,
"Fifty clients on one master"), from its raw data:

    fleet_analyse.py RUN_DIR [RUN_DIR ...]   markdown on stdout, summary.json in each

Every `mark` of the run starts a phase that lasts until the next one. Per
phase: the master's answer times to the two probes (median, 95th
percentile, max, failures), each container's CPU (cores, from its cgroup's
CPU time) and memory, the client process's CPU and memory, the delay from a
check result of the marker service to each engine seeing it (every result,
also those a newer one superseded within the same batch, from the web
probe's send times), requests a minute per client by mode, the JSON-RPC
queues and HTTP clients of master-01, loads and what reconciles found, and
when the clients planned their next reconciles. Reads gzipped files too.
"""
import bisect
import calendar
import csv
import gzip
import io
import json
import os
import re
import statistics
import sys
import time
from collections import Counter, defaultdict


def opener(run, name):
    path = os.path.join(run, name)
    if os.path.exists(path):
        return open(path, newline="")
    if os.path.exists(path + ".gz"):
        return io.TextIOWrapper(gzip.open(path + ".gz"), newline="")
    return None


def rows(run, name):
    handle = opener(run, name)
    if handle is None:
        return []
    with handle:
        return list(csv.DictReader(handle))


def pct(values, share):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(round((len(ordered) - 1) * share)))]


def ms(value):
    if value == float("inf"):
        return "failed"
    return "–" if value is None else f"{value * 1000:.0f}"


def secs(value):
    return "–" if value is None else f"{value:.1f}"


def phases(run):
    """Every mark starts a phase; so does every command that changes the
    fleet (`add`, `mode`, `stop`: a load or a switch in progress, labelled
    `(command; …)`), so a steady phase holds no loads of the next one."""
    bounds = []
    for r in rows(run, "timeline.csv"):
        t = float(r["t"])
        if r["event"] == "mark":
            bounds.append((t, r["detail"]))
        elif r["event"] == "command" and r["detail"].split()[0] in ("add", "mode", "stop"):
            if bounds and bounds[-1][1].startswith("(") and t - bounds[-1][0] < 2:
                bounds[-1] = (bounds[-1][0], bounds[-1][1][:-1] + "; " + r["detail"] + ")")
            else:
                bounds.append((t, "(" + r["detail"] + ")"))
    ends = [t for t, _ in bounds[1:]]
    last = max([float(r["t"]) for r in rows(run, "probe-byname.csv")] or [bounds[-1][0] if bounds else 0])
    ends.append(last)
    return [(label, start, end) for (start, label), end in zip(bounds, ends)]


def series(run):
    """Everything sampled, loaded once."""
    data = {}
    # (t, status, seconds, open-loop?): the `open` column exists since the
    # probes became open-loop (2026-10-11's settling).
    for kind in ("byname", "web"):
        data[kind] = sorted((float(r["t"]), int(r["status"]), float(r["seconds"]), r.get("open") == "1")
                            for r in rows(run, f"probe-{kind}.csv"))
    cg = defaultdict(list)
    for r in rows(run, "cgroups.csv"):
        cg[r["container"]].append((float(r["t"]), int(r["cpu_ns"]), int(r["mem_bytes"]), int(r["rss_bytes"])))
    data["cgroups"] = cg
    data["clients"] = [(float(r["t"]), int(r["ticks"]), int(r["rss_kib"]), int(r["threads"]))
                       for r in rows(run, "clients.csv")]
    status = []
    handle = opener(run, "status.ndjson")
    if handle:
        with handle:
            status = [json.loads(line) for line in handle if line.strip()]
    data["status"] = status
    data["timeline"] = rows(run, "timeline.csv")
    data["delays"] = rows(run, "delays.csv")
    data["requests"] = rows(run, "requests.csv")
    data["loads"] = rows(run, "loads.csv")
    data["plans"] = rows(run, "plans.csv")
    data["cpu"] = rows(run, "cpu.csv")
    # fleet_streams.py (bare streams that time the marker), fleet_sample.py's
    # per-CPU and per-process samples: in the runs since 2026-10-11's settling.
    data["marker"] = rows(run, "marker.csv")
    data["chunks"] = rows(run, "chunks.csv")
    data["cpus"] = rows(run, "cpus.csv")
    data["procs"] = rows(run, "procs.csv")
    actions = []
    handle = opener(run, "actions.csv")
    if handle:
        with handle:
            for line in handle:
                t, _, text = line.rstrip("\n").partition(",")
                actions.append((float(t), text))
    data["actions"] = actions
    # One header per storm: storm.csv is appended to.
    data["storm"] = [(float(r["t"]), int(r["results"]), float(r["seconds"]), int(r["status"]))
                     for r in rows(run, "storm.csv") if r["t"] != "t"]
    # master-01's own log (warnings and its work queues), shared by the
    # runs: `icinga-master-01.log` next to the run directories.
    log = []
    parent = os.path.dirname(os.path.abspath(run))
    handle = opener(parent, "icinga-master-01.log")
    if handle:
        with handle:
            for line in handle:
                match = re.match(r"\[(\d{4}-\d\d-\d\d \d\d:\d\d:\d\d) \+0000\] (\S+): (.*)", line)
                if match:
                    t = calendar.timegm(time.strptime(match.group(1), "%Y-%m-%d %H:%M:%S"))
                    log.append((t, match.group(2), match.group(3)))
    data["icinga_log"] = log
    return data


def window(samples, start, end, key=0):
    return [s for s in samples if start <= s[key] < end]


def cpu_cores(samples, start, end):
    """Average cores over [start, end) from cumulative CPU nanoseconds."""
    inside = window(samples, start, end)
    if len(inside) < 2:
        return None
    (t0, c0, *_), (t1, c1, *_) = inside[0], inside[-1]
    # A restart resets the counter: sum the increments that are positive.
    total = 0
    for (ta, ca, *_), (tb, cb, *_) in zip(inside, inside[1:]):
        if cb >= ca:
            total += cb - ca
    return total / 1e9 / (t1 - t0) if t1 > t0 else None


def cpu_seconds(samples, start, end):
    inside = window(samples, start, end)
    total = 0
    for (ta, ca, *_), (tb, cb, *_) in zip(inside, inside[1:]):
        if cb >= ca:
            total += cb - ca
    return total / 1e9


def open_loop(samples):
    """Every probe slot of a fixed one-second schedule, as `(t, ok,
    seconds)`. Open-loop probes (`fleet_probe.py`'s `open` field, since
    2026-10-11's settling) are taken as they are. The runs before were
    closed-loop: one request a second, but each waited for the previous
    answer, so a slow answer stood for every second it blocked. Those are
    re-scored: each second a slow answer covered gets the time that answer
    was still outstanding then (`seconds - k` for the k-th second), the
    estimate of what a request sent then would have waited at least. A
    failed request counts as over every threshold (infinite)."""
    out = []
    for t, status, s, *rest in samples:
        out.append((t, status == 200, s if status == 200 else float("inf")))
        if rest and rest[0]:
            continue  # open-loop: the next slot went out on time
        k = 1
        while s - k > 0:
            out.append((t + k, True, s - k))
            k += 1
    out.sort()
    return out


def probe_stats(samples, start, end):
    inside = window(samples, start, end)
    ok = [s for _, status, s, *_ in inside if status == 200]
    failed = sum(1 for _, status, *_ in inside if status != 200)
    # The worst minute: the highest 95th percentile of any minute of the
    # phase (a short spike that a long phase's percentile would hide).
    minutes = defaultdict(list)
    for t, status, s, *_ in inside:
        if status == 200:
            minutes[int((t - start) // 60)].append(s)
    worst = max((pct(v, 0.95) for v in minutes.values() if len(v) >= 10), default=None)
    # The same over every second's slot (`open_loop`), failures as infinite.
    slots = [v for t, _, v in open_loop(window(samples, start - 120, end)) if start <= t < end]
    slot_minutes = defaultdict(list)
    for t, _, v in open_loop(window(samples, start - 120, end)):
        if start <= t < end:
            slot_minutes[int((t - start) // 60)].append(v)
    return {"n": len(inside), "median": pct(ok, 0.5), "p95": pct(ok, 0.95), "max": max(ok) if ok else None,
            "failed": failed, "over_500ms": sum(1 for s in ok if s > 0.5), "over_2s": sum(1 for s in ok if s > 2),
            "worst_minute_p95": worst,
            "slots": {"n": len(slots), "median": pct(slots, 0.5), "p95": pct(slots, 0.95),
                      "max": max(slots) if slots else None,
                      "over_500ms": sum(1 for v in slots if v > 0.5), "over_2s": sum(1 for v in slots if v > 2),
                      "worst_minute_p95": max((pct(v, 0.95) for v in slot_minutes.values() if len(v) >= 10),
                                              default=None)}}


def modes(timeline):
    """Per client: [(t, live?)] from its start and mode changes; and when it stopped."""
    changes = defaultdict(list)
    stopped_at = []
    for r in timeline:
        if r["event"] == "start":
            changes[int(r["client"])].append((float(r["t"]), r["detail"].startswith("live")))
        elif r["event"] == "mode":
            changes[int(r["client"])].append((float(r["t"]), r["detail"] == "live"))
        elif r["event"] == "command" and r["detail"].startswith("stop"):
            stopped_at.append(float(r["t"]))
    return changes, stopped_at


def live_at(changes, client, t):
    state = None
    for at, live in changes.get(client, []):
        if at <= t:
            state = live
    return state


def result_delays(data):
    """Per engine sighting of the marker: the delay of every result it
    brought, from the web probe's send times (a result's execution_end is
    when Icinga processed the probe's request)."""
    probes = sorted((t + s, t) for t, status, s, _ in data["web"] if status == 200)
    ends = [end for end, _ in probes]
    by_client = defaultdict(list)
    for r in data["delays"]:
        by_client[int(r["client"])].append((float(r["t"]), float(r["execution_end"]), r["mode"]))
    out = []  # (seen, client, mode, delay)
    for client, sightings in by_client.items():
        sightings.sort()
        previous = None
        for seen, end, mode in sightings:
            out.append((seen, client, mode, seen - end))
            if previous is not None and end - previous < 30:
                # The superseded ones: sent after the previous sighting's
                # result, processed before this one's.
                lo = bisect.bisect_right(ends, previous + 0.05)
                hi = bisect.bisect_left(ends, end - 0.05)
                for index in range(lo, hi):
                    out.append((seen, client, mode, seen - ends[index]))
            previous = end
    out.sort()
    return out


def stream_delays(data, start, end):
    """The master's own delivery delay to bare streams (fleet_streams.py):
    from the probe's send time (`hi`: includes the probe's request itself)
    and from its answer (`lo`: Icinga had processed the result by then), per
    stream kind, over the results that arrived in [start, end)."""
    sent = sorted((t, s) for t, status, s, _ in data["web"] if status == 200)
    times = [t for t, _ in sent]
    out = {}
    for kind in ("live", "quiet"):
        hi, lo = [], []
        for r in data["marker"]:
            t = float(r["t"])
            if r["kind"] != kind or not start <= t < end:
                continue
            at = float(r["sent"])
            hi.append(t - at)
            i = bisect.bisect_left(times, at - 0.2)
            if i < len(times) and abs(times[i] - at) < 0.2:
                lo.append(max(0.0, t - times[i] - sent[i][1]))
        if hi:
            out[kind] = {"n": len(hi), "median": pct(hi, 0.5), "p95": pct(hi, 0.95), "max": max(hi),
                         "lo_median": pct(lo, 0.5), "lo_p95": pct(lo, 0.95)}
    return out


def stream_flushes(data, start, end):
    """How often Icinga writes to the first live and the first quiet stream:
    gaps between reads (a gap under 50 ms is the same write)."""
    out = {}
    for kind in ("live", "quiet"):
        reads = sorted(float(r["t"]) for r in data["chunks"] if r["kind"] == kind and start <= float(r["t"]) < end)
        size = sum(int(r["bytes"]) for r in data["chunks"] if r["kind"] == kind and start <= float(r["t"]) < end)
        writes = [t for i, t in enumerate(reads) if i == 0 or t - reads[i - 1] >= 0.05]
        gaps = [b - a for a, b in zip(writes, writes[1:])]
        if gaps:
            out[kind] = {"writes": len(writes), "gap_median": pct(gaps, 0.5), "gap_p95": pct(gaps, 0.95),
                         "gap_max": max(gaps), "kib_s": size / 1024 / (end - start)}
    return out


def cpu_busy(samples, start, end):
    """Each CPU's busy share over [start, end) (/proc/stat)."""
    by = defaultdict(list)
    for r in samples:
        t = float(r["t"])
        if start <= t < end:
            by[r["cpu"]].append((int(r["busy_ticks"]), int(r["total_ticks"])))
    return {cpu: round((v[-1][0] - v[0][0]) / max(1, v[-1][1] - v[0][1]), 3) for cpu, v in sorted(by.items())
            if len(v) >= 2}


def procs_cores(samples, start, end):
    """Cores used by docker-proxy and the stream readers over [start, end)."""
    by = defaultdict(list)
    for r in samples:
        t = float(r["t"])
        if start <= t < end:
            by[(r["name"], r["pid"])].append((t, int(r["ticks"])))
    out = defaultdict(float)
    for (name, _), v in by.items():
        if len(v) >= 2 and v[-1][0] > v[0][0]:
            out[name] += (v[-1][1] - v[0][1]) / 100 / (end - start)
    return {k: round(v, 3) for k, v in out.items()}


def reconcile_cost(data, start, end):
    """master-01's CPU per periodic reconcile (worked out from the raw data,
    two ways): the seconds with a reconcile running against those with none
    (`split`), and a fit of each second's cores on the number of reconciles
    running then (`fit`: slope × the mean reconcile length)."""
    m1 = [(t, c) for t, c, *_ in data["cgroups"].get("master-01", []) if start <= t < end]
    spans = [(float(r["t"]) - int(r["took_ms"]) / 1000, float(r["t"])) for r in data["loads"]
             if r["kind"] == "Reconcile" and start <= float(r["t"]) < end]
    if len(m1) < 60 or not spans:
        return None
    seconds = []
    for (ta, ca), (tb, cb) in zip(m1, m1[1:]):
        if cb >= ca and tb > ta:
            mid = (ta + tb) / 2
            seconds.append((sum(1 for a, b in spans if a <= mid < b), (cb - ca) / 1e9 / (tb - ta)))
    busy = [c for k, c in seconds if k]
    idle = [c for k, c in seconds if not k]
    length = statistics.mean(b - a for a, b in spans)
    split = (statistics.mean(busy) - statistics.mean(idle)) * len(busy) / len(spans) if busy and idle else None
    line = fit(seconds)
    return {"reconciles": len(spans), "mean_s": length, "split_cpu_s": split,
            "fit_cpu_s": line[1] * length if line else None}


def new_connections(data, start, end, idle=5.0):
    """The share of an engine's requests (the stream aside) that come more
    than `idle` seconds after its previous one: the connection pool has
    dropped its idle connection by then, so they open a new TLS connection
    (an estimate from request times; the pool itself isn't logged)."""
    by = defaultdict(list)
    for r in data["requests"]:
        t = float(r["t"])
        if start <= t < end and r["path"] != "/v1/events":
            by[r["client"]].append(t)
    new = total = 0
    for times in by.values():
        times.sort()
        total += len(times)
        new += 1 + sum(1 for a, b in zip(times, times[1:]) if b - a > idle)
    return new / total if total else None


def summarise(run):
    data = series(run)
    changes, _ = modes(data["timeline"])
    delays = result_delays(data)
    delay_times = [d[0] for d in delays]
    m1 = data["cgroups"].get("master-01", [])
    m2 = data["cgroups"].get("master-02", [])
    result = {"run": run, "phases": []}
    request_times = sorted(((float(r["t"]), r) for r in data["requests"]), key=lambda x: x[0])
    req_t = [t for t, _ in request_times]
    cpu_by_client = defaultdict(list)
    for r in data["cpu"]:
        cpu_by_client[int(r["client"])].append((float(r["t"]), int(r["ticks"])))
    for label, start, end in phases(run):
        if end - start < 1:
            # A mark and a command in the same moment: nothing to measure.
            continue
        phase = {"label": label, "start": start, "end": end, "minutes": (end - start) / 60}
        phase["byname"] = probe_stats(data["byname"], start, end)
        phase["web"] = probe_stats(data["web"], start, end)
        phase["m1_cores"] = cpu_cores(m1, start, end)
        phase["m2_cores"] = cpu_cores(m2, start, end)
        if label.endswith("-storm") or label.startswith("D"):
            # The storm's CPU beyond the master's rest, its tail included
            # (Icinga writes the streams 5 s apart).
            rest_label = label.replace("-storm", "-rest")
            rest = next((q["m1_cores"] for q in result["phases"] if q["label"] == rest_label), None)
            if rest is None and result["phases"]:
                rest = result["phases"][-1]["m1_cores"]
            tail = end + 20
            phase["storm_extra_cpu_s"] = cpu_seconds(m1, start, tail) - (rest or 0) * (tail - start)
            phase["storm_results"] = sum(r for t, r, _, status in data["storm"] if start <= t < end and status == 200)
            phase["storm_request_s_max"] = max((x for t, _, x, _ in data["storm"] if start <= t < end), default=None)
        phase["cluster_cores"] = sum(filter(None, (cpu_cores(v, start, end) for v in data["cgroups"].values())))
        inside = window(m1, start, end)
        if inside:
            phase["m1_mem_mib"] = {"start": inside[0][2] >> 20, "peak": max(s[2] for s in inside) >> 20,
                                   "end": inside[-1][2] >> 20, "rss_end": inside[-1][3] >> 20}
        clients = window(data["clients"], start, end)
        if len(clients) >= 2:
            phase["clients_cores"] = (clients[-1][1] - clients[0][1]) / 100 / (clients[-1][0] - clients[0][0])
            phase["clients_rss_mib"] = {"start": clients[0][2] >> 10, "peak": max(c[2] for c in clients) >> 10,
                                        "end": clients[-1][2] >> 10}
        # Engines running through the whole phase, by mode at its start.
        running = [c for c in changes if changes[c][0][0] <= start and live_at(changes, c, start) is not None]
        phase["engines"] = len(running)
        phase["live"] = sum(1 for c in running if live_at(changes, c, start + 1))
        # Delays.
        lo, hi = bisect.bisect_left(delay_times, start), bisect.bisect_left(delay_times, end)
        for mode in ("live", "quiet"):
            values = [d[3] for d in delays[lo:hi] if d[2] == mode]
            phase[f"delay_{mode}"] = {"n": len(values), "median": pct(values, 0.5), "p95": pct(values, 0.95),
                                      "max": max(values) if values else None}
        # Requests a minute per client, the event stream aside.
        lo, hi = bisect.bisect_left(req_t, start), bisect.bisect_left(req_t, end)
        per = Counter()
        paths = {True: Counter(), False: Counter()}
        streams = 0
        for t, r in request_times[lo:hi]:
            client = int(r["client"])
            if r["path"] == "/v1/events":
                streams += 1
                continue
            per[client] += 1
            mode = live_at(changes, client, t)
            if mode is not None:
                paths[mode][r["path"]] += 1
        minutes = (end - start) / 60
        for mode, name in ((True, "live"), (False, "quiet")):
            members = [c for c in running if live_at(changes, c, start + 1) == mode]
            rates = [per[c] / minutes for c in members]
            phase[f"rpm_{name}"] = {"clients": len(members), "mean": statistics.mean(rates) if rates else None,
                                    "max": max(rates) if rates else None,
                                    "paths": {p: round(n / minutes / max(1, len(members)), 2)
                                              for p, n in paths[mode].most_common(6)}}
        phase["rpm_all"] = sum(per.values()) / minutes
        phase["streams_opened"] = streams
        # Client CPU per engine by mode.
        for mode, name in ((True, "live"), (False, "quiet")):
            values = []
            for c in running:
                if live_at(changes, c, start + 1) != mode:
                    continue
                s = [x for x in cpu_by_client.get(c, []) if start <= x[0] < end]
                if len(s) >= 2 and s[-1][0] > s[0][0]:
                    values.append((s[-1][1] - s[0][1]) / 100 / (s[-1][0] - s[0][0]))
            phase[f"engine_cores_{name}"] = statistics.mean(values) if values else None
        # Icinga's own numbers.
        st = [s for s in data["status"] if start <= s["t"] < end]
        def values(key, sub="m1"):
            return [s[sub][key] for s in st if s.get(sub) and key in s[sub]]
        phase["http_clients"] = pct(values("api_num_http_clients"), 0.5)
        for key, name in (("api_num_json_rpc_relay_queue_items", "relay_items"),
                          ("api_num_json_rpc_sync_queue_items", "sync_items"),
                          ("api_num_json_rpc_work_queue_item_rate", "work_rate"),
                          ("api_num_json_rpc_relay_queue_item_rate", "relay_rate")):
            v = values(key)
            phase[f"m1_{name}"] = {"max": max(v) if v else None, "last": v[-1] if v else None,
                                   "mean": statistics.mean(v) if v else None}
            v2 = values(key, "m2")
            phase[f"m2_{name}"] = {"max": max(v2) if v2 else None, "last": v2[-1] if v2 else None}
        cib = [s["cib"] for s in st if s.get("cib")]
        phase["pending_callbacks_max"] = max((c.get("current_pending_callbacks", 0) for c in cib), default=None)
        phase["latency_avg"] = statistics.mean([c["avg_latency"] for c in cib]) if cib else None
        phase["status_answer_max"] = max((s.get("m1_listener_s") or 0 for s in st), default=None)
        # Icinga's own complaints meanwhile.
        log = [x for x in data["icinga_log"] if start - 1 <= x[0] < end]
        phase["icinga_connection_timeouts"] = sum(1 for _, _, m in log if "Timeout while processing incoming connection" in m)
        waits = [int(w) for _, _, m in log for w in re.findall(r"waited (\d+)ms on semaphore", m)]
        phase["icinga_jsonrpc_waits"] = len(waits)
        phase["icinga_jsonrpc_wait_max_ms"] = max(waits, default=None)
        relay = [int(n) for _, src, m in log if src == "information/WorkQueue" and "RelayQueue" in m
                 for n in re.findall(r"items: (\d+)", m)]
        phase["icinga_relay_items_max"] = max(relay, default=None)
        # Loads.
        loads = [r for r in data["loads"] if start <= float(r["t"]) < end]
        phase["loads"] = dict(Counter(r["kind"] for r in loads))
        rec = [r for r in loads if r["kind"] == "Reconcile"]
        phase["reconciles"] = len(rec)
        phase["reconciles_found"] = sum(1 for r in rec if int(r["found"]) > 0)
        phase["reconcile_found_objects"] = sum(int(r["found"]) for r in rec)
        phase["load_took_s"] = {k: pct([int(r["took_ms"]) / 1000 for r in loads if r["kind"] == k], 0.5)
                                for k in phase["loads"]}
        # Reconcile plans made in the phase (absolute due times).
        plans = [r for r in data["plans"] if start <= float(r["t"]) < end]
        due = sorted(float(r["t"]) + int(r["in_s"]) for r in plans if r["cadence"] == "First")
        if due:
            phase["plans_first"] = {"n": len(due), "first_in_min": (due[0] - start) / 60,
                                    "last_in_min": (due[-1] - start) / 60,
                                    "largest_bunch_60s": max(bisect.bisect_left(due, d + 60) - i
                                                             for i, d in enumerate(due))}
        # Connection events.
        tl = [r for r in data["timeline"] if start <= float(r["t"]) < end]
        phase["reconnecting"] = sum(1 for r in tl if r["event"] == "state" and r["detail"].startswith("reconnecting"))
        phase["connected_events"] = sum(1 for r in tl if r["event"] == "state" and r["detail"].startswith("connected"))
        phase["completed"] = sum(1 for r in tl if r["event"] == "complete")
        if data["marker"]:
            phase["stream_delay"] = stream_delays(data, start, end)
        if data["chunks"]:
            phase["stream_flushes"] = stream_flushes(data, start, end)
        if data["cpus"]:
            phase["cpu_busy"] = cpu_busy(data["cpus"], start, end)
        if data["procs"]:
            phase["procs_cores"] = procs_cores(data["procs"], start, end)
        result["phases"].append(phase)
    # A fit step's storm: master-01's CPU from the storm's start to the end
    # of the wait after it, beyond the step's own rest.
    by_label = {p["label"]: p for p in result["phases"]}
    for label, p in by_label.items():
        if label.startswith("F") and label.endswith("-storm"):
            after = by_label.get(label.replace("-storm", "-after"))
            rest = by_label.get(label.replace("-storm", "-rest"), {}).get("m1_cores")
            if after and rest is not None:
                p["storm_extra_cpu_s"] = cpu_seconds(m1, p["start"], after["end"]) - rest * (after["end"] - p["start"])
                p["storm_window_s"] = after["end"] - p["start"]
    # Starts: from each engine's start to connected and to complete (an
    # index is used again after `stop`: each start is its own).
    starts, current = [], {}
    for r in data["timeline"]:
        client, t = int(r["client"]), float(r["t"])
        if r["event"] == "start":
            current[client] = {"client": client, "start": t, "mode": r["detail"]}
            starts.append(current[client])
        elif r["event"] == "state" and r["detail"].startswith("connected") and client in current \
                and "connected" not in current[client]:
            current[client]["connected"] = t
        elif r["event"] == "complete" and client in current and "complete" not in current[client]:
            current[client]["complete"] = t
    result["starts"] = starts
    # Whole run: reconciles and what they found.
    rec = [r for r in data["loads"] if r["kind"] == "Reconcile"]
    result["reconciles_total"] = len(rec)
    result["reconciles_found_total"] = sum(1 for r in rec if int(r["found"]) > 0)
    # What each periodic reconcile found (found.csv, when the run logged
    # it): the marker (it flips every second, so a reconcile's answer is
    # newer than the stream's last batch about half the time), changes
    # that began within the last 10 s (Icinga's 5 s batches were still
    # bringing them), and the rest: what the stream really missed.
    found = rows(run, "found.csv")
    if found:
        marker = os.environ.get("FLEET_MARKER", "storage-1999.prod.example.com!smart")
        def kind(r):
            if r["object"] == marker:
                return "marker"
            changed = set(filter(None, (r.get("changed") or "").split("+")))
            try:
                began = float(r["began_s_ago"].strip("Some()"))
            except ValueError:
                began = None
            if changed and changed <= {"flapping"}:
                return "flapping only"
            if changed and changed <= {"reachable"}:
                return "reachable only"
            if changed and changed <= {"since"}:
                return "since only"
            if changed and changed <= {"state_type"}:
                # Soft to hard in the same state, at the retry check: its
                # `since` is the soft state's.
                return "state type only"
            if began is not None and began <= 10:
                return "in flight"
            return "missed"
        quiet_at = {}
        for r in rec:
            quiet_at.setdefault(r["client"], []).append((float(r["t"]), r["quiet"] == "true"))
        def mode_of(f):
            times = quiet_at.get(f["client"], [])
            later = [q for t, q in times if t >= float(f["t"]) - 1]
            return "quiet" if (later[0] if later else False) else "live"
        per_reconcile = []
        for r in rec:
            client, done = r["client"], float(r["t"])
            start = done - int(r["took_ms"]) / 1000 - 1
            mine = [f for f in found if f["client"] == client and start <= float(f["t"]) <= done + 1]
            per_reconcile.append(Counter(kind(f) for f in mine))
        result["reconcile_findings"] = {
            "reconciles": len(per_reconcile),
            "with_any": sum(1 for c in per_reconcile if c),
            "with_marker": sum(1 for c in per_reconcile if c["marker"]),
            "with_in_flight": sum(1 for c in per_reconcile if c["in flight"]),
            "with_missed": sum(1 for c in per_reconcile if c["missed"]),
            "with_reachable_only": sum(1 for c in per_reconcile if c["reachable only"]),
            "with_flapping_only": sum(1 for c in per_reconcile if c["flapping only"]),
            "with_state_type_only": sum(1 for c in per_reconcile if c["state type only"]),
            "changed_fields": dict(Counter(f.get("changed") or "?" for f in found)),
            "objects": dict(Counter(kind(f) for f in found)),
            "missed": [f for f in found if kind(f) == "missed"][:40],
            "by_mode": {q: dict(Counter(kind(f) for f in found if mode_of(f) == q)) for q in ("live", "quiet")},
        }
    for p in result["phases"]:
        cost = reconcile_cost(data, p["start"], p["end"])
        if cost and cost["reconciles"] >= 10:
            p["reconcile_cost"] = cost
        if data["requests"] and p["minutes"] >= 5:
            p["new_connection_share"] = new_connections(data, p["start"], p["end"])
    result["loads_total"] = dict(Counter(r["kind"] for r in data["loads"]))
    result["found_by_kind"] = {k: sum(int(r["found"]) for r in data["loads"] if r["kind"] == k)
                               for k in result["loads_total"]}
    return result, data


def fit(points):
    """Least squares y = a + b x."""
    if len(points) < 2:
        return None
    n = len(points)
    mx = sum(x for x, _ in points) / n
    my = sum(y for _, y in points) / n
    sxx = sum((x - mx) ** 2 for x, _ in points)
    if sxx == 0:
        return None
    b = sum((x - mx) * (y - my) for x, y in points) / sxx
    return my - b * mx, b


def report(result, data):
    print(f"\n### {os.path.basename(result['run'].rstrip('/'))}\n")
    print("| phase | min | engines (live) | by-name ms med / p95 / max | web ms med / p95 / max | "
          "worst minute p95 ms by-name / web | >500 ms / >2 s / failed | master-01 cores | master-02 | "
          "master-01 MiB start / peak / end | clients cores | clients MiB peak |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for p in result["phases"]:
        b, w = p["byname"], p["web"]
        mem = p.get("m1_mem_mib", {})
        print(f"| {p['label']} | {p['minutes']:.1f} | {p['engines']} ({p['live']}) | "
              f"{ms(b['median'])} / {ms(b['p95'])} / {ms(b['max'])} | {ms(w['median'])} / {ms(w['p95'])} / {ms(w['max'])} | "
              f"{ms(b['worst_minute_p95'])} / {ms(w['worst_minute_p95'])} | "
              f"{b['over_500ms'] + w['over_500ms']} / {b['over_2s'] + w['over_2s']} / {b['failed'] + w['failed']} | "
              f"{p['m1_cores'] or 0:.3f} | {p['m2_cores'] or 0:.3f} | "
              f"{mem.get('start', '–')} / {mem.get('peak', '–')} / {mem.get('end', '–')} | "
              f"{p.get('clients_cores') or 0:.2f} | {p.get('clients_rss_mib', {}).get('peak', '–')} |")
    print("\nEvery second's slot (`open_loop`: closed-loop runs re-scored, failures as over every threshold):\n")
    print("| phase | by-name ms med / p95 / max | web ms med / p95 / max | worst minute p95 ms by-name / web | "
          "slots > 500 ms / > 2 s (by-name + web) |")
    print("|---|---|---|---|---|")
    for p in result["phases"]:
        b, w = p["byname"]["slots"], p["web"]["slots"]
        print(f"| {p['label']} | {ms(b['median'])} / {ms(b['p95'])} / {ms(b['max'])} | "
              f"{ms(w['median'])} / {ms(w['p95'])} / {ms(w['max'])} | "
              f"{ms(b['worst_minute_p95'])} / {ms(w['worst_minute_p95'])} | "
              f"{b['over_500ms'] + w['over_500ms']} / {b['over_2s'] + w['over_2s']} |")
    print("\n| phase | delay live s med / p95 / max (n) | delay quiet s med / p95 / max (n) | "
          "req/min live (mean, max) | req/min quiet (mean, max) | all req/min | engine cores live / quiet | "
          "HTTP clients | relay queue max m1 / m2 | work queue rate | pending callbacks | loads | reconciles (found) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for p in result["phases"]:
        dl, dq = p["delay_live"], p["delay_quiet"]
        rl, rq = p["rpm_live"], p["rpm_quiet"]
        f = lambda v, d=2: "–" if v is None else f"{v:.{d}f}"
        print(f"| {p['label']} | {secs(dl['median'])} / {secs(dl['p95'])} / {secs(dl['max'])} ({dl['n']}) | "
              f"{secs(dq['median'])} / {secs(dq['p95'])} / {secs(dq['max'])} ({dq['n']}) | "
              f"{f(rl['mean'])}, {f(rl['max'])} | {f(rq['mean'])}, {f(rq['max'])} | {f(p['rpm_all'], 1)} | "
              f"{f(p['engine_cores_live'], 4)} / {f(p['engine_cores_quiet'], 4)} | {p['http_clients']} | "
              f"{p['m1_relay_items']['max']} / {p['m2_relay_items']['max']} | {f(p['m1_work_rate']['mean'], 0)} | "
              f"{p['pending_callbacks_max']} | {p['loads']} | {p['reconciles']} ({p['reconciles_found']}) |")
    print("\n| phase | Icinga: connection timeouts | JSON-RPC semaphore waits (max ms) | relay queue items max (log) | "
          "status answer max s |")
    print("|---|---|---|---|---|")
    for p in result["phases"]:
        print(f"| {p['label']} | {p['icinga_connection_timeouts']} | {p['icinga_jsonrpc_waits']} "
              f"({p['icinga_jsonrpc_wait_max_ms']}) | {p['icinga_relay_items_max']} | "
              f"{'–' if p['status_answer_max'] is None else round(p['status_answer_max'], 2)} |")
    print("\nRequests a minute per client by path (mean over the phase's clients):\n")
    for p in result["phases"]:
        if p["rpm_live"]["paths"] or p["rpm_quiet"]["paths"]:
            print(f"- {p['label']}: live {p['rpm_live']['paths']}; quiet {p['rpm_quiet']['paths']}")
    for p in result["phases"]:
        extra = {k: p[k] for k in ("stream_delay", "stream_flushes", "cpu_busy", "procs_cores", "reconcile_cost",
                                   "new_connection_share") if p.get(k)}
        if extra:
            print(f"- {p['label']}: {json.dumps(extra, default=lambda v: round(v, 3) if isinstance(v, float) else str(v))}")
    plans = [p for p in result["phases"] if p.get("plans_first")]
    if plans:
        print("\nFirst reconciles planned after a load (minutes after the phase began):\n")
        for p in plans:
            pf = p["plans_first"]
            print(f"- {p['label']}: {pf['n']} plans, due {pf['first_in_min']:.1f} to {pf['last_in_min']:.1f} min, "
                  f"at most {pf['largest_bunch_60s']} due within one minute")
    if result.get("reconcile_findings"):
        print(f"\nWhat the periodic reconciles found: {json.dumps(result['reconcile_findings'])}")
    print(f"\nLoads over the run: {result['loads_total']}; objects found that no event announced, by kind: "
          f"{result['found_by_kind']}; periodic reconciles {result['reconciles_total']}, "
          f"of which found something: {result['reconciles_found_total']}")
    starts = result["starts"]
    if starts:
        groups = defaultdict(list)
        for s in starts:
            if "complete" in s:
                groups[s["mode"]].append((s.get("connected", s["complete"]) - s["start"], s["complete"] - s["start"]))
        for mode, v in groups.items():
            print(f"- starts {mode}: {len(v)}; connected after {pct([a for a, _ in v], 0):.1f}–"
                  f"{pct([a for a, _ in v], 1):.1f} s (median {pct([a for a, _ in v], 0.5):.1f}); "
                  f"complete median {pct([b for _, b in v], 0.5):.1f} s, max {pct([b for _, b in v], 1):.1f} s")


def events(result, data):
    """What happened around each restart and reload of master-01."""
    actions = data["actions"]
    loads = [(float(r["t"]) - int(r["took_ms"]) / 1000, float(r["t"]), int(r["client"]), r["kind"])
             for r in data["loads"]]
    out = []
    for index, (t, text) in enumerate(actions):
        if text not in ("restart master-01", "reload master-01"):
            continue
        up = next((u for u, x in actions[index + 1:] if x.startswith("master-01 answers")), None)
        if up is None:
            continue
        # Until the next restart or reload, at most 15 minutes.
        until = min([u for u, x in actions[index + 1:] if x in ("restart master-01", "reload master-01")]
                    + [t + 900])
        reloads = sorted(start for start, _, _, kind in loads if kind == "Reload" and t <= start < until)
        firsts = sorted(start for start, _, _, kind in loads if kind == "First" and t <= start < until)
        reconciles = sorted(start for start, _, _, kind in loads if kind == "Reconcile" and t <= start < until)
        after = {"byname": probe_stats(data["byname"], up, up + 180), "web": probe_stats(data["web"], up, up + 180)}
        down = sum(1 for tt, status, *_ in data["byname"] if t <= tt < up and status != 200)
        plans = sorted(float(r["t"]) + int(r["in_s"]) for r in data["plans"]
                       if r["cadence"] == "First" and t <= float(r["t"]) < until)
        tl = [r for r in data["timeline"] if t <= float(r["t"]) < until]
        reconnected = sorted(float(r["t"]) for r in tl if r["event"] == "state" and r["detail"].startswith("connected"))
        m1 = data["cgroups"].get("master-01", [])
        out.append({
            "what": text, "at": t, "answers_after_s": up - t, "probe_failures_while_down": down,
            "reconnected": len(reconnected),
            "reconnected_s": [round(x - up, 1) for x in (reconnected[:1] + reconnected[-1:])] if reconnected else None,
            "reloads": len(reloads),
            "reload_start_s": {"min": reloads[0] - up, "median": pct(reloads, 0.5) - up, "max": reloads[-1] - up}
            if reloads else None,
            "reload_largest_10s": max((bisect.bisect_left(reloads, x + 10) - i for i, x in enumerate(reloads)),
                                      default=0),
            "first_loads": len(firsts),
            "after_3min": after,
            "m1_cores_3min": cpu_cores(m1, up, up + 180),
            "m1_mem_peak_mib": max((x[2] for x in window(m1, t, until)), default=0) >> 20,
            "reload_done_s": {"median": pct(sorted(done for _, done, _, kind in loads if kind == "Reload" and t <= done < until), 0.5) - up,
                              "max": max(done for _, done, _, kind in loads if kind == "Reload" and t <= done < until) - up}
            if reloads else None,
            "reload_took_s": {"median": pct([d - s0 for s0, d, _, kind in loads if kind == "Reload" and t <= s0 < until], 0.5),
                              "max": max([d - s0 for s0, d, _, kind in loads if kind == "Reload" and t <= s0 < until], default=None)}
            if reloads else None,
            "plans": {"n": len(plans), "first_min": (plans[0] - up) / 60, "last_min": (plans[-1] - up) / 60,
                      "fifths": fifths(plans)} if plans else None,
            "reconciles": len(reconciles),
        })
    return out


def fifths(times):
    """How the times fall into five equal parts of their span (a check that
    no part holds most of them)."""
    if len(times) < 2:
        return None
    lo, hi = times[0], times[-1]
    counts = [0] * 5
    for t in times:
        counts[min(4, int((t - lo) / (hi - lo) * 5))] += 1
    return counts


def fits(results):
    """The master's CPU against the number of event streams."""
    lines = []
    for result in results:
        phases = {p["label"]: p for p in result["phases"]}
        a = [(p["engines"], p["m1_cores"]) for label, p in phases.items()
             if label in ("A0", "A1", "A10", "A25", "A50") and p["m1_cores"] is not None]
        if len(a) >= 2:
            intercept, slope = fit(a)
            lines.append(f"- at rest, engines (about a tenth live): master-01 = {intercept:.3f} cores + "
                         f"{slope * 1000:.2f} milli-cores per engine ({a})")
        base = phases.get("A50") or phases.get("E50")
        if "A50-live" in phases and base:
            d = phases["A50-live"]["m1_cores"] - base["m1_cores"]
            lines.append(f"- 45 more engines live (the same 50 streams): +{d:.3f} cores, "
                         f"{d / 45 * 1000:.2f} milli-cores per live stream more than a quiet one")
            # With `steady`'s 1.68 milli-cores per engine (a tenth live):
            # quiet + 0.1 × (live − quiet) = 1.68.
            per_engine = next((line for line in lines if "per engine" in line), None)
            if per_engine:
                mean = float(re.search(r"([\d.]+) milli-cores per engine", per_engine).group(1))
                quiet = mean - 0.1 * d / 45 * 1000
                lines.append(f"- so a quiet engine about {quiet:.1f} and a live one {quiet + d / 45 * 1000:.1f} milli-cores")
        f = [(int(label[1:-5]), p["m1_cores"]) for label, p in phases.items()
             if label.startswith("F") and label.endswith("-rest") and p["m1_cores"] is not None]
        if len(f) >= 2:
            intercept, slope = fit(f)
            lines.append(f"- at rest, bare live streams: master-01 = {intercept:.3f} cores + "
                         f"{slope * 1000:.2f} milli-cores per stream ({f})")
        storms = []
        for label, p in phases.items():
            if label.startswith("F") and label.endswith("-storm"):
                n = int(label[1:-6])
                rest = phases.get(f"F{n}-rest", {}).get("m1_cores")
                p_storm = p
                if rest is None:
                    continue
                storms.append((n, p_storm["storm_extra_cpu_s"], p_storm["storm_results"], p_storm["m1_cores"]))
        if len(storms) >= 2:
            storms.sort()
            results_n = statistics.mean(r for _, _, r, _ in storms)
            subsets = [("every step", storms)]
            if len(storms) >= 3:
                # Without the last step: its window was cut off before the
                # master was done in the first `fit` (2026-10-11).
                subsets.append(("without the last step", storms[:-1]))
            # A step's own rest can be off: in `fit-dedicated` (2026-10-11)
            # master-01 was still busy from opening the step's streams when
            # its rest was taken (0.58 cores with 10 streams, 0.81 with 50).
            # So also against the rest without streams (F0) plus a clean
            # at-rest cost per stream (`fit`: 5.06 milli-cores; override
            # with FLEET_REST_PER_STREAM in cores).
            per_stream = float(os.environ.get("FLEET_REST_PER_STREAM", "0.00506"))
            base = phases.get("F0-rest", {}).get("m1_cores")
            if base is not None and all(phases[f"F{n}-storm"].get("storm_window_s") for n, *_ in storms):
                alt = []
                for n, extra, results, cores in storms:
                    p = phases[f"F{n}-storm"]
                    own = phases[f"F{n}-rest"]["m1_cores"]
                    alt.append((n, extra + (own - base - per_stream * n) * p["storm_window_s"], results, cores))
                subsets.append((f"against F0's rest + {per_stream * 1000:.2f} milli-cores per stream", alt))
            for name, points in subsets:
                intercept, slope = fit([(n, cpu) for n, cpu, _, _ in points])
                lines.append(f"- storm (about {results_n:.0f} state changes), master-01 CPU above rest, {name}: "
                             f"{intercept:.1f} s + {slope:.2f} s per stream; per state change and stream "
                             f"{slope / results_n * 1e6:.1f} µs ({[(n, round(c, 1), round(k, 2)) for n, c, _, k in points]})")
    return lines


def main():
    results = []
    for run in sys.argv[1:]:
        result, data = summarise(run)
        result["events"] = events(result, data)
        report(result, data)
        if result["events"]:
            print("\nRestarts and reloads:\n")
            for e in result["events"]:
                print(f"- {json.dumps(e, default=lambda v: round(v, 3) if isinstance(v, float) else str(v))}")
        results.append(result)
        with open(os.path.join(run, "summary.json"), "w") as out:
            json.dump(result, out, indent=1, default=str)
    lines = fits(results)
    if lines:
        print("\nThe master's CPU against event streams:\n")
        print("\n".join(lines))


if __name__ == "__main__":
    main()
