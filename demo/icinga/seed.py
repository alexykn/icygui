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
       seed.py --never-checked [--reschedule]
  --marker  written once the seed is done (the health check waits for
            it); FILE.failed instead if it failed (the health check says
            so, and demo/up.sh stops waiting)
  --feed    then keeps going as the outside system that feeds the passive
            checks: their results again every SECONDS, so they stay fresh
            (Icinga, and icygui with it, calls a passive check late after
            twice its interval without a result)
  --never-checked
            only prints how many objects with active checks were never
            checked, and the first of their names, on one line (`0` when
            every check ran); with --reschedule it also reschedules them
            at once (demo/up.sh, which never says healthy while one never
            ran)
"""
import base64
import http.client
import json
import os
import ssl
import sys
import time
import traceback

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import generate  # noqa: E402

# The demo's admin user (generate.API_USERS): a demo value, not a secret.
AUTH = "Basic " + base64.b64encode(b"demo-admin:demo-admin-password").decode()
CONTEXT = ssl.create_default_context(cafile="/var/lib/icinga2/certs/ca.crt")


def log(message):
    print(time.strftime("%H:%M:%S"), message, flush=True)


# Transient failures of a call (Icinga busy starting or reloading on a slow
# machine): retried this often, a few seconds apart, before they count.
RETRIES = 6


class OutOfTime(Exception):
    """The seed ran past its whole budget ([`DEADLINE`]): it gives up (and
    writes its failure marker) instead of retrying on."""


def remaining():
    """Seconds left until the seed's [`DEADLINE`] (no limit once seeded:
    the feed runs on)."""
    if not SEEDING:
        return float("inf")
    return STARTED + DEADLINE - time.time()


def call(method, path, body=None):
    """One API call; connection errors and 5xx are retried (Icinga may be
    busy starting or reloading), the last error raised. Never past the
    seed's [`DEADLINE`]: a hung master can't keep it retrying."""
    for attempt in range(1, RETRIES + 1):
        if remaining() <= 0:
            raise OutOfTime(f"{method} {path}: out of time after {DEADLINE} s")
        try:
            status, answer = call_once(method, path, body)
        except (OSError, http.client.HTTPException) as error:
            if attempt == RETRIES or remaining() <= 0:
                raise
            log(f"{method} {path}: {error!r}; retrying")
        else:
            if status < 500 or attempt == RETRIES:
                return status, answer
            log(f"{method} {path}: {status}; retrying")
        time.sleep(max(0, min(2 * attempt, 10, remaining())))
    raise AssertionError("unreachable")


def call_once(method, path, body=None):
    # A request never outlasts the seed's deadline by more than a moment.
    timeout = max(5, min(60, remaining()))
    connection = http.client.HTTPSConnection("master-01", 5665, context=CONTEXT, timeout=timeout)
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


# All the seed's waits together (the API, the cluster, the first checks,
# the problems to acknowledge) end within this, so the seed is done well
# before demo/up.sh gives up (20 minutes by default): a slow 2-core runner
# takes about two minutes for all of them; one that never finishes is
# logged and the seed goes on with what it has (first checks that never
# ran are rescheduled first, and demo/up.sh doesn't say healthy while one
# never ran).
BUDGET = 600
# And the whole seed, its calls and their retries too, ends within this
# (the waits' budget and two minutes for the seeding itself; a call in
# flight at the deadline may take 5 s more): then it gives up and writes
# its failure marker, still well before demo/up.sh's 20 minutes.
DEADLINE = BUDGET + 120
STARTED = time.time()
# The deadline holds while seeding; the feed afterwards has none.
SEEDING = True


def wait(what, condition, seconds):
    """Waits for `condition`, saying what it waits for (the health check's
    output and seed.log show it), at most `seconds` and never past the
    seed's [`BUDGET`]."""
    seconds = max(5, min(seconds, STARTED + BUDGET - time.time()))
    log(f"waiting for {what} (at most {seconds:.0f} s)")
    started = time.time()
    deadline = started + seconds
    while True:
        try:
            if condition():
                log(f"{what}: after {time.time() - started:.0f} s")
                return True
        except (OSError, http.client.HTTPException, KeyError, IndexError, ValueError) as error:
            last = error
        else:
            last = None
        if time.time() > deadline:
            log(f"gave up waiting for {what}" + (f" ({last!r})" if last else ""))
            return False
        time.sleep(5)


def connected():
    status, answer = call("GET", "/v1/status/ApiListener")
    return status == 200 and not answer["results"][0]["status"]["api"]["not_conn_endpoints"]


def never_checked():
    """The objects with active checks that haven't been checked yet, as
    (`Host` or `Service`, full name)."""
    found = []
    for kind, plural in (("Host", "hosts"), ("Service", "services")):
        status, answer = call("QUERY", f"/v1/objects/{plural}", {
            "attrs": ["last_check", "enable_active_checks"]})
        if status != 200:
            raise KeyError(f"{plural}: {status}")
        found += [(kind, r["name"]) for r in answer["results"]
                  if r["attrs"]["enable_active_checks"] and r["attrs"]["last_check"] <= 0]
    return found


def unchecked():
    """How many objects with active checks haven't been checked yet."""
    return len(never_checked())


def named(objects, most=8):
    """`objects`' names for a log line: the first `most`, then how many
    more."""
    names = [name for _, name in objects]
    more = f" and {len(names) - most} more" if len(names) > most else ""
    return ", ".join(names[:most]) + more


def reschedule(objects):
    """Runs the checks of `objects` now (`reschedule-check`, forced), by
    name, in batches. Icinga 2.15 sometimes leaves first checks unrun in
    the HA zone fra on a loaded machine: their `next_check` passes and
    nothing runs them (ledger, 2026-10-11); a forced reschedule runs them
    at once."""
    done = 0
    for kind in ("Host", "Service"):
        names = [name for k, name in objects if k == kind]
        attribute = "host.name" if kind == "Host" else "service.__name"
        for start in range(0, len(names), 200):
            batch = names[start:start + 200]
            done += len(batch) if action("reschedule-check", {
                "type": kind, "filter": f"{attribute} in names",
                "filter_vars": {"names": batch}, "force": True}) else 0
    return done


def first_checks():
    """Waits for every object's first check; what is still unchecked after
    that is rescheduled (and named in the log) and waited for once more.
    Returns whether every check ran: demo/up.sh doesn't say healthy
    otherwise (it reschedules them again and waits, or fails naming
    them)."""
    if wait("every object's first check", lambda: unchecked() == 0, 240):
        return True
    late = never_checked()
    log(f"{len(late)} objects with active checks never checked: {named(late)}; "
        "rescheduling them (reschedule-check, forced)")
    log(f"rescheduled {reschedule(late)} of {len(late)}")
    if wait("the rescheduled checks", lambda: unchecked() == 0, 180):
        return True
    late = never_checked()
    log(f"still never checked after rescheduling: {len(late)}: {named(late)} "
        "(demo/up.sh tries again and doesn't say healthy until they ran)")
    return False


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
    # Within the BUDGET: the cluster usually connects within a minute and
    # checks everything within its longest check interval (5 minutes).
    if not wait("the API", lambda: call_once("GET", "/v1")[0] == 200, 300):
        return 1
    wait("the cluster to connect", connected, 300)
    first_checks()
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


def seed_until_done(attempts=3):
    """`main`, again after an error that the retries of each call didn't
    cover (a reload in the middle): it only adds what is missing. An error
    is logged in full, so the health check's `seeding` says why."""
    for attempt in range(1, attempts + 1):
        try:
            return main()
        except OutOfTime as error:
            log(f"seeding failed: {error}")
            return 1
        except Exception:  # noqa: BLE001 - logged, then tried again
            log(f"seeding failed (attempt {attempt} of {attempts}):\n{traceback.format_exc()}")
            if remaining() <= 10:
                log(f"seeding failed: out of time after {DEADLINE} s")
                return 1
            time.sleep(10)
    return 1


def report_never_checked(again):
    """`--never-checked`: one line, the count first (`0`: every check
    ran); `again` reschedules them too."""
    late = never_checked()
    if not late:
        print("0")
        return
    rescheduled = f"; rescheduled {reschedule(late)}" if again else ""
    print(f"{len(late)} objects with active checks never checked: {named(late)}{rescheduled}")


if __name__ == "__main__":
    args = sys.argv[1:]
    if "--never-checked" in args:
        SEEDING = False
        try:
            report_never_checked("--reschedule" in args)
        except (OSError, http.client.HTTPException, KeyError) as error:
            print(f"? the API didn't answer: {error!r}")
            sys.exit(2)
        sys.exit(0)
    marker = args[args.index("--marker") + 1] if "--marker" in args else None
    every = float(args[args.index("--feed") + 1]) if "--feed" in args else None
    status = seed_until_done()
    SEEDING = False
    log("seeded" if status == 0 else "seeding failed")
    if marker:
        # The health check reads it: `ok`, or that the seed failed (so a
        # start that can't succeed says so at once instead of waiting out
        # demo/up.sh's whole timeout).
        with open(marker if status == 0 else marker + ".failed", "w") as out:
            out.write("ok\n" if status == 0 else "seeding failed, see /var/log/icinga2/seed.log\n")
    if status == 0 and every:
        feed(every)
    sys.exit(status)
