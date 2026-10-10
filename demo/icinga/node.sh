#!/bin/sh
# Starts one node of the demo cluster (docker-compose.yml): the node's
# certificates from the `pki` volume, its constants, zones, features and
# API listener, then `icinga2 daemon`. The node is the container's
# hostname. The image's entrypoint has already filled /data (where
# /etc/icinga2 and /var/lib/icinga2 live).
#
# The cluster (one zones.conf for everyone):
#   master  master-01, master-02  (HA; master-01 is the config master)
#   ams     sat-ams-01            (satellite zone, parent master)
#   fra     sat-fra-01, sat-fra-02 (HA satellite zone, parent master)
#   global  global-templates, director-global
# Children connect to their parents and to the other endpoints of their
# zone; the masters connect to each other, never down to a satellite.
set -eu

NODE=$(hostname)
case "$NODE" in
  master-*) ZONE=master ;;
  sat-ams-*) ZONE=ams ;;
  sat-fra-*) ZONE=fra ;;
  *) echo "node.sh: unknown node $NODE" >&2; exit 1 ;;
esac
ETC=/etc/icinga2
LIB=/var/lib/icinga2

# Certificates: the node's own from the `pki` volume; the CA with its key
# only on the config master, which could sign requests from new nodes (the
# `ca` volume, mounted by master-01 alone; docker-compose.yml).
for _ in $(seq 1 120); do
  [ -f "/pki/$NODE/$NODE.crt" ] && break
  sleep 1
done
mkdir -p "$LIB/certs"
cp "/pki/$NODE/$NODE.crt" "/pki/$NODE/$NODE.key" "/pki/$NODE/ca.crt" "$LIB/certs/"
if [ "$NODE" = master-01 ]; then
  mkdir -p "$LIB/ca"
  cp /ca/ca.crt /ca/ca.key "$LIB/ca/"
fi

cat >"$ETC/constants.conf" <<EOF
const PluginDir = "/usr/lib/nagios/plugins"
const ManubulonPluginDir = "/usr/lib/nagios/plugins"
const PluginContribDir = "/usr/lib/nagios/plugins"
const NodeName = "$NODE"
const ZoneName = "$ZONE"
const TicketSalt = ""
EOF

# No conf.d: everything but the node's own setup comes through the zones.
# No plugin commands either: the demo's checks are dummy checks under the
# plugins' names (generate.py); <itl> brings dummy, icinga and cluster-zone.
cat >"$ETC/icinga2.conf" <<'EOF'
include "constants.conf"
include "zones.conf"
include <itl>
include "features-enabled/*.conf"
include "local.d/*.conf"
EOF

endpoint() { # endpoint <name> <its zone>
  if [ "$1" = "$NODE" ] || { [ "$ZONE" = master ] && [ "$2" != master ]; }; then
    echo "object Endpoint \"$1\" { }"
  else
    echo "object Endpoint \"$1\" { host = \"$1\" }"
  fi
}
{
  endpoint master-01 master
  endpoint master-02 master
  endpoint sat-ams-01 ams
  endpoint sat-fra-01 fra
  endpoint sat-fra-02 fra
  cat <<'EOF'
object Zone "master" { endpoints = [ "master-01", "master-02" ] }
object Zone "ams" { endpoints = [ "sat-ams-01" ]; parent = "master" }
object Zone "fra" { endpoints = [ "sat-fra-01", "sat-fra-02" ]; parent = "master" }
object Zone "global-templates" { global = true }
object Zone "director-global" { global = true }
EOF
} >"$ETC/zones.conf"

# Every node accepts configuration: master-02 and the satellites take
# theirs from master-01, and master-01 takes the objects made at run time
# through master-02 (downtimes, comments); Icinga ignores synced zone
# configuration for the zones a node has in zones.d itself. Every node runs
# commands for the others (checks pinned with command_endpoint). The
# masters require the filter-expression permission for filters, as Icinga
# 2.17 will by default (contract/icinga/api.conf).
enforce=false
[ "$ZONE" = master ] && enforce=true
cat >"$ETC/features-available/api.conf" <<EOF
object ApiListener "api" {
  accept_config = true
  accept_commands = true
  ticket_salt = TicketSalt
  enforce_filter_expression_permission = $enforce
}
EOF
rm -f "$ETC"/features-enabled/*
features="api checker"
[ "$ZONE" = master ] && features="$features notification"
for feature in $features; do
  ln -s "../features-available/$feature.conf" "$ETC/features-enabled/$feature.conf"
done

# This node's own API user for its health check (docker-compose.yml);
# outside the zones, so it is never synced. A demo value, not a secret.
mkdir -p "$ETC/local.d"
cat >"$ETC/local.d/healthcheck.conf" <<'EOF'
object ApiUser "healthcheck" {
  password = "healthcheck"
  permissions = [ "status/query" ]
}
EOF

# The objects: generated on the config master (demo/icinga/generate.py,
# the same every time), plus the contract tests' fixtures, so the
# contract tests run against this cluster too. The others get theirs
# through the cluster's config sync.
rm -rf "${ETC:?}/zones.d" "${ETC:?}/conf.d"
mkdir -p "$ETC/zones.d"
if [ "$NODE" = master-01 ]; then
  python3 -I /demo/icinga/generate.py "$ETC/zones.d"
  cp /contract/icinga/icygui-test.conf "$ETC/zones.d/master/"
  cp /contract/icinga/icygui-groups.conf "$ETC/zones.d/global-templates/"
  # The production-size workload for measurements (docker-compose.yml):
  # N hosts x 15 services of the scale benchmark in the master zone, so
  # both masters check them (HA) and serve them, as at the user's site.
  scale=${ICYGUI_DEMO_SCALE_HOSTS:-0}
  case "$scale" in
    '' | *[!0-9]*) echo "node.sh: ICYGUI_DEMO_SCALE_HOSTS=$scale is not a number" >&2; exit 1 ;;
  esac
  if [ "$scale" -gt 0 ]; then
    python3 -I /contract/scale/generate.py "$scale" >"$ETC/zones.d/master/scale.conf"
    echo "node.sh: the scale workload, $scale hosts x 15 services" >&2
  fi
fi

if ! check=$(icinga2 daemon -C 2>&1); then
  echo "$check" | grep -v "information/" >&2
  exit 1
fi
if [ "$NODE" = master-01 ]; then
  # Acknowledgements, comments and downtimes, once the cluster has
  # checked everything (demo/icinga/seed.py; the health check waits for
  # it). Again after every start: it only adds what is missing.
  # It then feeds the passive checks as an outside system would.
  rm -f /tmp/seeded
  (python3 -I /demo/icinga/seed.py --marker /tmp/seeded --feed 50 >/var/log/icinga2/seed.log 2>&1 &)
fi
exec icinga2 daemon
