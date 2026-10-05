<p align="center"><img src="assets/logo/banner.png" alt="icygui" width="640"></p>

# icygui

A native desktop client for Icinga 2 on macOS and Linux, written in Rust with [GPUI](https://www.gpui.rs).

- Live monitoring from the Icinga 2 REST API's event stream, without polling dashboards.
- Problem lists in the style of Icinga Web: state circles, time in state, `service on host`, plugin output. Click a row for the host or service pane.
- Custom dashboards ("threads") in sidebar groups, each a filter over hosts or services in Icinga's own filter language.
- Operator actions: check now, acknowledge, downtimes, comments, passive check results, run commands. All runtime operations; the client never changes Icinga's configuration.
- Native notifications with per-environment, per-group, per-dashboard and per-object rules, quiet hours and storm control. The app keeps running in the menu bar / tray.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/alexykn/icygui/main/install.sh | bash
```

On macOS this installs `icygui.app` into `/Applications`. On Linux it installs the binary, desktop entry and icons into `~/.local` (no sudo). The script:
- verifies the download against the release's checksums;
- re-runs as an update;
- takes `--version X.Y.Z` to pin a release;
- removes the app with `--uninstall` (add `--purge` to drop settings too).

Pass options with `… | bash -s -- --uninstall`.

macOS builds aren't notarized by Apple yet. The installer handles that: it re-signs the app with a local identity created once on your Mac, and Gatekeeper doesn't block a curl download. If you download the `.dmg` from the [releases page](https://github.com/alexykn/icygui/releases) in a browser instead, open the app once via System Settings → Privacy & Security → *Open Anyway*. Linux `.deb` packages are on the releases page too.

Status: in development. See [`PLAN.md`](PLAN.md) for the plan and decisions, and [`docs/architecture.md`](docs/architecture.md) for the crate contracts.

## Icinga API user

The client needs an `ApiUser`. Read-only use:

```
object ApiUser "icygui" {
  password = "…"
  permissions = [
    "objects/query/Host", "objects/query/Service", "objects/query/HostGroup",
    "objects/query/ServiceGroup", "objects/query/Comment", "objects/query/Downtime",
    "objects/query/Dependency", "objects/query/Endpoint", "status/query",
    "events/CheckResult", "events/StateChange", "events/AcknowledgementSet",
    "events/AcknowledgementCleared", "events/CommentAdded", "events/CommentRemoved",
    "events/DowntimeAdded", "events/DowntimeRemoved", "events/DowntimeStarted",
    "events/DowntimeTriggered", "events/Flapping", "events/ObjectCreated",
    "events/ObjectModified", "events/ObjectDeleted",
  ]
}
```

For operators, add `actions/reschedule-check`, `actions/acknowledge-problem`, `actions/remove-acknowledgement`, `actions/schedule-downtime`, `actions/remove-downtime`, `actions/add-comment`, `actions/remove-comment`, `actions/process-check-result` and, optionally, `actions/execute-command`. Buttons for actions the user isn't allowed to run are disabled.

## Development

See [`docs/development.md`](docs/development.md) for requirements and commands. The original design handoff (HTML prototypes and the design conversation) is in [`design/`](design/).
