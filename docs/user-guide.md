# icygui user guide

icygui is a desktop client for the Icinga 2 REST API: live problem lists, dashboards, operator actions and native notifications, for macOS and Linux. This guide covers everything in detail; the [README](../README.md) is the overview.

Contents: [Install](#install) · [The API user](#the-api-user) · [First connection](#first-connection) · [TLS](#tls) · [Environments](#environments) · [Dashboards and filters](#dashboards-and-filters) · [Lists and panes](#lists-and-panes) · [Keyboard shortcuts](#keyboard-shortcuts) · [Actions](#actions) · [Notifications](#notifications) · [In the background](#in-the-background) · [Files and data](#files-and-data) · [How icygui talks to Icinga](#how-icygui-talks-to-icinga) · [Troubleshooting](#troubleshooting)

Shortcuts are written for both platforms: <kbd>⌘</kbd> on macOS is <kbd>Ctrl</kbd> on Linux (<kbd>⌘K</kbd> / <kbd>Ctrl K</kbd>).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/alexykn/icygui/main/install.sh | bash
```

The script downloads the latest release for your platform (macOS universal, Linux x86_64 or arm64), checks it against the release's `SHA256SUMS` and installs it:

- **macOS:** `icygui.app` in `/Applications` (or `~/Applications` when `/Applications` isn't writable). It quits a running icygui first, and signs the app with a local identity it creates once per Mac (`icygui local code signing`), so macOS remembers across updates that icygui may read its keychain entries. Nothing beyond what ships with macOS is needed (no Xcode).
- **Linux:** the binary in `~/.local/bin`, the desktop entry and icons in `~/.local/share`. No sudo. It tells you about missing system libraries.

Options (pass them with `… | bash -s -- <options>`): `--version X.Y.Z` installs that release, `--uninstall` removes the app, `--uninstall --purge` also removes the settings, data and logs (passwords stay in the keychain; remove them there). Running the script again updates.

Other ways:
- **Packages** from the [releases page](https://github.com/alexykn/icygui/releases): `.dmg` (macOS), `.deb` and `.tar.gz` (Linux). Check them with `sha256sum -c SHA256SUMS` and `gh attestation verify <file> --repo alexykn/icygui`. A `.dmg` downloaded in a browser is quarantined because builds aren't notarized yet: open the app once via System Settings → Privacy & Security → *Open Anyway*, or run `xattr -dr com.apple.quarantine /Applications/icygui.app`.
- **Homebrew**, once a release has published the tap: `brew install --cask alexykn/tap/icygui` (macOS), `brew install alexykn/tap/icygui` (Linux).
- **From source:** see [development.md](development.md); `cargo xtask install` installs into `~/.local` on Linux.

**Linux desktop requirements.** A Vulkan driver (every desktop Mesa or NVIDIA install has one). For the tray icon, a StatusNotifierItem host: KDE, most other desktops, and GNOME with the *AppIndicator* extension. For desktop notifications, a notification server (every desktop has one). For saved passwords, a Secret Service keyring (GNOME Keyring, KWallet, KeePassXC).

**Try it without an Icinga:** `icygui --demo` runs the whole app against a simulated Icinga in the same process (a 150-host estate with live changes, problem storms and notifications). Actions work against it; nothing is saved and the keychain isn't touched. A *demo* badge sits in every dashboard's header.

Command line: `icygui [--demo] [--background]`, `icygui --version`, `icygui --help`. `--background` starts in the tray without a window (what launch at login runs); the window opens if no tray shows the icon within 20 seconds.

## The API user

icygui logs in as an Icinga `ApiUser`. Give it exactly what it needs and nothing more. This one is ready to paste (into `/etc/icinga2/conf.d/api-users.conf`, a zone's config, or your Ansible role):

```
object ApiUser "icygui" {
  password = "<a long random password>"
  permissions = [
    // read
    "objects/query/Host",
    "objects/query/Service",
    "objects/query/HostGroup",
    "objects/query/ServiceGroup",
    "objects/query/Comment",
    "objects/query/Downtime",
    "objects/query/Notification",
    "objects/query/Dependency",
    "objects/query/Endpoint",
    "objects/query/Zone",
    "status/query",

    // the live event stream
    "events/CheckResult",
    "events/StateChange",
    "events/Flapping",
    "events/AcknowledgementSet",
    "events/AcknowledgementCleared",
    "events/CommentAdded",
    "events/CommentRemoved",
    "events/DowntimeAdded",
    "events/DowntimeRemoved",
    "events/DowntimeStarted",
    "events/DowntimeTriggered",
    "events/ObjectCreated",
    "events/ObjectModified",
    "events/ObjectDeleted",
    "events/Notification",

    // runtime actions (leave these out for a read-only user)
    "actions/reschedule-check",
    "actions/acknowledge-problem",
    "actions/remove-acknowledgement",
    "actions/schedule-downtime",
    "actions/remove-downtime",
    "actions/add-comment",
    "actions/remove-comment",
    "actions/process-check-result",
  ]
}
```

Reload Icinga afterwards (`systemctl reload icinga2`).

- This is everything icygui uses except `actions/execute-command`, which is left out on purpose (*Run command* below): with this list, *test connection* reports all the permissions icygui needs and lists run command's as the opt-in one it lacks.
- icygui never sends `filter` expressions in its requests (it addresses hosts and services by name), so it needs no `filter-expression` permission, which Icinga 2.17 requires by default for requests that carry a filter.
- Permissions restricted with a `filter` in the `ApiUser` work too: icygui then shows and acts on only the objects the user may see.
- **Read-only:** leave out the `actions/*` lines. Action buttons are then disabled, and hovering one says which permission is missing.
- **Run command is opt-in, and it is remote code execution.** `actions/execute-command` runs a check or event command on an endpoint, and icygui's *run command* sends whatever macros you type. With a command such as the ITL's `by_ssh` (its `by_ssh_command` is free text), or any command with a free-form argument, whoever holds this API user's password can run arbitrary commands as the `icinga` user on every agent and satellite the master reaches, and that password sits in the keychain of every on-call laptop. Without the permission, *run command* is disabled and says which permission is missing. If you do need it:
  - give it to a **separate `ApiUser`** that only the people who need it have (in icygui, a second environment with the same URL, switched to only to run a command; mute its notifications from the switcher, or both environments notify about the same problems), and/or
  - **restrict it with a filter** to the objects it may target, for example `{ permission = "actions/execute-command", filter = {{ "lab" in host.groups }} }` (`host` is the host itself or the service's host). A filter limits *which* hosts and services, not *what* runs on them.
- `objects/query/Notification` and `events/Notification` let the panes show whom Icinga notified about a problem, and when. Without them, that row says it can't tell; everything else works.
- `status/query` lets icygui notice an Icinga restart and a stalled event stream. Without it, the periodic reconcile still catches up.
- icygui never needs `objects/modify`, `objects/create`, `objects/delete`, `config/*`, `console` or `actions/restart-process`, and never calls them.

**Client certificates instead of a password:** create the `ApiUser` with `client_cn = "<the certificate's CN>"` instead of `password`, and choose *client certificate* in icygui (below).

## First connection

Start icygui. Without an environment, the window shows the onboarding form:

| Field | |
|---|---|
| name | Shown in the footer and the switcher, e.g. `prod` |
| URL | `https://<master>:5665`. https only. An HA pair or a master with satellites: *+ add URL* for each further node, in order of preference (see [Clusters: which URLs to list](#clusters-which-urls-to-list)). Behind a load balancer, read [TLS](#tls) first: a pinned certificate doesn't work there |
| login | *password*: the API user and its password, or *client certificate*: a PEM certificate and its unencrypted PEM key (*browse* or type the paths; `~` works) |
| author | Recorded on acknowledgements, downtimes and comments; defaults to the API user. Use your own name |
| TLS | Folded away until needed: CA file (for every URL), pinned SHA-256 and server name (per URL), *also trust the system's root certificates*. See [TLS](#tls) |

*test connection* logs in and reads what the user may do: it shows the node that answered (`master-01 · zone master · full view`), the API user, Icinga's version, the permissions, and which of icygui's permissions are missing. With several URLs it tests each in turn (*test all URLs*), and each URL's *test* tests that one: the answer shows under the URL. It changes nothing. *connect* saves the environment (the password goes to the system keychain, never into the settings file) and connects.

What happens on connect: icygui loads hosts in full, services with lean attributes, and problems in detail by name (the problem lists are complete within a few seconds even at 30 000 services; a thin progress bar shows under the header meanwhile), then follows Icinga's event stream. The footer shows `● <environment> <node> <age of the last event> ⌄`: the environment on screen, the node icygui is connected to and how long ago Icinga last sent something.

A new environment starts with one group, *overview*: **problems** (unhandled service problems, worst first), **host problems** and **all services**.

## TLS

Icinga signs its API certificate with its own CA, so the operating system's roots don't trust it. Pick one of these:

1. **The CA file** (recommended). Copy `/var/lib/icinga2/certs/ca.crt` from the master and set it as *CA file*. icygui then verifies the certificate chain and the host name like any TLS client. Certificates renewed by Icinga's CA keep working, and the CA covers every node of the cluster, so one file serves every URL of an environment.
2. **A pinned SHA-256 fingerprint**, per URL. icygui accepts exactly the certificate with that fingerprint at that URL and skips the CA and name checks. Get it on each node with
   ```sh
   openssl x509 -noout -fingerprint -sha256 -in /var/lib/icinga2/certs/$(hostname -f).crt
   ```
   Colons are optional. When the master's certificate is renewed the pin no longer matches, and icygui shows both fingerprints (see below).

   **Not with several masters behind one address.** A pin names exactly one certificate, and each master of an HA zone has its own. Behind a load balancer or round-robin DNS, the first connection that reaches the other master fails as a changed certificate, and icygui stops (it never retries against a certificate it doesn't trust), so notifications stop too. Use the CA file there. Icinga's node certificates name only their own node, so with the CA file either have the masters' certificates include the load balancer's name, or set the environment's URL to one master's own name (*server name* checks one name, not several).
3. **Trust on first use.** Leave both empty and press *test connection*. When the certificate isn't trusted, icygui shows the certificate it was offered: SHA-256 fingerprint, subject, issuer, names and expiry. Compare the fingerprint with the node's (the `openssl` command above) and press *trust this certificate*: icygui pins it for that URL. Never trust a fingerprint you haven't compared. With several URLs, test them all before you rely on them: a URL whose certificate isn't trusted can't take over when the others fail. If that happens, the *Connection lost* banner offers *Review certificate* for it while icygui keeps retrying the others.

More settings:
- **Server name** (per URL): check the certificate against this name instead of the URL's host. Use it when you connect by IP address, through an SSH tunnel (`https://localhost:5665`) or through an alias that isn't in the certificate.
- **Also trust the system's root certificates:** for an API behind a reverse proxy with a public certificate.
- **Client certificates:** a PEM certificate and an unencrypted PEM key, for an `ApiUser` with `client_cn`. The key file should be readable only by you.

**When the certificate changes later:** the connection stops (it doesn't retry against an untrusted certificate) and a banner says *certificate not trusted* with *Review certificate*. A changed pin shows the pinned and the presented fingerprint side by side and *trust the new certificate*. Only trust it if the master's certificate really was renewed. If another master answered (a load balancer in front of an HA zone), trusting its certificate only moves the problem to the first master: use the CA file instead (see pinning above).

## Environments

An environment is one Icinga cluster (one or more API URLs, see below) with its own dashboards, groups and notification rules. Several can be configured; one is active: the one the window shows.

**Every environment stays connected and notifies**, whichever one is on screen, also while the window is closed: each keeps its own event stream, rules, event log and notifications from the moment icygui starts. Switching only changes what the window shows, so it is instant: the other environment's states and counts are current already (its check outputs fill in within a moment, see [quiet mode](#quiet-mode)). Each Icinga sees at most the load of a single client (one event stream, one lean load on connect and the periodic reconcile), and less while its environment is off screen.

- **Switch** from the footer: click `● prod master-01 2s ⌄` to open the connection details (environment, URL, state, the node and how much of the cluster it sees, the URLs it passed over and why, version, last event, API user, missing permissions, *Reload from Icinga*) with the cluster's nodes and the switcher under them. *Nodes* lists every master and satellite, one per line: a green dot while it is connected, red when it isn't, grey when the node icygui talks to can't tell (a node further away); its zone after it; the one icygui is connected to has the selection's background. *Environments* lists every environment with its connection's dot, its node and the age of its last event, the one on screen with the selection's background; click one to switch. In a short window long lists scroll (the label says so). The footer shows the node icygui is connected to (after a failover, the master that took over), with its first name (`icinga-master-02` of `icinga-master-02.example.com`); its tooltip and the details have it in full. The footer's chevron turns blue while another environment has unread notifications (its tooltip says which; the counts are in the notification centre). The palette has *Switch to <name>*, the tray its environments menu. The sidebar always belongs to the active environment.
- **Mute** one environment for 30 minutes, an hour or until 08:00 from the switcher or the palette (*Mute <name> …*). In the switcher the row under the list mutes the environment on screen (*mute <name>*); for another one, point at its row and click the bell that shows left of its gear: the row then says *mute <that name>*. A muted environment has a bell-off in the switcher, and *unmute* ends it. Its notifications are still recorded in the notification centre, marked silent. To silence all of them, pause notifications (see [Notifications](#notifications)).
- **Add** from the switcher (*add environment*) or the palette (*Add environment*).
- **Edit** from the switcher (the gear at the right of its row; it doesn't switch) or the palette (*Edit environment <name>*). Changing the URLs (or their order), the login or TLS reconnects. The stored password stays unless you type a new one.
- **Delete** from the editor (*delete environment*, after a confirmation). It also deletes the environment's password from the keychain and its local event log.

The footer's colour says how the connection is: green while events arrive (or, in quiet mode, while the connection is up: a quiet stream can be silent for minutes); yellow when none arrived for 30 seconds while Icinga reports checks running; red while reconnecting (`retry in 12s`) or stopped (`login refused`, `not trusted`, `no password`, `invalid settings`); grey while connecting or loading.

### Clusters: which URLs to list

An environment lists its cluster's API URLs in order of preference, and icygui finds out on every connect which node answers and how much of the cluster it sees:
- A node of the **top-level zone** (the masters: no parent zone) has every object: the *full view*.
- A node of a **child zone** (a satellite) has only the objects of its zone and the zones below it: a *partial view*. icygui uses one only while no master answers, and says so wherever it matters: the node's name in the footer turns yellow, every dashboard's summary bar says `partial view: zone ams`, and the connection details name the view and the masters it couldn't reach. Meanwhile it asks the preferred URLs again, after 30 seconds and then less and less often (up to every 10 minutes), and switches back as soon as a master answers; the dashboards then fill in again. Nothing is lost meanwhile: what changed outside the satellite's zone (a new problem, a recovery) notifies once a master is back, and what was already wrong doesn't notify again.
- If the API user may not read the status or the zones (`status/query`, `objects/query/Zone`), icygui can't tell and says *view not verified* instead of guessing.

What to list, by layout:

| Your cluster | URLs to list | Notes |
|---|---|---|
| A single master | `https://master:5665` | One URL, as always. |
| Two masters (an HA zone) | `https://master-01:5665`, `https://master-02:5665` | Both see everything. When the first is down icygui connects to the second and stays there when the first comes back (switching back would only cost a new event stream). Use the CA file, or pin each URL. |
| Masters with satellites | the masters first, then a satellite if you want a fallback: `https://master-01:5665`, `https://master-02:5665`, `https://sat-ams-01:5665` | A satellite is only used while no master answers, labelled *partial view*. The `ApiUser` must exist on the satellite too (define it in a global zone, or in the satellite's zone). Don't list satellites first: icygui walks past them to a master anyway. |
| A load balancer or round-robin DNS in front of the masters | `https://icinga-api.example.com:5665` | One URL. Put only the masters (the top-level zone) behind it, and use the CA file with certificates that name the balanced address (a pin names one master's certificate). Listing each master's own URL instead lets icygui fail over by itself. |
| Agents | none | An agent's API sees only itself, and agents usually have no `ApiUser`. |

The footer always shows the node icygui is connected to, and *test connection* shows for each URL which node answered and its view. A node that accepts connections but doesn't answer (an Icinga busy reloading its configuration) is passed over after 8 seconds when another URL is left to try; after a failover icygui stays on the master that took over and asks it first on the next reconnect. All URLs share the login and the CA file; each URL has its own pinned certificate and server name. icygui keeps one event stream, on the node it is connected to; the other URLs cost three small requests each when it tries them (on connect, and while it waits for a master to come back).

## Dashboards and filters

The sidebar holds **groups** (folders) of **dashboards** in the active environment. Each dashboard row shows the worst unhandled state of its objects as a dot and the number of unhandled problems.

- A group's `+` creates a dashboard in it. Its `···` menu: new group, rename, move up or down, collapse or expand, notifications (inherit, on, off, custom rule), export group, delete group (after a confirmation).
- A dashboard's `···` (or a right click): edit dashboard, rename, duplicate, move up or down, move to another group, notifications, delete dashboard.
- The footer's `+`: new dashboard, new group, import dashboards, export all dashboards.
- <kbd>⌘N</kbd> creates a dashboard; <kbd>⌘1</kbd> … <kbd>⌘9</kbd> select the first nine; the sidebar's search filters them by name.
- **Export and import:** *export group* writes a group to a file, *export all dashboards* every group; *import dashboards* adds the groups of such a file (with fresh ids, so importing twice gives two copies). Share dashboards with your team this way. Without a desktop file chooser (xdg-desktop-portal), a small dialog asks for the path.

**The editor** takes the main area: name, group, *services* or *hosts*, the filter, *problems only*, *hide handled problems*, sort (severity, last state change, host, service; descending or ascending), group by (none, host, host group, service group) and the dashboard's notification setting. The list on the left previews the result live; the filter's status says `valid · N matches · M shown`, or where the filter fails. <kbd>⌘S</kbd> saves, <kbd>Esc</kbd> discards (asking first when something changed).

The list header's sort menu and the summary bar's *handled hidden* toggle change the dashboard directly and are saved with it.

### Filters

Filters use **Icinga's own filter language**, the one of the API's `filter` parameter and of `assign where`: if it works in Icinga, it works here. In a service dashboard both `service.*` and `host.*` (the service's host) are available; in a host dashboard, `host.*`. An empty filter matches everything.

| Want | Filter |
|---|---|
| Everything on production hosts | `host.vars.env == "prod"` |
| Postgres services in trouble | `host.vars.role == "postgres" && service.state != 0` |
| Hosts by name pattern | `match("db-prod-*", host.name)` |
| Several patterns | `match("web-*", host.name) \|\| match("lb-*", host.name)` |
| A regular expression | `regex("^k8s-node-[0-9]+$", host.name)` |
| Host group membership | `"linux-servers" in host.groups` |
| Service group membership | `"databases" in service.groups` |
| Hosts in a network | `cidr_match("10.0.2.0/24", host.address)` |
| A check command | `service.check_command == "disk"` |
| Only hard critical | `service.state == 2 && service.state_type == 1` |
| Unhandled problems only | `service.problem && !service.handled` |
| A team's services via a custom var array | `"payments" in service.vars.teams` |
| Text in the output | `service.last_check_result.output.contains("timeout")` |
| Changed in the last hour | `service.last_state_change > get_time() - 1h` |

States are numbers as in Icinga: services 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN; hosts 0 UP, 1 DOWN. `state_type` is 0 soft, 1 hard.

Supported: literals (numbers, durations like `5m` and `1h`, strings, `true`/`false`/`null`, arrays, dictionaries), every operator (`!`, `&&`, `||`, comparisons, `in`, `!in`, arithmetic, bit operators), member access and indexing, and the functions `match`, `regex`, `cidr_match`, `len`, `typeof`, `string`, `number`, `bool`, `intersection`, `union`, `range` and `get_time`, plus the string, array and dictionary methods (`contains`, `find`, `lower`, `upper`, `split`, `starts_with`, `keys`, …). A variable a host doesn't have is `null` rather than an error, so `host.vars.role == "db"` simply doesn't match hosts without a `role`. Statements (assignments, loops, functions) aren't filters and are refused with a message.

Filters run on your machine against the live data, so dashboards cost Icinga nothing. One difference to keep in mind: the check output (`last_check_result`) is only known for objects loaded in detail (problems, opened panes) or after their next check result, so a filter on the output may fill in over the first check interval.

## Lists and panes

Each row: the state circle (filled when unhandled, hollow when acknowledged or in downtime) with the time in state under it, `service on host`, the first line of the output, and a tag at the right (`ack <author>`, `downtime`, `flapping`). Checks that are overdue are marked `late 12m`. The summary bar counts the shown rows per state; *handled hidden* toggles handled problems.

Click a row (or press <kbd>Enter</kbd>) for the **pane** at the right:
- **Service:** state, host link, duration, hard/soft attempt; the action buttons; the plugin output and long output; the performance data with warning and critical thresholds (exceeded values coloured); the check (command, interval, last and next check, endpoint, zone, attempt, latency and runtime, Icinga's switches read-only, flapping, *notified*: whom Icinga notified and when); comments and downtimes (with remove buttons); custom vars; groups; notes and links; the recent history from the local event log.
- **Host:** state, address, uptime, output; actions; tabs *services* (OK services folded into `+ N more ok`), *history*, *vars* and *config* (the check configuration and Icinga's feature switches, read-only); parents and children from dependencies.
- `↗ open as tab` pins the object in the sidebar's *open* section; tabs survive restarts.
- The pane's `···` menu holds the less common actions (submit check result, run command, remove downtimes), copies the name, the output or a filter expression for the object, and watches or mutes it. Notes URLs and action URLs open in the browser.
- **Protected custom variables** show `***` in the vars and in links, never their value: names matching `*pw*`, `*pass*`, `*community*` (Icinga Web's defaults, `community` widened to `snmp_community`), `*secret*`, `*token*`, `*auth_pair*`, `*auth_key*` and `*priv_key*`, ignoring case, at any nesting level. Filters still see the real values.

## Keyboard shortcuts

| Keys | Where | What |
|---|---|---|
| <kbd>j</kbd> / <kbd>↓</kbd>, <kbd>k</kbd> / <kbd>↑</kbd> | list | Next / previous row |
| <kbd>Home</kbd>, <kbd>End</kbd>, <kbd>Page Up</kbd>, <kbd>Page Down</kbd> | list | First, last, a page up or down |
| <kbd>Shift J</kbd> / <kbd>Shift ↓</kbd>, <kbd>Shift K</kbd> / <kbd>Shift ↑</kbd> | list | Extend the marked rows |
| <kbd>x</kbd> | list | Mark or unmark the row (also: Shift-click for a range, ⌘-click / Ctrl-click) |
| <kbd>⌘A</kbd> | list | Mark every row |
| <kbd>Enter</kbd> | list | Open the pane |
| <kbd>⌘Enter</kbd> | list | Open as a tab |
| <kbd>Esc</kbd> | list, pane | Close the pane, clear the marks |
| <kbd>a</kbd> | list, pane | Acknowledge |
| <kbd>d</kbd> | list, pane | Schedule downtime |
| <kbd>r</kbd> | list, pane | Check now |
| <kbd>c</kbd> | list, pane | Add comment |
| <kbd>⌘K</kbd> | anywhere | Command palette |
| <kbd>⌘N</kbd> | anywhere | New dashboard |
| <kbd>⌘1</kbd> … <kbd>⌘9</kbd> | anywhere | Select dashboard 1 to 9 |
| <kbd>⌘B</kbd> | anywhere | Show or hide the sidebar |
| <kbd>Ctrl Tab</kbd>, <kbd>Ctrl Shift Tab</kbd> | anywhere | Next / previous tab |
| <kbd>⌘W</kbd> | anywhere | Close the tab |
| <kbd>⌘,</kbd> | anywhere | Settings |
| <kbd>⌘Q</kbd> | anywhere | Quit (also from the tray) |
| <kbd>⌘S</kbd>, <kbd>Esc</kbd> | dashboard editor | Save, discard |
| <kbd>Tab</kbd>, <kbd>Shift Tab</kbd> | dialogs | Next / previous field |
| <kbd>Enter</kbd> | dialogs | Send (<kbd>Shift Enter</kbd> for a new line in a comment) |
| <kbd>⌘Enter</kbd> | dialogs | Send from any field |
| <kbd>Esc</kbd> | dialogs, palette | Close |
| <kbd>↑</kbd> <kbd>↓</kbd> (or <kbd>Ctrl P</kbd> <kbd>Ctrl N</kbd>), <kbd>Enter</kbd> | palette | Choose, run |
| <kbd>Tab</kbd> | palette | Open the chosen object as a tab |
| <kbd>⌘Enter</kbd> | palette | Run a verb on all matches |

The action keys act on the marked rows if there are any, else on the pane's object, else on the row under the cursor.

**The command palette** (<kbd>⌘K</kbd>) searches hosts, services, dashboards and commands: switching environments, new dashboard and group, import and export, reload from Icinga, pause and resume notifications, the notification centre and settings, and the actions on the current object. Every row starts with a mark: an object's state dot for an action on it (`● Acknowledge · postgres-replication  on db-prod-03 · critical`), a stack in the worst state's colour for an action on several, a dashboard's dot, or a small icon for other commands. The current object's actions (the pane's object, else the marked rows or the row under the cursor) come first, with their keys (`a`, `d`, `r`, `c`). Start a query with a verb to act on what it finds: `ack db-prod-03`, `dt web`, `check mq-prod`, `comment …` (also `acknowledge`, `downtime`, `recheck`, `note`). The current object comes first if it matches, then problems; <kbd>⌘Enter</kbd> (the *all N matches* row) opens the action's dialog listing every match, a check too: nothing is sent before you confirm.

## Actions

All actions are runtime operations through Icinga's `/v1/actions`. icygui never changes configuration or object attributes (no `objects/modify`, no enabling or disabling checks or notifications in Icinga).

| Action | Options |
|---|---|
| **Acknowledge** (<kbd>a</kbd>) | Comment; sticky (stays until OK, through other problem states); persistent (keep the comment after the acknowledgement ends); expiry (1h, 4h, 1d, 08:00 tomorrow, or a time). Icinga is never asked to send notifications for it. *Remove acknowledgement* on acknowledged objects |
| **Schedule downtime** (<kbd>d</kbd>) | Comment; start and end with presets (30m, 1h, 2h, 4h, 8h, 1d, 1w, 08:00 tomorrow); fixed, or flexible with a duration; for hosts: all services too, child hosts (none, triggered, non-triggered); a triggering downtime, picked from the current downtimes of the objects, their hosts and those hosts' parents (or any downtime's name, which the pane's downtimes copy). *Remove downtime* per downtime, or all of the selection's |
| **Check now** (<kbd>r</kbd>) | Forced, at once. More than 20 objects ask first |
| **Add comment** (<kbd>c</kbd>) | Comment and an optional expiry. Remove comments from the pane |
| **Submit check result** | State, output, performance data (passive results) |
| **Run command** | Check or event command, endpoint, macros, TTL; after a confirmation that shows what will run (needs Icinga 2.13+) |

- Mark several rows (<kbd>x</kbd>, Shift-click, <kbd>⌘A</kbd>) to act on all of them at once: one dialog, one result. icygui sends it in as few requests as it can: hosts and services in separate requests, up to 200 objects each, and 20 hosts per request for a downtime that also covers their services or child hosts (Icinga answers only once it has created every one of those downtimes). The requests go one after another, and after one that went unanswered nothing more is sent (see below). A selection bar under the list shows how many are marked and the actions.
- Results show as toasts in the bottom-right corner, with per-object failures. The changed rows update within about a second.
- **When Icinga doesn't answer.** Icinga answers an action only after it has run it for every object, which can take a while on a busy master; icygui waits up to five minutes. Without an answer the toast says *no answer from Icinga: it may have applied this anyway*: look at the object (its pane shows new comments and downtimes as Icinga reports them) before trying again. To keep a retry from adding a second comment or downtime, or running a command twice, icygui holds back the same action on those objects for ten minutes (the toast says why); other actions aren't affected.
- The author recorded with acknowledgements, downtimes and comments is the environment's *author* (default: the API user).
- Buttons for actions the API user may not run are disabled; hovering says which permission is missing.

## Notifications

icygui decides about notifications **on your machine**, from the live event stream, with your rules. It doesn't depend on Icinga's notification users, and nothing you set here changes Icinga.

Notifications come from **every environment** (see [Environments](#environments)): while you look at staging, production still notifies.

**What a notification looks like:** `CRITICAL · postgres-replication on db-prod-03`, the first line of the output, and the group and dashboard; with more than one environment the environment's name comes first (`production · CRITICAL · postgres-replication on db-prod-03`). *Open* (or a click) brings the window back (it is recreated if you closed it), switches to the notification's environment and shows the object's pane. *Acknowledge* opens the acknowledge dialog for the object in its own environment without switching (its title names the environment), and the acknowledgement goes to that environment's Icinga, never to the one on screen.

**Rules** are inherited from top to bottom, and the most specific one wins:
1. **Environment default rule** (Settings → *notifications*).
2. **Group:** inherit, on, off, or a custom rule.
3. **Dashboard:** inherit, on, off, or a custom rule (in its `···` menu, the editor or the settings).
4. **Object:** from the pane's `···` menu: *watch: always notify* (even when none of its dashboards notify), or *mute* for 1 hour, 4 hours, until 08:00, or until unmuted. Muted groups, dashboards and objects show a bell-off in the sidebar or pane.

An object that is in several notifying dashboards notifies **once**.

**A rule's conditions** (defaults in brackets):
- states: critical, warning, unknown, down, unreachable, recovery [critical, unknown, down, recovery];
- hard states only [on]: soft states are retries in progress;
- skip handled problems [on]: acknowledged, in downtime, or the host is down;
- events: acknowledgements, downtimes, flapping [off];
- only after: a problem must last this long first, e.g. `5m` [0, at once]; it's dropped if it recovers or gets handled before;
- play a sound [on].

Recoveries only notify for problems that notified.

**Quiet hours** (Settings → notifications): a daily window (it may cross midnight, e.g. 22:00 to 07:00) on chosen days. During it, notifications are recorded in the centre but not shown, except critical and down ones if *allow critical* is on.

**Storm control:** when more notifications than the threshold arrive within the window [5 within 10 seconds], the rest become one summary (`14 new problems in prod-cluster`). They are all in the centre, marked *silent*.

**Pause:** 30 minutes, 1 hour, or until 08:00, from the notification centre, the palette, the settings or the tray; *resume* ends it. A pause holds for every environment (with several, the chips say *pause all*). One environment can be muted on its own instead (see [Environments](#environments)). While the environment on screen is paused or muted, the footer's clock turns into a bell-off.

**The notification centre** (the clock icon in the footer; its badge counts the unread notifications of the environment on screen) lists the recent notifications, newest first, under *now*, *last hour*, *earlier today*, *yesterday* and *older*:

- **Scope:** with several environments, the chips at the bottom pick whose: *all*, the environment on screen (where it opens, filled) or another one. A chip turns blue while its environment has unread notifications (*all*: another environment has); the heading counts the unread ones of the chip chosen. Environments that don't fit go behind `···`. The gear at the bottom right opens the notification settings.
- **Entries:** time, title, the output's first line and where it matched (`overview`, `databases / production`; in *all* with the environment in front: `staging · overview`). Unread ones have a bright title. Click one to open its object (switching to its environment first) and mark it read; opening an object anywhere marks its notifications read too. Click a label to show only that place's notifications, again to show all.
- **Silent entries** (no desktop notification) have a hollow dot, a dimmer title and the reason: `silent · paused`, `silent · quiet hours`, `silent · storm`.
- **Storms:** what a storm held back collapses into its summary (`24 new problems in prod-cluster · 19 held back`; while the storm goes on, `19 notifications held back by a storm`); click it to list them, again to fold them.
- **Pause row:** *pause all* with 30 minutes, 1 hour and until 08:00 for every environment, or until when they are paused (*resume*); in the scope of an environment muted on its own, until when it is muted (*unmute*).
- The centre keeps its size and place while it is open, so nothing moves under the pointer when notifications arrive or you pick a scope or a label; in a short window its list is shorter.
- *mark all read* marks what the list shows: the scope's environments, or only what a label left.

The history stays on this computer, for as long as Settings → general → *keep events for* says.

No notifications are sent for what's already wrong when icygui connects, but icygui keeps track of it: a service still critical from before its host went down waits for a fresh check (or five minutes) once the host is back, and an object that is flapping stays quiet until it stops. A problem that notified before icygui restarted (or before the environment's connection settings changed) still notifies its recovery, as long as the event log keeps it. Problems that a reconcile finds after a reconnect do notify, also one that recovered and failed again while your laptop slept.

Platform notes: on macOS, notifications come from the app bundle (allow them in System Settings → Notifications the first time, with sounds); a rule's sound is the system's alert sound, also while icygui is in front. On Linux they go to your desktop's notification server, with a sound by state (critical, warning, recovery) where the server plays sounds.

## In the background

- **Closing the window** keeps icygui running in the tray (Linux) or the menu bar (macOS), still connected to every environment and notifying for each (Settings → general → *keep running in the tray when the window closes*, on by default). Where no tray icon can be shown (stock GNOME without the AppIndicator extension), closing the window quits, and the settings say so. With unsaved work in the window (dashboard editor changes, text typed in an action dialog) it asks first.
- **The tray icon** is the logo mark tinted with the worst unhandled state across all environments; its tooltip has a line per environment with its connection (`partial view: zone ams` while on a satellite, *muted* when muted on its own) and its counts; its menu has *Open*, *Pause notifications* (every environment), the environments and *Quit*. Choosing another environment there switches like the footer does.
- **Launch at login** (Settings → general) starts icygui in the background, without a window, with every environment connected in [quiet mode](#quiet-mode) (a launch agent on macOS, an XDG autostart entry on Linux). If no tray shows its icon within 20 seconds (a panel that starts after icygui gets that long), the window opens instead.
- **One instance:** starting icygui again brings the running one's window forward.
- **Quit** from the tray, the app menu or <kbd>⌘Q</kbd>.

### Quiet mode

While nobody looks, icygui follows Icinga more quietly (Settings → general → *quiet mode when hidden*, on by default): every environment that isn't on screen, and the one on screen once the window has been hidden for half a minute: closed to the tray, or not shown at all as your system reports it (minimised, on another virtual desktop or Space, the display asleep; on macOS also a window other windows cover completely). A window that is merely unfocused or partly covered, such as a dashboard on a second screen, stays fully live, and a quick look at another window changes nothing. The environment switcher shows `quiet` instead of the age of the last event for an environment in quiet mode, with a green dot: its stream can be silent for minutes without anything being wrong.

- **Notifications are never delayed:** state changes, acknowledgements, downtimes, comments, flapping and Icinga's own notifications still arrive the moment they happen; icygui only leaves out the check results that change nothing (about 99 % of the event stream, one per check).
- **Stale while quiet:** check outputs (the text of a check that didn't change the state), last-check times and the *late* markers. States, counts, the tray icon and the notification centre stay current. After an acknowledgement or downtime ends, the "is it still a problem?" confirmation waits a few minutes instead of for the next check.
- **Less work for the master and your laptop:** the status is read every 5 minutes instead of every 30 seconds, the lean reload runs every 30 minutes or more, and nothing is fetched for rows nobody sees.
- **Waking up is instant:** when the window opens or you switch environments, what changed state is already there with its output; the full event stream comes back within a moment without missing anything, the object you open is fetched first (a notification you click was fetched when it was shown), then the rows on screen, then the other problems' outputs. A pane never waits: it shows what icygui has and, if fresher details take longer than about 300 ms, a small *updating* beside `service` or `host` in its header, which goes away when they are in. Moving through the list with `j` and `k`, icygui asks only for the row you stop on.
- **Started at login** (in the background, with the tray), every environment starts quiet and loads after a short random wait that grows with the size of its Icinga; opening the window ends the wait.
- Turn it off to keep every environment fully live all the time (each then costs its Icinga a full event stream).

## Files and data

| | Linux | macOS |
|---|---|---|
| Settings | `~/.config/icygui/config.toml` (and `config.toml.bak`) | `~/Library/Application Support/io.github.alexykn.icygui/config.toml` |
| Window, open tabs, selected dashboards | `~/.local/share/icygui/state.toml` | `~/Library/Application Support/io.github.alexykn.icygui/state.toml` |
| Local event log (one per environment) | `~/.local/share/icygui/events-<id>.sqlite3` | `~/Library/Application Support/io.github.alexykn.icygui/events-<id>.sqlite3` |
| Log | `~/.local/state/icygui/logs/icygui.log` | `~/Library/Logs/io.github.alexykn.icygui/icygui.log` |
| Passwords | Secret Service keyring | Keychain |

The Linux paths follow `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_STATE_HOME`.

- The settings file is written atomically, readable only by you (0600), with the previous version kept as `config.toml.bak`. It never contains passwords.
- The event log keeps state changes, acknowledgements, comments, downtimes, flapping and notifications for 48 hours (Settings → general → *keep events for*). It feeds the notification centre and the history tabs, so history starts when icygui first connected.
- The log rotates at 10 MiB and keeps four old files. Passwords and authentication headers are never logged. More detail: `RUST_LOG=icygui=debug,ic_core=debug icygui`.
- *Reconcile with Icinga* (Settings → general): *adaptive* reloads the lean object list at an interval that grows with the installation (every 5 minutes up to about 10 000 hosts and services, about every 15 minutes at 30 000, at most hourly), and less often while the event stream has been running without a break and the reloads found nothing it missed (up to an hour); *fixed interval* lets you choose (at least a minute, and at least 5 minutes from 5 000 objects on). Quiet mode reconciles every 30 minutes at most.

## How icygui talks to Icinga

It's built to be gentle on the master, also when the whole on-call team runs it:
- **On connect:** a login, the node's name and the zone tree at each URL it tries (in order of preference, until a node that sees the whole cluster answers; see [Clusters](#clusters-which-urls-to-list)), then one lean load: hosts, services without their check results, comments, downtimes, groups, dependencies and endpoints, then problems in detail by name, and Icinga's `Notification` objects in the background. At 2 000 hosts and 30 000 services that is about 35 MB, comparable to opening a large page in Icinga Web once.
- **Starting at login** (in the background), each environment first reads Icinga's object counts, then waits a random moment before that load: up to 3 seconds per 1 000 services (at most 90 seconds; under a second for a small Icinga), so a team whose laptops start at nine doesn't load at the same second. Opening the window ends the wait; a start you open yourself never waits.
- **Then one event stream** (`/v1/events`, about 75 KB/s at that size; under 1 KB/s in [quiet mode](#quiet-mode)). Changes are applied from the events themselves; nothing is re-queried per event. Objects are queried by name only when the events can't tell (a configuration change, an unknown object, a pane you open, rows that come into view, a notification shown). These by-name queries share a budget of 5 a second (bursts of 10), which a small Icinga never reaches; the object you open always goes first.
- **Reconcile:** a lean reload every 5 to 60 minutes depending on the installation's size and how reliably the stream runs (see *Reconcile with Icinga* above), after an Icinga restart (once, also when both masters of an HA zone restart), and after a reconnect that followed a gap of two minutes or more (a laptop that slept; with jitter, so a team doesn't reload at once). After a shorter gap the events since catch up: a proxy that ends the stream every few minutes doesn't cause a reload each time. Never a periodic full reload.
- **Reload from Icinga** (and *Retry now* while connected) reloads at once; pressed again it waits 30 seconds, then 2 minutes, then 5 minutes between reloads, until you leave it for 10 minutes.
- **Actions** show their effect through Icinga's events (a forced check's result, the acknowledgement, the downtime); the objects aren't queried again.
- **Status:** `/v1/status` every 30 seconds, every 5 minutes in quiet mode (restart and stall detection).
- **Reconnects** back off from 1 second to 60 seconds. If the first load fails (a busy master, a proxy answering 504), it is retried after 30 seconds, then a minute, two, and so on up to 15 minutes. An answer icygui can't read stops it with *invalid settings* until you press *Retry now*.

Details and measurements: [performance.md](performance.md).

## Troubleshooting

**The certificate isn't trusted** (`not trusted` in the footer). Set Icinga's CA file, or compare and trust the fingerprint (see [TLS](#tls)). Connecting by IP or through a tunnel: set *server name* to the name in the certificate. When a pin is set and *Review certificate* shows two fingerprints, the master presented another certificate than the pinned one: compare the new fingerprint with the master's before trusting it.

**Login refused** (`login refused`). Wrong API user or password, or the `ApiUser` isn't loaded (check `icinga2 object list --type ApiUser` on the master and reload Icinga). Fix it with *Edit environment* in the banner. icygui stops retrying until you change something or press *Retry now*.

**No password** (`no password`). The keychain has no password for this environment (a new machine, or the keychain entry was removed): edit the environment and type it again. On Linux without a Secret Service keyring, passwords can't be saved; use a client certificate or install a keyring (GNOME Keyring, KWallet). On macOS, if the keychain asks for permission after an update, choose *Always Allow*; `install.sh`'s local signing identity keeps that answer across updates.

**Missing permissions.** *test connection* and the connection details list them. Without a query permission a dashboard shows *No permission*; without an action permission its button is disabled with the reason. Add the lines from [the API user](#the-api-user), reload Icinga, then restart icygui: it reads the user's permissions when it connects.

**Reconnecting.** A banner shows the countdown and *Retry now*; icygui backs off up to a minute between attempts (trying every URL each time) and catches up with a reconcile once connected. With several URLs the banner names each URL's problem. Persistent: check the URL and port (5665), firewalls and VPN, and that the Icinga API feature is enabled (`icinga2 feature list`). The log says why each attempt failed.

**Partial view** (the node's name in the footer is yellow, dashboards say `partial view: zone …`). No master answered, so icygui connected to a satellite, which only has its zone's objects. The connection details say why each master was passed over. icygui switches back by itself once a master answers; *Reload from Icinga* in the connection details asks the masters at once.

**View not verified.** The API user may not read the status or the zones (`status/query`, `objects/query/Zone`; both are in the [API user](#the-api-user) snippet), so icygui can't tell whether the node sees the whole cluster.

**The footer turns yellow.** No event arrived for 30 seconds although Icinga reports checks running. A proxy or load balancer may be holding the stream; after two minutes without a line icygui reconnects by itself.

**Rows marked late.** Icinga hasn't reported a check result when it should have. icygui re-asks Icinga for that object; still late means the check really isn't running (a stopped satellite or agent, disabled active checks).

**A broken settings file.** icygui never overwrites a settings file it can't read. It shows the error with line and column and offers to restore the backup (`config.toml.bak`), start fresh (the broken file is kept as `config.toml.unreadable-<time>`), try again after you fixed it by hand, or quit.

**Something else.** The log file (see [files](#files-and-data)) has the details; run with `RUST_LOG=icygui=debug,ic_core=debug` for more. When reporting a problem, include the log, `icygui --version`, your platform, and Icinga's version (shown by *test connection*). The log contains host and service names but never passwords.
