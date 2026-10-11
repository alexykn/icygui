#!/usr/bin/env python3
"""Samples the master and the clients while the icygui fleet runs
(contract/scale/fleet.sh, docs/performance.md), into a directory:

- `cgroups.csv`, every second: each demo container's CPU time (its cgroup's
  `cpuacct.usage`, nanoseconds: CPU time, not wall clock) and memory
  (`memory.usage_in_bytes`, and the `rss` and `cache` of `memory.stat`);
- `clients.csv`, every second: the fleet's process (the PID in the file
  `fleet.pid` once it exists): CPU time in clock ticks, resident memory;
- `status.ndjson`, every 5 s: master-01's and master-02's
  `/v1/status/ApiListener` and master-01's `/v1/status/CIB` perfdata (the
  JSON-RPC relay, sync and work queues, the HTTP clients, check latency).

    fleet_sample.py OUT_DIR URL_1 URL_2 CA

Reads cgroup v1 (cgroupfs driver) or v2. Credentials for the status come
from FLEET_STATUS_USER and _PASSWORD. Only this machine's demo cluster:
fleet.sh checks it first.
"""
import base64
import http.client
import json
import os
import ssl
import subprocess
import sys
import threading
import time
import urllib.parse

CONTAINERS = ["master-01", "master-02", "sat-ams-01", "sat-fra-01", "sat-fra-02", "proxy"]


def container_ids():
    ids = {}
    for name in CONTAINERS:
        out = subprocess.run(["docker", "inspect", "-f", "{{.Id}}", f"icygui-demo-{name}-1"],
                             capture_output=True, text=True)
        if out.returncode == 0:
            ids[name] = out.stdout.strip()
    return ids


def read(path):
    try:
        with open(path) as handle:
            return handle.read()
    except OSError:
        return None


def cgroup_sample(cid):
    """(cpu ns, memory bytes, rss bytes, cache bytes) or None."""
    v1 = f"/sys/fs/cgroup/cpuacct/docker/{cid}/cpuacct.usage"
    cpu = read(v1)
    if cpu is not None:
        mem = read(f"/sys/fs/cgroup/memory/docker/{cid}/memory.usage_in_bytes")
        stat = read(f"/sys/fs/cgroup/memory/docker/{cid}/memory.stat") or ""
        fields = dict(line.split() for line in stat.splitlines() if " " in line)
        return (int(cpu), int(mem or 0), int(fields.get("total_rss", fields.get("rss", 0))),
                int(fields.get("total_cache", fields.get("cache", 0))))
    for base in (f"/sys/fs/cgroup/system.slice/docker-{cid}.scope", f"/sys/fs/cgroup/docker/{cid}"):
        stat = read(f"{base}/cpu.stat")
        if stat is None:
            continue
        usage = next(int(line.split()[1]) for line in stat.splitlines() if line.startswith("usage_usec"))
        mem = read(f"{base}/memory.current")
        mstat = dict(line.split() for line in (read(f"{base}/memory.stat") or "").splitlines())
        return (usage * 1000, int(mem or 0), int(mstat.get("anon", 0)), int(mstat.get("file", 0)))
    return None


def process_sample(pid):
    stat = read(f"/proc/{pid}/stat")
    status = read(f"/proc/{pid}/status")
    if stat is None or status is None:
        return None
    rest = stat[stat.rfind(")") + 2:].split()
    ticks = int(rest[11]) + int(rest[12])
    rss = next((int(line.split()[1]) for line in status.splitlines() if line.startswith("VmRSS:")), 0)
    threads = next((int(line.split()[1]) for line in status.splitlines() if line.startswith("Threads:")), 0)
    return ticks, rss, threads


def status_loop(out_dir, urls, ca, stop):
    context = ssl.create_default_context(cafile=ca)
    context.check_hostname = False
    auth = "Basic " + base64.b64encode(
        f"{os.environ['FLEET_STATUS_USER']}:{os.environ['FLEET_STATUS_PASSWORD']}".encode()).decode()
    connections = {}

    def get(url, path):
        parsed = urllib.parse.urlsplit(url)
        for _ in range(2):
            connection = connections.get(url)
            if connection is None:
                connection = http.client.HTTPSConnection(parsed.hostname, parsed.port, context=context,
                                                         timeout=30)
                connections[url] = connection
            started = time.time()
            try:
                connection.request("GET", path, headers={"Authorization": auth, "Accept": "application/json"})
                response = connection.getresponse()
                body = response.read()
                if response.status != 200:
                    return None, time.time() - started
                return json.loads(body), time.time() - started
            except (OSError, http.client.HTTPException, ValueError):
                connection.close()
                connections.pop(url, None)
        return None, None

    with open(os.path.join(out_dir, "status.ndjson"), "a") as out:
        while not stop.is_set():
            t = time.time()
            record = {"t": round(t, 3)}
            for label, url in (("m1", urls[0]), ("m2", urls[1])):
                answer, took = get(url, "/v1/status/ApiListener")
                record[f"{label}_listener_s"] = took
                if answer:
                    entry = answer["results"][0]
                    record[label] = {p["label"]: p["value"] for p in entry.get("perfdata", [])}
                    api = entry["status"]["api"]
                    record[f"{label}_http_clients"] = api.get("http", {}).get("clients")
                    record[f"{label}_json_rpc"] = api.get("json_rpc")
                    record[f"{label}_not_conn"] = api.get("not_conn_endpoints")
            answer, took = get(urls[0], "/v1/status/CIB")
            record["m1_cib_s"] = took
            if answer:
                record["cib"] = answer["results"][0]["status"]
            out.write(json.dumps(record) + "\n")
            out.flush()
            stop.wait(max(0.0, 5.0 - (time.time() - t)))


def main():
    out_dir, url1, url2, ca = sys.argv[1:5]
    for url in (url1, url2):
        if urllib.parse.urlsplit(url).hostname not in ("127.0.0.1", "localhost", "::1"):
            raise SystemExit(f"fleet_sample.py: {url} is not this machine")
    stop = threading.Event()
    threading.Thread(target=status_loop, args=(out_dir, (url1, url2), ca, stop), daemon=True).start()
    ids = container_ids()
    refreshed = time.time()
    pid_file = os.path.join(out_dir, "fleet.pid")
    with open(os.path.join(out_dir, "cgroups.csv"), "a") as cg, \
            open(os.path.join(out_dir, "clients.csv"), "a") as cl:
        if cg.tell() == 0:
            cg.write("t,container,cpu_ns,mem_bytes,rss_bytes,cache_bytes\n")
        if cl.tell() == 0:
            cl.write("t,pid,ticks,rss_kib,threads\n")
        next_at = time.time()
        try:
            while True:
                t = time.time()
                if t - refreshed > 10:
                    # A restarted container keeps its id; a recreated one doesn't.
                    ids = container_ids()
                    refreshed = t
                for name, cid in ids.items():
                    sample = cgroup_sample(cid)
                    if sample:
                        cg.write(f"{t:.3f},{name},{sample[0]},{sample[1]},{sample[2]},{sample[3]}\n")
                pid = (read(pid_file) or "").strip()
                if pid:
                    sample = process_sample(pid)
                    if sample:
                        cl.write(f"{t:.3f},{pid},{sample[0]},{sample[1]},{sample[2]}\n")
                cg.flush()
                cl.flush()
                next_at += 1.0
                time.sleep(max(0.0, next_at - time.time()))
        except KeyboardInterrupt:
            stop.set()


if __name__ == "__main__":
    main()
