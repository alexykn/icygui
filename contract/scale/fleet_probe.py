#!/usr/bin/env python3
"""What anyone else using the master sees while the icygui fleet runs
(contract/scale/fleet.sh, docs/performance.md): one request a second on a
fresh TLS connection, its HTTP status and how long it took, as CSV on
stdout (`t,status,seconds,bytes,error`, Unix seconds).

    fleet_probe.py byname URL CA   another client opening an object: one
                                   service by name, two attributes (the
                                   `starts` measurement's probe)
    fleet_probe.py web URL CA      Icinga Web's command transport: a passive
                                   check result for the marker service by
                                   filter, its state flipped every second
                                   (the fleet sees the flips: delays.csv)

Credentials come from the environment (FLEET_PROBE_USER, _PASSWORD), the
marker from FLEET_MARKER (host!service), the probed service from
FLEET_PROBE_SERVICE. The URL must be this machine; fleet.sh checks it is
the demo cluster's master-01 first. Never point it at a production Icinga.
"""
import base64
import http.client
import json
import os
import ssl
import sys
import time
import urllib.parse


def main():
    kind, url, ca = sys.argv[1], sys.argv[2], sys.argv[3]
    parsed = urllib.parse.urlsplit(url)
    if parsed.hostname not in ("127.0.0.1", "localhost", "::1"):
        raise SystemExit(f"fleet_probe.py: {parsed.hostname} is not this machine")
    context = ssl.create_default_context(cafile=ca)
    # The demo's certificates name the node (master-01), not 127.0.0.1;
    # the chain is still checked against the cluster's own CA.
    context.check_hostname = False
    user = os.environ["FLEET_PROBE_USER"]
    password = os.environ["FLEET_PROBE_PASSWORD"]
    auth = "Basic " + base64.b64encode(f"{user}:{password}".encode()).decode()
    if kind == "byname":
        service = os.environ.get("FLEET_PROBE_SERVICE", "web-0000.prod.example.com!ping4")
        method = "GET"
        path = ("/v1/objects/services?service=" + urllib.parse.quote(service)
                + "&attrs=state&attrs=last_check_result")
    elif kind == "web":
        host, service = os.environ.get(
            "FLEET_MARKER", "storage-1999.prod.example.com!smart").split("!", 1)
        method = "POST"
        path = "/v1/actions/process-check-result"
    else:
        raise SystemExit(f"fleet_probe.py: unknown probe {kind}")
    print("t,status,seconds,bytes,error", flush=True)
    flip = 0
    next_at = time.time()
    while True:
        next_at += 1.0
        body = None
        if kind == "web":
            flip ^= 1
            # As Icinga Web's API command transport sends it: by filter.
            body = json.dumps({
                "type": "Service",
                "filter": f'host.name=="{host}" && service.name=="{service}"',
                "exit_status": 2 if flip else 0,
                "plugin_output": f"fleet marker {'CRITICAL' if flip else 'OK'} {time.time():.3f}",
            })
        started = time.time()
        status, size, error = 0, 0, ""
        try:
            connection = http.client.HTTPSConnection(
                parsed.hostname, parsed.port or 5665, context=context, timeout=60)
            connection.request(method, path, body=body, headers={
                "Authorization": auth, "Accept": "application/json",
                "Content-Type": "application/json"})
            response = connection.getresponse()
            size = len(response.read())
            status = response.status
            connection.close()
        except (OSError, http.client.HTTPException) as failure:
            error = type(failure).__name__
        took = time.time() - started
        print(f"{started:.3f},{status},{took:.4f},{size},{error}", flush=True)
        time.sleep(max(0.0, next_at - time.time()))
        if time.time() - next_at > 1.0:
            # A slow answer: the next one goes out now, not in a burst.
            next_at = time.time()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
