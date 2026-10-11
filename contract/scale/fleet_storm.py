#!/usr/bin/env python3
"""A problem storm for the fleet measurement (contract/scale/fleet.sh,
docs/performance.md): about RATE state changes a second for SECONDS
seconds, by passive check results through the API (`process-check-result`
by filter, as a mass re-check or a monitoring outage would bring them).

    fleet_storm.py URL CA RATE SECONDS > storm.csv

Every second the same scale services (`contract/scale/generate.py`'s hosts
of whole racks, 750 services each, `host.vars.rack`) flip between CRITICAL
and OK: each flip is a state change (a soft CRITICAL, then its recovery),
never a hard problem, so nobody is notified and nothing stays broken; the
last second leaves them OK, and their next checks return them to their
usual state. One request per rack, sent side by side. Prints a line per
request (`t,rack,exit_status,results,seconds,status`) and a summary on
stderr. Credentials from FLEET_STORM_USER and _PASSWORD (the demo's admin).
Only this machine: fleet.sh checks it is the demo cluster first.
"""
import base64
import concurrent.futures
import http.client
import json
import math
import os
import ssl
import sys
import time
import urllib.parse

PER_RACK = 50 * 15


def main():
    url, ca, rate, seconds = sys.argv[1], sys.argv[2], int(sys.argv[3]), float(sys.argv[4])
    parsed = urllib.parse.urlsplit(url)
    if parsed.hostname not in ("127.0.0.1", "localhost", "::1"):
        raise SystemExit(f"fleet_storm.py: {parsed.hostname} is not this machine")
    context = ssl.create_default_context(cafile=ca)
    context.check_hostname = False
    auth = "Basic " + base64.b64encode(
        f"{os.environ['FLEET_STORM_USER']}:{os.environ['FLEET_STORM_PASSWORD']}".encode()).decode()
    racks = [f"r{index:02d}" for index in range(max(1, math.ceil(rate / PER_RACK)))]

    def send(rack, exit_status):
        body = json.dumps({
            "type": "Service",
            "filter": f'host.vars.rack=="{rack}" && host.vars.scale_services',
            "exit_status": exit_status,
            "plugin_output": f"fleet storm {'CRITICAL' if exit_status else 'OK'}",
        })
        started = time.time()
        status, results = 0, 0
        try:
            connection = http.client.HTTPSConnection(parsed.hostname, parsed.port, context=context,
                                                     timeout=120)
            connection.request("POST", "/v1/actions/process-check-result", body=body, headers={
                "Authorization": auth, "Accept": "application/json",
                "Content-Type": "application/json"})
            response = connection.getresponse()
            answer = response.read()
            status = response.status
            if status == 200:
                results = len(json.loads(answer).get("results", []))
            connection.close()
        except (OSError, http.client.HTTPException, ValueError):
            pass
        return started, rack, exit_status, results, time.time() - started, status

    print("t,rack,exit_status,results,seconds,status", flush=True)
    began = time.time()
    total = 0
    futures = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=len(racks)) as pool:
        second = 0
        while time.time() - began < seconds or second % 2:
            # An odd count of seconds ends on OK.
            exit_status = 2 if second % 2 == 0 else 0
            futures += [pool.submit(send, rack, exit_status) for rack in racks]
            second += 1
            time.sleep(max(0.0, began + second - time.time()))
        for future in concurrent.futures.as_completed(futures):
            started, rack, exit_status, results, took, status = future.result()
            total += results
            print(f"{started:.3f},{rack},{exit_status},{results},{took:.3f},{status}", flush=True)
    elapsed = time.time() - began
    print(f"fleet_storm.py: {total} check results in {elapsed:.1f} s ({total / elapsed:.0f}/s), "
          f"{second} rounds of {len(racks)} racks", file=sys.stderr)


if __name__ == "__main__":
    main()
