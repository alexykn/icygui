#!/usr/bin/env python3
"""Gives the demo cluster's handling, downtime and comment views something
to show: acknowledgements, comments and downtimes by the demo's people,
and passive results for the contract fixtures' passive services (the
lists are `SEED` in generate.py). Runs on master-01 after every start
(node.sh), as the demo's admin API user, once the cluster is connected and
has checked everything; the health check waits for it.

It only adds what is missing: an acknowledgement on an object that has
none, a comment or downtime that isn't there with the same author and
text. So a restart keeps what is there, and expired downtimes come back
relative to the new start.

Usage: seed.py [--marker FILE] [--feed SECONDS]
  --marker  touched once the seed is done (the health check waits for it)
  --feed    then keeps going as the outside system that feeds the passive
            checks: their results again every SECONDS, so they stay fresh
            (Icinga, and icygui with it, calls a passive check late after
            twice its interval without a result)
"""
import base64
import http.client
import json
import os
import ssl
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import generate  # noqa: E402

# The demo's admin user (generate.API_USERS): a demo value, not a secret.
AUTH = "Basic " + base64.b64encode(b"demo-admin:demo-admin-password").decode()
CONTEXT = ssl.create_default_context(cafile="/var/lib/icinga2/certs/ca.crt")


def log(message):
    print(time.strftime("%H:%M:%S"), message, flush=True)


def call(method, path, body=None):
    connection = http.client.HTTPSConnection("master-01", 5665, context=CONTEXT, timeout=60)
    headers = {"Authorization": AUTH, "Accept": "application/json"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    if method == "QUERY":
        method, headers["X-HTTP-Method-Override"] = "POST", "GET"
    connection.request(method, path, json.dumps(body) if body is not None else None, headers)
    response = connection.getresponse()
    data = response.read()
    connection.close()
    try:
        answer = json.loads(data) if data else {}
    except ValueError:
        answer = {"raw": data[:200].decode(errors="replace")}
    return response.status, answer


def wait(what, condition, seconds):
    deadline = time.time() + seconds
    while True:
        try:
            if condition():
                return True
        except (OSError, http.client.HTTPException, KeyError, IndexError) as error:
            last = error
        else:
            last = None
        if time.time() > deadline:
            log(f"gave up waiting for {what}" + (f" ({last})" if last else ""))
            return False
        time.sleep(5)


def connected():
    status, answer = call("GET", "/v1/status/ApiListener")
    return status == 200 and not answer["results"][0]["status"]["api"]["not_conn_endpoints"]


def unchecked():
    """Objects with active checks that haven't been checked yet."""
    count = 0
    for kind in ("hosts", "services"):
        status, answer = call("QUERY", f"/v1/objects/{kind}", {
            "attrs": ["last_check", "enable_active_checks"]})
        if status != 200:
            raise KeyError(f"{kind}: {status}")
        count += sum(1 for r in answer["results"]
                     if r["attrs"]["enable_active_checks"] and r["attrs"]["last_check"] <= 0)
    return count


def target(host, service):
    if service is None:
        return {"type": "Host", "host": host}
    return {"type": "Service", "service": f"{host}!{service}"}


def object_state(host, service):
    kind, name = ("hosts", host) if service is None else ("services", f"{host}!{service}")
    status, answer = call("QUERY", f"/v1/objects/{kind}", {
        kind: [name], "attrs": ["state", "state_type", "acknowledgement"]})
    if status != 200 or not answer.get("results"):
        return None
    return answer["results"][0]["attrs"]


def action(name, body):
    status, answer = call("POST", f"/v1/actions/{name}", body)
    results = answer.get("results") or [{}]
    ok = status == 200 and all(r.get("code", 200) == 200 for r in results)
    if not ok:
        log(f"{name} failed: {status} {json.dumps(answer)[:300]}")
    return ok


def existing(kind):
    """The comments or downtimes there are (each type knows only its own
    attributes: asking for another's fails the whole query)."""
    attrs = {"comments": ["author", "text", "host_name", "service_name"],
             "downtimes": ["author", "comment", "host_name", "service_name", "end_time"]}[kind]
    status, answer = call("QUERY", f"/v1/objects/{kind}", {"attrs": attrs})
    if status != 200:
        raise OSError(f"{kind}: {status} {json.dumps(answer)[:200]}")
    return answer.get("results", [])


def main():
    if not wait("the API", lambda: call("GET", "/v1")[0] == 200, 300):
        return 1
    wait("the cluster to connect", connected, 600)
    wait("every object's first check", lambda: unchecked() == 0, 900)
    seed = generate.SEED

    for host, service, state, output, perfdata in seed["results"]:
        # Three results: past max_check_attempts, a hard state.
        attrs = object_state(host, service) or {}
        if attrs.get("state") == state and attrs.get("state_type") == 1:
            continue
        for _ in range(3):
            action("process-check-result", {**target(host, service), "exit_status": state,
                                            "plugin_output": output, "performance_data": perfdata})
    log(f"{len(seed['results'])} passive results")

    # Acknowledgements need the problem to be there (the stuck checks turn
    # hard within a few retries).
    def problems():
        return all((object_state(h, s) or {}).get("state", 0) != 0
                   for h, s, _, _ in seed["acknowledgements"])
    wait("the acknowledged problems", problems, 300)
    added = 0
    for host, service, author, comment in seed["acknowledgements"]:
        attrs = object_state(host, service)
        if not attrs or attrs["state"] == 0 or attrs["acknowledgement"] != 0:
            continue
        added += action("acknowledge-problem", {**target(host, service), "author": author,
                                                "comment": comment, "sticky": True, "notify": False})
    log(f"{added} acknowledgements added")

    comments = existing("comments")
    added = 0
    for host, service, author, text in seed["comments"]:
        if any(c["attrs"]["host_name"] == host and c["attrs"]["service_name"] == (service or "")
               and c["attrs"]["author"] == author and c["attrs"]["text"] == text for c in comments):
            continue
        added += action("add-comment", {**target(host, service), "author": author, "comment": text})
    log(f"{added} comments added")

    downtimes = existing("downtimes")
    now = time.time()
    added = 0
    for host, service, author, text, start, end, flexible, all_services in seed["downtimes"]:
        if any(d["attrs"]["host_name"] == host and d["attrs"]["service_name"] == (service or "")
               and d["attrs"]["author"] == author and d["attrs"]["comment"] == text
               and d["attrs"]["end_time"] > now for d in downtimes):
            continue
        body = {**target(host, service), "author": author, "comment": text,
                "start_time": int(now + start * 60), "end_time": int(now + end * 60),
                "fixed": flexible == 0}
        if flexible:
            body["duration"] = flexible * 60
        if all_services:
            body["all_services"] = True
        added += action("schedule-downtime", body)
    log(f"{added} downtimes added")
    return 0


def feed(seconds):
    while True:
        time.sleep(seconds)
        for host, service, state, output, perfdata in generate.SEED["results"]:
            try:
                action("process-check-result", {**target(host, service), "exit_status": state,
                                                "plugin_output": output, "performance_data": perfdata})
            except (OSError, http.client.HTTPException) as error:
                log(f"feeding {host}!{service}: {error}")


if __name__ == "__main__":
    args = sys.argv[1:]
    marker = args[args.index("--marker") + 1] if "--marker" in args else None
    every = float(args[args.index("--feed") + 1]) if "--feed" in args else None
    status = main()
    if status == 0 and marker:
        open(marker, "w").close()
    if status == 0 and every:
        feed(every)
    sys.exit(status)
