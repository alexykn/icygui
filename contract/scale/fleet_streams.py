#!/usr/bin/env python3
"""Bare event streams that time what they get (contract/scale/fleet.sh `fit`,
docs/performance.md, "Fifty clients on one master"): the master's own
delivery delay, with no icygui client behind the stream.

    fleet_streams.py OUT_DIR TAG LIVE QUIET HOST PORT CA

opens LIVE streams of every type (as a window on screen subscribes) and
QUIET streams of the quiet types (no `CheckResult`, as a window in the tray
subscribes) on master-01's `/v1/events`, each read by its own `curl` (TLS
in C, so the reading keeps up), and watches what comes out:

- `marker.csv` (`t,tag,stream,kind,sent,state`): every time a stream brings
  a result of the web probe's marker (`fleet_probe.py web` writes its send
  time into the plugin output), when it arrived. Only the first sighting
  per stream and send time. The delay is `t - sent`; the probe's answer
  time (probe-web.csv) bounds it from the other side.
- `chunks.csv` (`t,tag,stream,kind,bytes`): every read of the first live and
  the first quiet stream (when Icinga flushes a stream, and how much).

Runs until SIGTERM. Credentials from FLEET_STREAM_USER and _PASSWORD. HOST
is either this machine (127.0.0.1, through docker's port) or master-01's
address on the compose network, which fleet.sh takes from `docker inspect`
of the compose project's own master-01 after its guard; the certificate is
checked against the cluster's CA for the name master-01 either way. Never
point it at a production Icinga.
"""
import os
import re
import selectors
import signal
import subprocess
import sys
import time

LIVE_TYPES = ["CheckResult", "StateChange", "AcknowledgementSet", "AcknowledgementCleared", "CommentAdded",
              "CommentRemoved", "DowntimeAdded", "DowntimeRemoved", "DowntimeStarted", "DowntimeTriggered",
              "Flapping", "ObjectCreated", "ObjectModified", "ObjectDeleted", "Notification"]
QUIET_TYPES = [t for t in LIVE_TYPES if t != "CheckResult"]
MARKER = re.compile(rb"fleet marker (OK|CRITICAL) (\d+\.\d+)")
TAIL = 64


def main():
    out_dir, tag, live, quiet, host, port, ca = sys.argv[1:8]
    live, quiet = int(live), int(quiet)
    if not (host in ("127.0.0.1", "localhost") or re.fullmatch(r"172\.\d+\.\d+\.\d+", host)):
        raise SystemExit(f"fleet_streams.py: {host} is neither this machine nor a docker network address")
    user = os.environ["FLEET_STREAM_USER"]
    password = os.environ["FLEET_STREAM_PASSWORD"]
    streams = []
    for index in range(live + quiet):
        kind = "live" if index < live else "quiet"
        types = LIVE_TYPES if kind == "live" else QUIET_TYPES
        body = '{"queue":"fleet-%s-%s-%d","types":[%s]}' % (
            tag, kind, index, ",".join(f'"{t}"' for t in types))
        process = subprocess.Popen(
            ["curl", "-sS", "--noproxy", "*", "--cacert", ca, "--resolve", f"master-01:{port}:{host}",
             "-u", f"{user}:{password}", "-H", "Accept: application/json", "-N", "-X", "POST",
             f"https://master-01:{port}/v1/events", "-d", body],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        os.set_blocking(process.stdout.fileno(), False)
        streams.append({"index": index, "kind": kind, "process": process, "tail": b"", "seen": set()})
    with open(os.path.join(out_dir, f"procs-streams-{tag}.pid"), "w") as pids:
        pids.write(" ".join(str(p) for p in [os.getpid()] + [s["process"].pid for s in streams]))
    first = {next((s["index"] for s in streams if s["kind"] == k), None) for k in ("live", "quiet")}

    stopping = []
    signal.signal(signal.SIGTERM, lambda *_: stopping.append(True))
    selector = selectors.DefaultSelector()
    for stream in streams:
        selector.register(stream["process"].stdout, selectors.EVENT_READ, stream)
    marker_path = os.path.join(out_dir, "marker.csv")
    chunks_path = os.path.join(out_dir, "chunks.csv")
    with open(marker_path, "a") as marker, open(chunks_path, "a") as chunks:
        if marker.tell() == 0:
            marker.write("t,tag,stream,kind,sent,state\n")
        if chunks.tell() == 0:
            chunks.write("t,tag,stream,kind,bytes\n")
        last_flush = time.time()
        while not stopping and streams:
            for key, _ in selector.select(timeout=1.0):
                stream = key.data
                try:
                    data = os.read(key.fd, 1 << 18)
                except BlockingIOError:
                    continue
                now = time.time()
                if not data:
                    selector.unregister(key.fileobj)
                    streams.remove(stream)
                    continue
                if stream["index"] in first:
                    chunks.write(f"{now:.3f},{tag},{stream['index']},{stream['kind']},{len(data)}\n")
                window = stream["tail"] + data
                for match in MARKER.finditer(window):
                    sent = match.group(2).decode()
                    if sent not in stream["seen"]:
                        stream["seen"].add(sent)
                        marker.write(f"{now:.3f},{tag},{stream['index']},{stream['kind']},{sent},"
                                     f"{match.group(1).decode()}\n")
                stream["tail"] = window[-TAIL:]
                if len(stream["seen"]) > 4096:
                    stream["seen"] = set(sorted(stream["seen"])[-1024:])
            if time.time() - last_flush > 1:
                marker.flush()
                chunks.flush()
                last_flush = time.time()
    for stream in streams:
        stream["process"].terminate()
    for stream in streams:
        try:
            stream["process"].wait(timeout=5)
        except subprocess.TimeoutExpired:
            stream["process"].kill()


if __name__ == "__main__":
    main()
