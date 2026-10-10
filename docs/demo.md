# The icygui demo: a real Icinga cluster in Docker

The demo is a real Icinga 2.15 cluster that runs on your machine in Docker, with a believable estate that changes by itself, and a set of icygui dashboards made for it. Point icygui at it to see every view and the cluster health page with real data, and break the cluster on purpose (a satellite down, a frozen master, a dead network) to see what icygui tells you. Nothing in it talks to the outside world, and it never joins a real Icinga.

Contents: [What runs](#what-runs) · [Start it](#start-it) · [Connect icygui](#connect-icygui) · [The demo's passwords](#the-demos-passwords) · [Break it on purpose](#break-it-on-purpose) · [Stop and clean up](#stop-and-clean-up) · [Troubleshooting](#troubleshooting)

## What runs

| Zone | Endpoints | What it checks |
|---|---|---|
| `master` | `master-01`, `master-02` (an HA pair; master-01 holds the configuration) | the core estate: web, API, databases, Kubernetes, queues, caches, storage, backups, a lab |
| `ams` | `sat-ams-01` | the Amsterdam site: edge servers, storage, two database replicas, core switches |
| `fra` | `sat-fra-01`, `sat-fra-02` (an HA satellite zone) | the Frankfurt site: edge servers, storage, a Kubernetes cluster, core switches |
| `global-templates`, `director-global` | (global zones) | templates, commands, users, notification and dependency rules |

About 280 hosts and 2 900 services. Every check is Icinga's own dummy check under the plugin's name (`disk`, `http`, `check_postgres`, …), so nothing needs to be reachable, but each follows a schedule of its own: most stay OK, about one in eleven has a short problem now and then, a few flap, and some problems stay. A few problems come and go all the time, at a slow, realistic rate. At the start a script acknowledges some problems, comments on others and schedules downtimes (some running, some coming up tonight), as the demo's people (`j.berg`, `dba-oncall`, `a.ivanova`, …); a nightly backup window and an auto-update window for Frankfurt's Kubernetes nodes every four hours come from the configuration. Notifications go to the teams (the commands send nothing). Six heartbeats prove that every zone and every endpoint runs its checks (the user guide's *Heartbeats*): one pinned to each master (every 10 s), one per satellite zone and one pinned to each satellite of `fra` (every 30 s).

Only the masters' API is published, and only on this machine: master-01 at `https://127.0.0.1:5665`, master-02 at `https://127.0.0.1:5666`, and master-01 once more through a small proxy at `https://127.0.0.1:5667` (for the dead-network scenario).

## Start it

You need Docker with Compose v2 (Docker Desktop on macOS and Windows; on Linux the Docker Engine with its `compose` plugin), about 0.5 GB of free memory and 0.7 GB of disk for the image. You need this repository's `demo` and `contract` directories (clone it, or download it as a zip).

```sh
docker compose -f demo/docker-compose.yml up -d --wait
```

The first start downloads the `icinga/icinga2` image, makes the cluster's certificates with Icinga's own CA, generates the configuration and waits until every node is connected and the demo's acknowledgements, comments and downtimes are in place; that takes about a minute and a half once the image is there. `up` returns when the cluster is ready. Later starts reuse the certificates and Icinga's state.

What it costs, measured on a 4-core Linux machine: about 300 MB of memory right after the start (master-01 135 MB, master-02 90 MB, each satellite about 25 MB, the proxy 10 MB), growing to about 500 MB after hours with icygui and the tests connected, and about a tenth of one core while nobody looks (the masters 3 to 5 % each, the satellites about 1 %).

The ports can be changed with `ICYGUI_DEMO_PORT_1`, `ICYGUI_DEMO_PORT_2` and `ICYGUI_DEMO_PORT_PROXY` (then use the same ports in icygui).

## Connect icygui

Until icygui can import environments from a file (planned), there are two ways in.

**With this repository and Rust:** quit icygui, then

```sh
demo/icygui/write-config.sh
```

It writes the environment `demo` (both masters, the cluster's CA certificate) and the demo dashboards into icygui's settings, keeping your other environments, and makes `demo` the active one. Start icygui: it asks for the password once (`icygui-demo-password`, see below) and keeps it in your keychain. `--theme dark` or `--theme light` sets the appearance; `--select overview/databases` opens that dashboard.

**By hand:** in icygui, add an environment (the footer's `+`, or the onboarding form):

| Field | Value |
|---|---|
| name | `demo` |
| URL | `https://127.0.0.1:5665`, then *+ add URL* `https://127.0.0.1:5666` |
| login | password: `icygui-demo` / `icygui-demo-password` |
| TLS | either *trust* each master's certificate when icygui asks, or set the CA file to the cluster's CA: `docker compose -f demo/docker-compose.yml exec -T master-01 cat /var/lib/icinga2/certs/ca.crt > ~/icygui-demo-ca.crt` with the server names `master-01` and `master-02` for the two URLs |

Then import the dashboards: the sidebar's `···` → *import dashboards* (or the palette: *Import dashboards*) and pick `demo/icygui/dashboards.toml`.

What you get: `overview` (problems; *databases* with tiles per database role, failing services, replication slots and the database events; host problems; the orders cluster as a grouped list with host bands; *sites* with tiles per site and the edge hosts as labelled cells), `dba` (the database team's problems, handling and downtimes stacked, and full pages of each), `platform` (*fleet* with every host as a square by host group above the service problems; Kubernetes, network, certificates, queues, storage and backups with the nightly backup window, all services) and `lab`. The sidebar's cluster section has the handling, downtimes, events and the cluster health page.

## The demo's passwords

These are demo values, written here on purpose: the demo listens only on this machine and holds nothing worth protecting. Never use them, or this setup, for anything real.

| API user | Password | For |
|---|---|---|
| `icygui-demo` | `icygui-demo-password` | icygui: exactly the permissions the user guide recommends (*The API user*) |
| `demo-admin` | `demo-admin-password` | the seed and the scenario scripts only; never give it to icygui |
| `icygui`, `viewer` | `icygui-test`, `viewer-test` | the contract tests' fixtures (`contract/icinga`) |
| `healthcheck` | `healthcheck` | each node's own health check (status only, never synced) |

## Break it on purpose

`demo/scenario.sh` breaks the cluster in one of the ways icygui must notice and prints what icygui should show; `demo/scenario.sh recover` mends everything and waits until the cluster is healthy again. It only ever touches this compose project's containers.

| Scenario | What it does | What icygui shows |
|---|---|---|
| `satellite-down` | stops `sat-fra-01` | zone `fra` degraded (yellow); the beat pinned to sat-fra-01 comes back UNKNOWN, *Remote Icinga instance 'sat-fra-01' is not connected to 'sat-fra-02'*, in one line with the endpoint; sat-fra-02 runs the zone's checks |
| `zone-cut-off` | stops `sat-fra-01` and `sat-fra-02` | zone `fra` cut off (red): *zone fra: sat-fra-01 and sat-fra-02 disconnected, heartbeat lost*; its checks go late, nobody takes them over |
| `master-down` | stops `master-02` | master-02 disconnected; its pinned beat UNKNOWN, *Remote Icinga instance 'master-02' is not connected to 'master-01'*; master-01 runs every master check |
| `frozen-master` | freezes `master-01` (`docker pause`): connections stay open and silent | after about 2 minutes: event stream stalled, reconnecting; icygui fails over to master-02, which soon reports master-01 disconnected |
| `checks-stopped [zone]` | turns every active check off (or one zone's: `master`, `ams`, `fra`), as a hung checker would | *Icinga runs no checks* (or *zone fra runs no checks (sat-fra-01 and sat-fra-02 connected)*); checks go late |
| `beat-late [seconds]` | holds zone ams's next heartbeat back (12 s by default) | the heartbeat row shows it late for a few seconds; nothing is raised (with 25 s or more: *zone ams runs no checks* until it comes) |
| `problem-storm [count]` | 40 services (by default) go CRITICAL at once | a few desktop notifications, then one storm summary; they recover with their next check |
| `dead-network` | the proxy at port 5667 passes nothing while connections stay open | for an environment that connects through `https://127.0.0.1:5667` (server name `master-01`): stalled, then a failover to master-02, or *no live data* without a second URL |

Two things real Icinga does that are worth knowing: a check pinned to an endpoint that is gone only turns UNKNOWN once the node running it has been up for 5 minutes (until then it stays silent), and a node that is frozen rather than stopped is noticed by its peers only after about a minute and a half.

## Stop and clean up

```sh
docker compose -f demo/docker-compose.yml stop      # stop; the next up continues where it stopped
docker compose -f demo/docker-compose.yml down -v   # delete everything: containers, certificates, state
```

After `down -v` the next `up` makes a new CA: run `demo/icygui/write-config.sh` again, or trust the new certificates in icygui. Remove the environment `demo` in icygui when you no longer need it.

## Troubleshooting

- **`up --wait` fails or times out:** `docker compose -f demo/docker-compose.yml ps` shows which node isn't healthy; `docker compose -f demo/docker-compose.yml logs master-01` and `exec -T master-01 cat /var/log/icinga2/seed.log` say why. A node is healthy once it is connected to every endpoint it talks to (and master-01 once the seed is done), so a scenario that is still in effect keeps nodes unhealthy: run `demo/scenario.sh recover`.
- **A port is in use:** set `ICYGUI_DEMO_PORT_1` and `ICYGUI_DEMO_PORT_2` (and the URLs in icygui).
- **icygui says the certificate isn't trusted** after a `down -v`: the cluster has a new CA (see above).
