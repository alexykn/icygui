#!/usr/bin/env python3
"""Records object, status and parameter exchanges from a real Icinga 2.

Every request here is a read-only query: object and status queries (sent as
POST with `X-HTTP-Method-Override: GET`) and `GET /v1`. Never add actions,
config changes or anything else that modifies the instance.

Reads the ICYGUI_CONTRACT_* variables printed by run-icinga.sh and writes
samples/queries.json: a list of exchanges {name, user, path, body, compare,
status, response}. ic-mock's tests/fidelity.rs replays each request against
the mock and compares the answer as `compare` says:

  exact  - the same status and body
  error  - the same status and body apart from the text of
           diagnostic_information, which only has to be there when Icinga
           sent it
  error-text - like error, and the first line of diagnostic_information
           is the same too
  names  - the same status and result names in the same order, and the
           shape of the results (keys and JSON types of attrs, joins, meta)
  shape  - the same status and the shape of the results

The object names exist both in the contract fixtures (icinga/) and in
ic-mock's prod-cluster scenario. ic-mock's tests/fidelity.rs reads the
output from samples/ directly.

Usage: set -a; . <(contract/run-icinga.sh); set +a; contract/record-queries.py
"""
import base64
import json
import os
import ssl
import sys
import urllib.error
import urllib.request

# The attributes of ic_api::Detail (docs/architecture.md): Lean, then Full.
LEAN = [
    "name", "display_name", "state", "state_type", "last_state_change",
    "last_hard_state_change", "last_check", "next_check", "next_update",
    "check_attempt", "max_check_attempts", "acknowledgement",
    "acknowledgement_expiry", "downtime_depth", "flapping", "last_reachable",
    "check_interval", "retry_interval", "groups", "vars",
]
FULL = LEAN + [
    "last_check_result", "check_command", "command_endpoint", "zone",
    "enable_active_checks", "enable_passive_checks", "enable_notifications",
    "enable_event_handler", "enable_flapping", "enable_perfdata",
    "flapping_current", "notes", "notes_url", "action_url", "icon_image",
]
HOST_LEAN = LEAN + ["address", "address6"]
HOST_FULL = FULL + ["address", "address6"]
SERVICE_LEAN = LEAN + ["host_name"]
SERVICE_FULL = FULL + ["host_name"]
SERVICES = ["db-prod-03!load", "k8s-node-07!ping4", "k8s-node-07!disk /var"]
# The fields of Icinga's CheckResult type (its JSON adds `type`).
CHECK_RESULT_FIELDS = [
    "active", "check_source", "command", "execution_end", "execution_start",
    "exit_status", "output", "performance_data", "previous_hard_state",
    "schedule_end", "schedule_start", "scheduling_source", "state", "ttl",
    "vars_after", "vars_before",
]

HOSTS_PATH = "/v1/objects/hosts"
SERVICES_PATH = "/v1/objects/services"


def x(name, path, body, compare, user="icygui"):
    return {"name": name, "user": user, "path": path, "body": body, "compare": compare}


EXCHANGES = [
    # Attribute selection: the client's Lean and Full sets.
    x("hosts-lean", HOSTS_PATH, {"attrs": HOST_LEAN}, "shape"),
    x("hosts-full", HOSTS_PATH, {"attrs": HOST_FULL}, "shape"),
    x("services-lean", SERVICES_PATH, {"attrs": SERVICE_LEAN}, "shape"),
    x("services-full", SERVICES_PATH, {"attrs": SERVICE_FULL}, "shape"),
    x("hosts-full-by-name", HOSTS_PATH,
      {"hosts": ["k8s-node-11", "db-prod-03"], "attrs": HOST_FULL}, "names"),
    x("services-full-by-name", SERVICES_PATH,
      {"services": SERVICES, "attrs": SERVICE_FULL}, "names"),
    x("services-lean-by-name", SERVICES_PATH,
      {"services": SERVICES, "attrs": SERVICE_LEAN}, "names"),
    x("attrs-empty", HOSTS_PATH, {"hosts": ["db-prod-03"], "attrs": []}, "exact"),
    x("attrs-duplicate", HOSTS_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["name", "name", "display_name"]}, "exact"),
    x("attrs-hidden", HOSTS_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["state_raw", "name"]}, "exact"),
    x("attrs-navigation-only", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["host", "name"]}, "exact"),
    # Unknown attributes.
    x("attrs-unknown", HOSTS_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["name", "bogus", "other"], "verbose": True}, "exact"),
    x("attrs-unknown-all-objects", SERVICES_PATH, {"attrs": ["bogus"]}, "exact"),
    x("attrs-of-another-type", HOSTS_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["host_name"]}, "exact"),
    x("attrs-service-address", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["address"]}, "exact"),
    x("attrs-dotted", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["host.name"]}, "exact"),
    x("attrs-number", HOSTS_PATH, {"hosts": ["db-prod-03"], "attrs": [1]}, "exact"),
    x("attrs-unknown-without-targets", HOSTS_PATH,
      {"filter": "false", "attrs": ["bogus"], "joins": ["host.bogus"]}, "exact", "root"),
    x("attrs-not-an-array", HOSTS_PATH, {"hosts": ["db-prod-03"], "attrs": "name"}, "exact"),
    # Joins.
    x("joins-attributes", SERVICES_PATH,
      {"services": SERVICES, "attrs": ["name"],
       "joins": ["host.name", "host.state", "host.address", "check_command.name"]}, "names"),
    x("joins-whole-host", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "joins": ["host"]}, "names"),
    x("joins-whole-wins", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "joins": ["host.name", "host"]}, "names"),
    x("joins-without-objects", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"],
       "joins": ["bogus", "bogus.name", "check_period", "command_endpoint.name"]}, "exact"),
    x("joins-hidden-field", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "joins": ["host.state_raw", "host.name"]}, "exact"),
    x("joins-unknown-field", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "joins": ["host.bogus"]}, "exact"),
    x("joins-empty-field", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "joins": ["host."]}, "exact"),
    x("joins-nested-field", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "joins": ["host.name.x"]}, "exact"),
    x("joins-not-an-array", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "joins": "host"}, "exact"),
    x("joins-check-command", HOSTS_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["name"], "joins": ["check_command"]}, "names"),
    x("joins-dependencies", "/v1/objects/dependencies",
      {"attrs": ["name"], "joins": ["child_host.name", "parent_host.name"]}, "shape"),
    x("all-joins", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "all_joins": 1}, "names"),
    x("all-joins-zero-string", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "all_joins": "0"}, "exact"),
    x("all-joins-not-a-number", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "all_joins": "yes"}, "exact"),
    # Meta.
    x("meta-service", SERVICES_PATH,
      {"services": ["db-prod-03!load"], "attrs": ["name"], "meta": ["used_by", "location"]}, "names"),
    x("meta-host", HOSTS_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["name"], "meta": ["used_by", "location"]}, "names"),
    x("meta-empty", HOSTS_PATH, {"hosts": ["db-prod-03"], "attrs": ["name"], "meta": []}, "exact"),
    x("meta-unknown", HOSTS_PATH, {"hosts": ["db-prod-03"], "attrs": ["name"], "meta": ["bogus"]}, "exact"),
    x("meta-not-an-array", HOSTS_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["name"], "meta": "used_by"}, "exact"),
    # Name lists.
    x("names-in-order", HOSTS_PATH, {"hosts": ["k8s-node-11", "db-prod-03"], "attrs": ["name"]}, "exact"),
    x("names-duplicate", HOSTS_PATH, {"hosts": ["db-prod-03", "db-prod-03"], "attrs": ["name"]}, "exact"),
    x("names-singular-first", HOSTS_PATH,
      {"host": "k8s-node-11", "hosts": ["db-prod-03"], "attrs": ["name"]}, "exact"),
    x("names-url-first", HOSTS_PATH + "/db-prod-03", {"hosts": ["k8s-node-11"], "attrs": ["name"]}, "exact"),
    x("names-services", SERVICES_PATH,
      {"services": ["k8s-node-07!ping4", "db-prod-03!load"], "attrs": ["name", "host_name"]}, "exact"),
    x("names-one-missing", SERVICES_PATH,
      {"services": ["db-prod-03!load", "db-prod-03!no-such-service"], "attrs": ["name"]}, "exact"),
    x("names-one-missing-verbose", SERVICES_PATH,
      {"services": ["db-prod-03!load", "db-prod-03!no-such-service"], "verbose": True}, "error-text"),
    x("names-singular-missing", SERVICES_PATH, {"service": "db-prod-03!no-such-service"}, "exact"),
    x("names-wrong-type", HOSTS_PATH, {"hosts": ["k8s-node-07!ping4"]}, "exact"),
    x("names-host-only", SERVICES_PATH, {"services": ["db-prod-03"]}, "exact"),
    x("names-case-sensitive", HOSTS_PATH, {"hosts": ["DB-PROD-03"]}, "exact"),
    x("names-number", HOSTS_PATH, {"hosts": [1]}, "exact"),
    x("names-not-an-array", HOSTS_PATH, {"hosts": "db-prod-03"}, "exact"),
    x("names-empty-means-all", HOSTS_PATH, {"hosts": [], "attrs": ["name"]}, "shape"),
    x("names-null-means-all", HOSTS_PATH, {"hosts": None, "attrs": ["name"]}, "shape"),
    x("names-of-another-type-are-ignored", SERVICES_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["name"]}, "shape"),
    # Filters (the root user has `filter-expression`).
    x("filter-forbidden", HOSTS_PATH, {"filter": "true"}, "exact"),
    x("filter-names", HOSTS_PATH,
      {"filter": "host.name in names", "filter_vars": {"names": ["k8s-node-11", "db-prod-03"]},
       "attrs": ["name"]}, "shape", "root"),
    x("filter-regex", HOSTS_PATH,
      {"filter": "regex(\"^k8s-node-(07|11)$\", host.name)", "attrs": ["name"]}, "shape", "root"),
    x("filter-joined-host", SERVICES_PATH,
      {"filter": "service.host.name == \"db-prod-03\" && host.name == service.host_name && service.name == \"load\"",
       "attrs": ["name"]}, "exact", "root"),
    x("filter-empty", HOSTS_PATH, {"filter": "", "attrs": ["name"]}, "exact", "root"),
    x("filter-blank", HOSTS_PATH, {"filter": "  ", "attrs": ["name"]}, "exact", "root"),
    x("filter-syntax-error", HOSTS_PATH, {"filter": "host.name ==", "verbose": True}, "error", "root"),
    x("filter-statement", HOSTS_PATH, {"filter": "var x = 1", "verbose": True}, "error", "root"),
    x("filter-assignment", HOSTS_PATH, {"filter": "host.name = \"x\"", "verbose": True}, "error", "root"),
    x("filter-unknown-function", HOSTS_PATH, {"filter": "nonexistent(1)", "verbose": True}, "error", "root"),
    x("filter-unsafe-function", HOSTS_PATH, {"filter": "get_host(\"db-prod-03\")", "verbose": True}, "error", "root"),
    x("filter-undefined-variable", HOSTS_PATH, {"filter": "nothing == 1", "verbose": True}, "error-text", "root"),
    x("filter-unknown-attribute", HOSTS_PATH, {"filter": "host.bogus == 1", "verbose": True}, "error-text", "root"),
    x("filter-hidden-attribute", HOSTS_PATH, {"filter": "host.state_raw == 1", "verbose": True}, "error-text", "root"),
    x("filter-type-error", HOSTS_PATH, {"filter": "host.name < 1", "verbose": True}, "error", "root"),
    # A filter that doesn't compile fails when it is evaluated (Icinga
    # compiles it into a ThrowExpression): a type without objects finds
    # nothing, and invalid filter_vars, read first, are the error.
    x("filter-syntax-error-without-objects", "/v1/objects/graphitewriters",
      {"filter": "graphitewriter.name ==", "verbose": True}, "exact", "root"),
    x("filter-statement-without-objects", "/v1/objects/graphitewriters",
      {"filter": "var x = 1", "verbose": True}, "exact", "root"),
    x("filter-syntax-error-and-filter-vars", HOSTS_PATH,
      {"filter": "host.name ==", "filter_vars": "x", "verbose": True}, "error-text", "root"),
    # Check results are CheckResult objects: their fields, and no others.
    x("filter-check-result-fields", SERVICES_PATH,
      {"filter": "service.__name == \"db-prod-03!load\" && len(["
                 + ", ".join("service.last_check_result." + field for field in CHECK_RESULT_FIELDS)
                 + "]) == " + str(len(CHECK_RESULT_FIELDS)),
       "attrs": ["name"]}, "exact", "root"),
    x("filter-check-result-unknown-field", SERVICES_PATH,
      {"filter": "service.__name == \"db-prod-03!load\" && service.last_check_result.outptu == null",
       "verbose": True}, "error-text", "root"),
    x("filter-check-result-type-is-no-field", SERVICES_PATH,
      {"filter": "service.__name == \"db-prod-03!load\" && service.last_check_result.type != null",
       "verbose": True}, "error-text", "root"),
    # Targeted filters (ApplyRule::GetTargetHosts/GetTargetServices): looked
    # up by name, not evaluated, so in filter order with duplicates.
    x("filter-targeted-hosts", HOSTS_PATH,
      {"filter": "host.name == \"k8s-node-11\" || \"db-prod-03\" == host.name"
                 " || host[\"name\"] == n || host.name == \"no-such-host\"",
       "filter_vars": {"n": "k8s-node-07"}, "attrs": ["name"]}, "exact", "root"),
    x("filter-targeted-duplicates", HOSTS_PATH,
      {"filter": "(host.name == \"db-prod-03\") || host.name == \"db-prod-03\"", "attrs": ["name"]},
      "exact", "root"),
    x("filter-targeted-no-such-host", HOSTS_PATH,
      {"filter": "host.name == \"no-such-host\"", "attrs": ["name"]}, "exact", "root"),
    x("filter-targeted-services", SERVICES_PATH,
      {"filter": "host.name == \"k8s-node-07\" && service.name == \"ping4\""
                 " || service.name == \"load\" && host.name == \"db-prod-03\""
                 " || host.name == \"db-prod-03\" && service.name == \"load\"",
       "attrs": ["name"]}, "exact", "root"),
    x("filter-vars-not-a-dictionary", HOSTS_PATH,
      {"filter": "host.name in names", "filter_vars": "x", "verbose": True}, "error-text", "root"),
    x("filter-constants", HOSTS_PATH,
      {"filter": "host.name == \"db-prod-03\" && OK == \"OK\" && DowntimeNoChildren == \"DowntimeNoChildren\""
                 " && ServiceCritical == 2 && MatchAny == 1 && typeof(NodeName) == String",
       "attrs": ["name"]}, "exact", "root"),
    x("filter-no-such-constant", HOSTS_PATH,
      {"filter": "AcknowledgementSticky == 2", "verbose": True}, "error-text", "root"),
    x("status-filter", "/v1/status", {"filter": "dictionary.name == \"CIB\" && obj.name == \"CIB\""}, "names", "root"),
    x("status-filter-no-status-variable", "/v1/status",
      {"filter": "status.name == \"CIB\"", "verbose": True}, "error-text", "root"),
    x("status-filter-empty", "/v1/status", {"filter": ""}, "exact", "root"),
    x("status-filter-forbidden", "/v1/status", {"filter": "true"}, "exact"),
    # Flags Icinga converts through numbers.
    x("pretty-not-a-number", HOSTS_PATH, {"hosts": ["db-prod-03"], "attrs": ["name"], "pretty": "yes"}, "exact"),
    x("verbose-not-a-number", HOSTS_PATH, {"hosts": ["no-such-host"], "verbose": "yes"}, "exact"),
    x("verbose-not-a-number-success", HOSTS_PATH,
      {"hosts": ["db-prod-03"], "attrs": ["name"], "verbose": "yes"}, "exact"),
    x("info-pretty-not-a-number", "/v1", {"pretty": "yes"}, "exact"),
]


def main():
    env = os.environ
    url = env["ICYGUI_CONTRACT_URL"].rstrip("/")
    users = {
        "icygui": (env["ICYGUI_CONTRACT_USER"], env["ICYGUI_CONTRACT_PASSWORD"]),
        "root": (env["ICYGUI_CONTRACT_ADMIN_USER"], env["ICYGUI_CONTRACT_ADMIN_PASSWORD"]),
    }
    # The chain is verified against Icinga's CA. The URL names 127.0.0.1
    # while the certificate names the node (ICYGUI_CONTRACT_SERVER_NAME),
    # which urllib can't verify against, so the name check is off.
    context = ssl.create_default_context(cafile=env["ICYGUI_CONTRACT_CA_FILE"])
    context.check_hostname = False
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context))
    out = []
    for exchange in EXCHANGES:
        user, password = users[exchange["user"]]
        token = base64.b64encode(f"{user}:{password}".encode()).decode()
        request = urllib.request.Request(
            url + exchange["path"], data=json.dumps(exchange["body"]).encode(), method="POST",
            headers={"Authorization": "Basic " + token, "Accept": "application/json",
                     "Content-Type": "application/json", "X-HTTP-Method-Override": "GET"})
        try:
            with opener.open(request, timeout=30) as response:
                status, text = response.status, response.read()
        except urllib.error.HTTPError as error:
            status, text = error.code, error.read()
        body = json.loads(text)
        # Stack traces (non-script errors with `verbose`) are noise here.
        if isinstance(body, dict) and "diagnostic_information" in body:
            diagnostic = body["diagnostic_information"]
            body["diagnostic_information"] = diagnostic.split("\n\nStacktrace:")[0]
        recorded = dict(exchange, status=status, response=body)
        out.append(recorded)
        print(f"{status} {exchange['name']}", file=sys.stderr)
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "samples", "queries.json")
    with open(path, "w") as file:
        json.dump(out, file, indent=1, sort_keys=False)
        file.write("\n")


if __name__ == "__main__":
    main()
