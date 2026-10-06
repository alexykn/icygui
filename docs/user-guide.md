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

**Try it without an Icinga:** `icygui --demo` runs the whole app against a simulated Icinga in the same process (a 150-host estate with live changes, problem storms and notifications). Actions work against it; nothing is saved and the keychain isn't touched. The footer shows a *demo* badge.

Command line: `icygui [--demo] [--background]`, `icygui --version`, `icygui --help`. `--background` starts in the tray without a window (what launch at login runs).

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
    "objects/query/User",
    "objects/query/UserGroup",
    "objects/query/Notification",
    "objects/query/Dependency",
    "objects/query/Endpoint",
    "objects/query/Zone",
    "objects/query/CheckCommand",
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
    "actions/execute-command",
  ]
}
```

Reload Icinga afterwards (`systemctl reload icinga2`).

- This is the complete list icygui checks for: with it, *test connection* reports nothing missing.
- None of the permissions uses a `filter` (Icinga's `filter-expression`). icygui doesn't need them, so it works with Icinga 2.17's default, where filtered permissions are restricted.
- **Read-only:** leave out the `actions/*` lines. Action buttons are then disabled, and hovering one says which permission is missing.
- **No remote commands:** leave out `actions/execute-command` only; *run command* is then disabled.
- `objects/query/Notification` and `events/Notification` let the panes show whom Icinga notified about a problem, and when. Without them, that row says it can't tell; everything else works.
- `status/query` lets icygui notice an Icinga restart and a stalled event stream. Without it, the periodic reconcile still catches up.
- icygui never needs `objects/modify`, `objects/create`, `objects/delete`, `config/*`, `console` or `actions/restart-process`, and never calls them.

**Client certificates instead of a password:** create the `ApiUser` with `client_cn = "<the certificate's CN>"` instead of `password`, and choose *client certificate* in icygui (below).

## First connection

Start icygui. Without an environment, the window shows the onboarding form:

| Field | |
|---|---|
| name | Shown in the footer and the switcher, e.g. `prod` |
| URL | `https://<master or load balancer>:5665`. https only |
| login | *password*: the API user and its password, or *client certificate*: a PEM certificate and its unencrypted PEM key (*browse…* or type the paths; `~` works) |
| author | Recorded on acknowledgements, downtimes and comments; defaults to the API user. Use your own name |
| TLS | Folded away until needed: CA file, pinned SHA-256, server name, *also trust the system's root certificates*. See [TLS](#tls) |

*test connection* logs in and reads what the user may do: it shows the API user, Icinga's version, the permissions, and which of icygui's permissions are missing. It changes nothing. *connect* saves the environment (the password goes to the system keychain, never into the settings file) and connects.

What happens on connect: icygui loads hosts in full, services with lean attributes, and problems in detail by name (the problem lists are complete within a few seconds even at 30 000 services; a thin progress bar shows under the header meanwhile), then follows Icinga's event stream. The footer shows `● <endpoint> · <age of the last event>`.

A new environment starts with one group, *overview*: **problems** (unhandled service problems, worst first), **host problems** and **all services**.

## TLS

Icinga signs its API certificate with its own CA, so the operating system's roots don't trust it. Pick one of these:

1. **The CA file** (recommended). Copy `/var/lib/icinga2/certs/ca.crt` from the master and set it as *CA file*. icygui then verifies the certificate chain and the host name like any TLS client. Certificates renewed by Icinga's CA keep working.
2. **A pinned SHA-256 fingerprint.** icygui accepts exactly the certificate with that fingerprint and skips the CA and name checks. Get it on the master with
   ```sh
   openssl x509 -noout -fingerprint -sha256 -in /var/lib/icinga2/certs/$(hostname -f).crt
   ```
   Colons are optional. When the master's certificate is renewed the pin no longer matches, and icygui shows both fingerprints (see below).
3. **Trust on first use.** Leave both empty and press *test connection*. When the certificate isn't trusted, icygui shows the certificate it was offered: SHA-256 fingerprint, subject, issuer, names and expiry. Compare the fingerprint with the master's (the `openssl` command above) and press *trust this certificate*: icygui pins it. Never trust a fingerprint you haven't compared.

More settings:
- **Server name:** check the certificate against this name instead of the URL's host. Use it when you connect by IP address, through an SSH tunnel (`https://localhost:5665`) or through an alias that isn't in the certificate.
- **Also trust the system's root certificates:** for an API behind a reverse proxy with a public certificate.
- **Client certificates:** a PEM certificate and an unencrypted PEM key, for an `ApiUser` with `client_cn`. The key file should be readable only by you.

**When the certificate changes later:** the connection stops (it doesn't retry against an untrusted certificate) and a banner says *certificate not trusted* with *Review certificate…*. A changed pin shows the pinned and the presented fingerprint side by side and *trust the new certificate*. Only trust it if the master's certificate really was renewed.

## Environments

An environment is one Icinga API endpoint with its own dashboards, groups and notification rules. Several can be configured; one is active.

- **Switch** from the footer: click `● master-01 · 2s` to open the connection details (environment, URL, state, endpoint, version, last event, API user, missing permissions, *Reload from Icinga*) with the switcher under them. The palette has *Switch to <name>*. The sidebar always belongs to the active environment.
- **Add** from the switcher (*add environment…*) or the palette (*Add environment…*).
- **Edit** from the switcher (*edit <name>…*) or the palette. Changing the URL, the login or TLS reconnects. The stored password stays unless you type a new one.
- **Delete** from the editor (*delete environment…*, after a confirmation). It also deletes the environment's password from the keychain and its local event log.

The footer's colour says how the connection is: green while events arrive; yellow when none arrived for 30 seconds while Icinga reports checks running; red while reconnecting (`retry in 12s`) or stopped (`login refused`, `not trusted`, `no password`, `invalid settings`); grey while connecting or loading.

## Dashboards and filters

The sidebar holds **groups** (folders) of **dashboards** in the active environment. Each dashboard row shows the worst unhandled state of its objects as a dot and the number of unhandled problems.

- A group's `+` creates a dashboard in it. Its `···` menu: new group, rename, move up or down, collapse or expand, notifications (inherit, on, off, custom rule…), export group…, delete group… (after a confirmation).
- A dashboard's `···` (or a right click): edit dashboard…, rename, duplicate, move up or down, move to another group, notifications, delete dashboard….
- The footer's `+`: new dashboard, new group, import dashboards…, export all dashboards….
- <kbd>⌘N</kbd> creates a dashboard; <kbd>⌘1</kbd> … <kbd>⌘9</kbd> select the first nine; the sidebar's search filters them by name.
- **Export and import:** *export group…* writes a group to a file, *export all dashboards…* every group; *import dashboards…* adds the groups of such a file (with fresh ids, so importing twice gives two copies). Share dashboards with your team this way. Without a desktop file chooser (xdg-desktop-portal), a small dialog asks for the path.

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

**The command palette** (<kbd>⌘K</kbd>) searches hosts, services, dashboards and commands: switching environments, new dashboard and group, import and export, reload from Icinga, pause and resume notifications, the notification centre and settings, and the actions on the current object. Start a query with a verb to act on what it finds: `ack db-prod-03`, `dt web`, `check mq-prod`, `comment …` (also `acknowledge`, `downtime`, `recheck`, `note`). Problems come first; <kbd>⌘Enter</kbd> runs the verb on all matches.

## Actions

All actions are runtime operations through Icinga's `/v1/actions`. icygui never changes configuration or object attributes (no `objects/modify`, no enabling or disabling checks or notifications in Icinga).

| Action | Options |
|---|---|
| **Acknowledge** (<kbd>a</kbd>) | Comment; sticky (stays until OK, through other problem states); persistent (keep the comment after the acknowledgement ends); expiry (1h, 4h, 1d, 08:00 tomorrow, or a time). Icinga is never asked to send notifications for it. *Remove acknowledgement* on acknowledged objects |
| **Schedule downtime** (<kbd>d</kbd>) | Comment; start and end with presets (30m, 1h, 2h, 4h, 8h, 1d, 1w, 08:00 tomorrow); fixed, or flexible with a duration; for hosts: all services too, child hosts (none, triggered, non-triggered); a triggering downtime. *Remove downtime* per downtime, or all of the selection's |
| **Check now** (<kbd>r</kbd>) | Forced, at once. More than 20 objects ask first |
| **Add comment** (<kbd>c</kbd>) | Comment and an optional expiry. Remove comments from the pane |
| **Submit check result** | State, output, performance data (passive results) |
| **Run command** | Check or event command, endpoint, macros, TTL; after a confirmation that shows what will run (needs Icinga 2.13+) |

- Mark several rows (<kbd>x</kbd>, Shift-click, <kbd>⌘A</kbd>) to act on all of them at once: one request to Icinga. A selection bar under the list shows how many are marked and the actions.
- Results show as toasts in the bottom-right corner, with per-object failures. The changed rows update within about a second.
- The author recorded with acknowledgements, downtimes and comments is the environment's *author* (default: the API user).
- Buttons for actions the API user may not run are disabled; hovering says which permission is missing.

## Notifications

icygui decides about notifications **on your machine**, from the live event stream, with your rules. It doesn't depend on Icinga's notification users, and nothing you set here changes Icinga.

**What a notification looks like:** `CRITICAL · postgres-replication on db-prod-03`, the first line of the output, and the group and dashboard. *Acknowledge* opens the acknowledge dialog; *Open* (or a click) brings the window back (it is recreated if you closed it) with the object's pane.

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

**Pause:** 30 minutes, 1 hour, or until 08:00, from the notification centre, the palette, the settings or the tray; *resume* ends it. While paused, the footer's clock turns into a bell-off.

**The notification centre** (the clock icon in the footer) lists recent notifications, silent ones included, with an unread badge; *mark all read*; click one to open its object.

No notifications are sent for what's already wrong when icygui connects. Problems that a reconcile finds after a reconnect do notify.

Platform notes: on macOS, notifications come from the app bundle (allow them in System Settings → Notifications the first time). On Linux they go to your desktop's notification server.

## In the background

- **Closing the window** keeps icygui running in the tray (Linux) or the menu bar (macOS), still connected and notifying (Settings → general → *keep running in the tray when the window closes*, on by default). Where no tray icon can be shown (stock GNOME without the AppIndicator extension), closing the window quits, and the settings say so.
- **The tray icon** is the logo mark tinted with the worst unhandled state of the active environment; its tooltip has the counts; its menu has *Open*, *Pause notifications*, the environments and *Quit*.
- **Launch at login** (Settings → general) starts icygui in the background, without a window (a launch agent on macOS, an XDG autostart entry on Linux).
- **One instance:** starting icygui again brings the running one's window forward.
- **Quit** from the tray, the app menu or <kbd>⌘Q</kbd>.

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
- *Reconcile with Icinga* (Settings → general): *adaptive* reloads the lean object list every 5 minutes below 5 000 objects and every 15 above; *fixed interval* lets you choose.

## How icygui talks to Icinga

It's built to be gentle on the master, also when the whole on-call team runs it:
- **On connect:** one lean load: hosts, services without their check results, comments, downtimes, groups, dependencies and endpoints, then problems in detail by name, and Icinga's `Notification` objects in the background. At 2 000 hosts and 30 000 services that is about 35 MB, comparable to opening a large page in Icinga Web once.
- **Then one event stream** (`/v1/events`, about 75 KB/s at that size). Changes are applied from the events themselves; nothing is re-queried per event. Objects are queried by name only when the events can't tell (a configuration change, an unknown object, a pane you open, rows that come into view).
- **Reconcile:** a lean reload every 5 or 15 minutes, after a reconnect (with jitter, so a team doesn't reload at once) and after an Icinga restart. Never a periodic full reload.
- **Status:** `/v1/status` every 30 seconds (restart and stall detection).
- **Reconnects** back off from 1 second to 60 seconds.

Details and measurements: [performance.md](performance.md).

## Troubleshooting

**The certificate isn't trusted** (`not trusted` in the footer). Set Icinga's CA file, or compare and trust the fingerprint (see [TLS](#tls)). Connecting by IP or through a tunnel: set *server name* to the name in the certificate. When a pin is set and *Review certificate…* shows two fingerprints, the master presented another certificate than the pinned one: compare the new fingerprint with the master's before trusting it.

**Login refused** (`login refused`). Wrong API user or password, or the `ApiUser` isn't loaded (check `icinga2 object list --type ApiUser` on the master and reload Icinga). Fix it with *Edit environment…* in the banner. icygui stops retrying until you change something or press *Retry now*.

**No password** (`no password`). The keychain has no password for this environment (a new machine, or the keychain entry was removed): edit the environment and type it again. On Linux without a Secret Service keyring, passwords can't be saved; use a client certificate or install a keyring (GNOME Keyring, KWallet). On macOS, if the keychain asks for permission after an update, choose *Always Allow*; `install.sh`'s local signing identity keeps that answer across updates.

**Missing permissions.** *test connection* and the connection details list them. Without a query permission a dashboard shows *No permission*; without an action permission its button is disabled with the reason. Add the lines from [the API user](#the-api-user), reload Icinga, then restart icygui: it reads the user's permissions when it connects.

**Reconnecting.** A banner shows the countdown and *Retry now*; icygui backs off up to a minute between attempts and catches up with a reconcile once connected. Persistent: check the URL and port (5665), firewalls and VPN, and that the Icinga API feature is enabled (`icinga2 feature list`). The log says why each attempt failed.

**The footer turns yellow.** No event arrived for 30 seconds although Icinga reports checks running. A proxy or load balancer may be holding the stream; after two minutes without a line icygui reconnects by itself.

**Rows marked late.** Icinga hasn't reported a check result when it should have. icygui re-asks Icinga for that object; still late means the check really isn't running (a stopped satellite or agent, disabled active checks).

**A broken settings file.** icygui never overwrites a settings file it can't read. It shows the error with line and column and offers to restore the backup (`config.toml.bak`), start fresh (the broken file is kept as `config.toml.unreadable-<time>`), try again after you fixed it by hand, or quit.

**Something else.** The log file (see [files](#files-and-data)) has the details; run with `RUST_LOG=icygui=debug,ic_core=debug` for more. When reporting a problem, include the log, `icygui --version`, your platform, and Icinga's version (shown by *test connection*). The log contains host and service names but never passwords.
