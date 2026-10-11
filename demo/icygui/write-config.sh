#!/usr/bin/env bash
# Writes the demo cluster's environment (environment.toml) and dashboards
# (dashboards.toml) into icygui's settings, with the running cluster's CA
# certificate, until icygui can import environments itself (topic 08).
# A wrapper around `cargo xtask demo-config` (xtask/src/demo.rs), which
# goes through icygui's own settings code; the options:
#
#   --config-dir DIR      the directory of config.toml (default: icygui's own)
#   --data-dir DIR        icygui's data directory, for --select (default: its own)
#   --ca FILE             the CA certificate (default: read from the cluster)
#   --via-proxy           master-01 through the proxy (dead-network scenario)
#   --select GROUP/DASHBOARD   the dashboard shown at start
#   --theme dark|light|system
#
# Quit icygui first: a running icygui keeps its own copy of the settings.
set -euo pipefail
cd "$(dirname "$0")/../.."
exec cargo xtask demo-config "$@"
