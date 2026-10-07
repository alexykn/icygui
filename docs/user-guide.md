# icygui user guide

icygui is a desktop client for the Icinga 2 REST API: live problem lists, dashboards, operator actions and native notifications, for macOS and Linux. This guide covers everything in detail; the [README](../README.md) is the overview.

Contents: [Install](#install) · [The API user](#the-api-user) · [First connection](#first-connection) · [TLS](#tls) · [Environments](#environments) · [Dashboards and filters](#dashboards-and-filters) · [Lists and panes](#lists-and-panes) · [Keyboard shortcuts](#keyboard-shortcuts) · [Actions](#actions) · [Notifications](#notifications) · [In the background](#in-the-background) · [Files and data](#files-and-data) · [How icygui talks to Icinga](#how-icygui-talks-to-icinga) · [Troubleshooting](#troubleshooting)

Shortcuts are written for both platforms: <kbd>⌘</kbd> on macOS is <kbd>Ctrl</kbd> on Linux (<kbd>⌘K</kbd> / <kbd>Ctrl K</kbd>).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/alexykn/icygui/main/install.sh | bash
```

The script downloads the latest release for your platform (macOS universal, Linux x86_64 or arm64), checks it against the release's `SHA256SUMS` and installs it. While there is no full release yet, it takes the newest pre-release (a release candidate such as `0.1.0-rc.1`) and says so:

- **macOS:** `icygui.app` in `/Applications` (or `~/Applications` when `/Applications` isn't writable). It quits a running icygui first, and signs the app with a local identity it creates once per Mac (`icygui local code signing`), so macOS remembers across updates that icygui may read its keychain entries. Nothing beyond what ships with macOS is needed (no Xcode).
- **Linux:** the binary in `~/.local/bin`, the desktop entry and icons in `~/.local/share`. No sudo. It tells you about missing system libraries. It stops a running icygui first (otherwise starting icygui would only bring the old one forward); start it again afterwards.

Options (pass them with `… | bash -s -- <options>`): `--version X.Y.Z` installs that release (`--version 0.1.0-rc.1` a release candidate), `--uninstall` removes the app, `--uninstall --purge` also removes the settings, data and logs (passwords stay in the keychain; remove them there). Running the script again updates; `icygui --version` then names the new version.

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
  - give it to a **separate `ApiUser`** that only the people who need it have (in icygui, a second environment with the same URL, switched to only to run a command). Turn that environment's notifications off (Settings → notifications, pick it under *environment*, then *notifications for <name>*), or both environments notify about the same problems; a mute from the switcher ends by 08:00 at the latest. Like every environment it stays connected: a second event stream (a quiet one while it is off screen), its own load on start and its reconciles, so it costs the master as much as a second laptop. And/or
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
- **Edit** from the switcher (the gear at the right of its row; it doesn't switch) or the palette (*Edit environment <name>*). Changing the URLs (or their order), the login or TLS reconnects. The stored password stays unless you type a new one. Leaving the editor with something changed (<kbd>Esc</kbd>, *cancel*, a click beside it) asks first.
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

The sidebar holds **groups** (folders) of **dashboards** in the active environment. Each dashboard row has a **mark** and a count. A dashboard with a problem view (a list, a grouped list, a grid or tiles) shows the worst unhandled state of its objects as a dot and the number of unhandled problems; one with only handling, downtimes or event stream views shows its first view's icon and that view's count (objects being handled, downtimes in effect, nothing for events). Only problem views count toward the number and notify. The **sidebar mark** in the editor replaces the dot with an icon of your choice (the count stays).

Above the groups, the fixed **cluster** section holds the whole environment's **handling**, **downtimes** and **events** (the same views as on a dashboard, without a filter) and **health** with the cluster's state dot. Objects opened as tabs are listed under the groups, in the *open* section, which shows only while there are any.

- A group's `+` creates a dashboard in it. Its `···` menu: new group, rename, move up or down, collapse or expand, notifications (inherit, on, off, custom rule), export group, delete group (after a confirmation).
- A dashboard's `···` (or a right click): edit dashboard, rename, duplicate, move up or down, move to another group, notifications, delete dashboard.
- The footer's `+`: new dashboard, new group, import dashboards, export all dashboards.
- <kbd>⌘N</kbd> creates a dashboard; <kbd>⌘1</kbd> … <kbd>⌘9</kbd> select the first nine; the sidebar's search filters them by name.
- **Export and import:** *export group* writes a group to a file, *export all dashboards* every group; *import dashboards* adds the groups of such a file (with fresh ids, so importing twice gives two copies). Share dashboards with your team this way. Without a desktop file chooser (xdg-desktop-portal), a small dialog asks for the path.

**The editor** takes the main area: the live preview of the whole dashboard on the left, as the dashboard will show it (a one-view dashboard's controls are in the editor's header, as they will be in the page header), and the inspector on the right. The inspector holds the dashboard's name; its **sidebar mark** (a square previewing it, and *state*, the worst problem's dot, or *icon*; *state* is greyed out, its reason in the tooltip, on a dashboard without a problem view) beside its **group**; then its **views**, one field-like row each, with a drag handle, the display's icon, the name, what the view shows (`6 matches`, `124 hosts`, `4 tiles`, `8 today`, `4 handled`, `1 in effect`; `invalid` when its filter doesn't work) and `···` (*move up*, *move down*, *duplicate*, *collapse by default*, *remove view*); and the notification setting. With *icon*, a click on the square opens the **icon picker**: a search (`ser` finds `server`), the icons you picked recently and the rest, eight a row; <kbd>Enter</kbd> takes the first one found. *add view* lists every kind in sections, *lists* (list, grouped list), *overviews* (host-group grid, summary tiles) and *activity* (event stream, handling, downtimes); the new view starts from the selected view's filter and goes under it. **A new dashboard** (a group's `+`, the footer's `+`, <kbd>⌘N</kbd>, the palette) starts with one empty list view named *list*: an empty filter is every object. Pick its kind in *display* or add more views. Drag a row by its handle to move the view, or select it and press <kbd>⌥↑</kbd> / <kbd>⌥↓</kbd> (<kbd>Alt ↑</kbd> / <kbd>Alt ↓</kbd> on Linux); <kbd>↑</kbd> / <kbd>↓</kbd> select the view above or below, <kbd>⌘⌫</kbd> removes the selected one. Removing asks nothing: *discard* brings it back. A dashboard keeps at least one view and has at most 16; a new dashboard starts with one list view.

Under the views come the **selected view's settings**: its name (*view 2 of 3*), then what depends on its display:

- **List, grouped list:** display and *services* or *hosts*, a grouped list's *group by* (services: host, host group, service group; hosts: host group, since a host would be its own band's only row), the filter, *problems only*, **handled** (*as in settings*: the kinds Settings → appearance → *handled problems* hides, shown dimmed; *show*: every handled problem shows, hollow; *hide*: the kinds you pick, acknowledged, in downtime, host down), sort and direction, and **rows**.
- **Host-group grid:** group by host group (every one, or the ones you pick as chips) or a host custom variable (`site` or `host.vars.site`), the filter (empty: every host of those groups), colour by *worst of host and services* or *host only*, *hosts as* squares (the default) or labelled cells, *hide groups where every host is ok* and *a host in several groups shows in each*.
- **Summary tiles:** what they count (services or hosts), their groups as for a grid, the filter.
- **Event stream:** the filter (empty: every host and service), which events (state changes, acknowledgements, downtimes, comments, flapping), *hard states only*, *include recoveries to OK*, how many lines show before it scrolls (8, 15 or 30), and rows.
- **Handling:** the filter (empty: every object), *opens with* (the chip it opens on: all, acknowledged, in downtime, upcoming, comments), its sort and rows.
- **Downtimes:** the filter, *opens as* (timeline or list), *shows* (in effect, upcoming, from config), its sort and rows.

**Rows** is *as in settings (comfortable)*, naming Settings → appearance → *row density*, or *comfortable* or *compact* for this view alone. Every header control is saved with the view: the editor shows the same values (*opens with* is the chip, *opens as* the timeline or list switch). **Copy filter from…**, the copy icon in the filter field's corner, lists the other dashboards' and views' filters (a search narrows them by name, group or filter); choosing one fills the field as an edit, so <kbd>⌘Z</kbd> in the field takes it back.

The filter's status says what it matches (`valid · 8 matches, 2 handled`, `empty: every object · 38 problems`, `valid · 124 hosts`, an event stream's `valid · 9 hosts and their services`, handling's `matches 214 objects · 4 being handled`), or where it fails; a grid's filter sees hosts only, so its status says so when the filter reads `service.*`. *Add view* starts the new view from the selected view's filter (a grid starts with an empty one when that filter reads `service.*`). In the preview, the selected view is marked on its header only (the accent bar and a faint accent wash), and a click in a view selects it in the inspector; its header's own controls (sort, `···`, the handled button) change the draft, not the saved dashboard. <kbd>⌘S</kbd> saves, after checking every view: a filter that doesn't work or a custom variable without a name stops the save and selects the view in question. <kbd>Esc</kbd> discards (asking first when something changed). Showing something else keeps your changes for the next edit of the same dashboard, and so does switching environments (also through another environment's notification): back in that environment, edit the dashboard again (a new one: <kbd>⌘N</kbd> in the same group) to continue. Closing the window with kept changes asks first.

The list header's sort menu and the summary bar's handled slot (`28 hidden · show`) change the dashboard directly and are saved with it.

### Views

A dashboard is a list of **views** (`[[…dashboards.views]]` in the settings file and in exported files), each with its own display, filter and options; a dashboard from an earlier version has one view and shows what it showed. Every view has one set of **controls**, always in its header. On a dashboard with one view, that header is the page header (the controls roomy: a stream's `live`, the downtimes view's *timeline | list*, *only mine*, the rows toggle, the sort, `···`) and a list's, grid's or tiles' counts are the summary bar under it; a handling or downtimes view fills the page as in the cluster section. With several views the page stacks them, each under a 36px header: a chevron that folds the view to its header, the display's icon, the name, the filter (faint, cut off first when the window is narrow, and left out where only a few of its characters would fit), then from the right: `···`, the sort (sized to its word; its menu hangs from the header's right edge), the counts (critical, warning and unknown in fixed slots wide enough for three digits; a state at zero keeps its slot, empty) or a handling or downtimes view's chips (their words give way to marks and counts in a narrow header; the downtimes view's *timeline | list* small, left of them), and a list's handled slot. A list's counts, in the header and in the summary bar, are **filter chips**: a click shows only that state, a click on the picked one all again, and the counts stay the same either way. The view's `···` holds a list's grouping and handled problems, a grid's *hosts as* squares or labelled cells, handling's and downtimes' *only mine*, the **rows** group (comfortable, compact, *follow the default*), copy filter expression, collapse view, and *edit view*, which opens the editor with that view selected; *edit dashboard* is in the page header's `···`. Rows choose their density per view: the two-icon toggle of a one-view header (the chosen one filled; while the view follows the settings, the settings' density has a dashed outline), or the rows group when stacked. One cursor moves through every view; the view holding it has an accent bar along its header's left edge. Beside a page of several views the pane is a little narrower, so the headers keep their counts.

- **List:** rows as in [Lists and panes](#lists-and-panes); grouped by host, host group or service group (see below).
- **Host-group grid:** one square per host in each group (host groups, or a host custom variable's values; the header's sort orders the groups *worst first* or *by name*). Worst first goes by the colour of a group's reddest unhandled count (critical or down, then unknown or unreachable, then warning), then by how many hosts have it; the group's dot is that colour too, so it always matches the counts beside it. Healthy hosts are a dim green, problems their state's colour, and a ring is a handled problem (acknowledged or in downtime). Hover a square for the host, its state and its problems; click it (or <kbd>Enter</kbd>) for the host's pane. *Hosts as labelled cells* in the view's `···` shows each host's name in a cell instead; a filled (problem) cell names what makes it so (`host down`, or its worst service) and a hollow one why it is handled (`downtime`, `acknowledged`). A group's name filters the whole page to that group's hosts: the page header shows a chip (`host group edge-ams ×`), every view shows only those hosts, the other groups dim and the cursor stays in the group; <kbd>Ctrl A</kbd> marks only what the filter leaves. The chip's `×` or <kbd>Esc</kbd> shows every group again.
- **Summary tiles:** one tile per group with its worst state, the number of hosts, a bar of the states and the counts. The counts are unhandled problems (and the OK objects), like the view header's, so the tiles add up to the header. A click on a tile filters the page to its group like a grid's group name; a second click shows every group again.
- **Event stream:** the recent events of the view's objects from the local event log, newest first: the time, what happened in the state's colour (`CRITICAL`, `WARNING`, `OK`, `ACK`, `DOWNTIME`, `COMMENT` …), `service on host` and the output or comment. `live` shows while icygui follows Icinga's events. <kbd>Enter</kbd> opens the object's pane.
- **Handling, downtimes:** the views of [Handling and downtimes](#handling-and-downtimes) over the objects the view's filter matches. Stacked, their bands, entries and timeline lines are the page's lines: one cursor moves through them, the band's chevron folds an object, <kbd>Enter</kbd> opens its pane. A team can keep its problems, who handles them and its downtimes timeline on one dashboard. They never count toward the sidebar's number and never notify.

**Hosts with their services.** A service list grouped by host shows each host as a band: the chevron at the left, the host's state mark in the rows' mark column, its name, the address and status (faint) and the counts of its services per state at the right. Its services follow as ordinary rows, not indented. A host shows up to seven of its services: its problems first, worst first (every problem, even when there are more than seven), then OK services in name order; `+ 12 more` shows the rest in place and becomes `− show fewer` in the same slot. The host pane's *services* tab pages the same way. The band has two targets: the chevron folds the host to its band (the band and its counts stay), and a click anywhere else on the band opens the host's pane, like <kbd>Enter</kbd>. Scrolled through a long host, its band sticks to the top of the page. Folding and paging only change what shows: marks stay on folded and paged-away rows (a folded host with marked services takes the marked tint), and *mark all*, the counts and the actions cover every service. A list grouped by host group or service group has the same bands without a state mark; a click on such a band folds it. A list of hosts grouped by host (an earlier version's *host problems grouped by host*) shows as a plain list, since each host would be its own band's only row.

Lists stay smooth with tens of thousands of rows: only the rows on screen are drawn.

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

Each row: the state circle with the time in state under it, `service on host`, the first line of the output, and a tag at the right. **Hollow = handled:** the circle is a ring in the state's colour when the object counts as handled, filled otherwise. Handled means acknowledged, behind a host that is down, or in a downtime that is in effect, whatever the state: an OK service in downtime is a hollow green ring. A downtime that hasn't started yet (scheduled for later, or flexible and waiting for a problem) keeps the circle filled: the problem is still unhandled. The same rule holds wherever an object has a mark: the palette, the pane and its service rows, the sidebar's tabs, the dialogs' target lists and the notification centre (an entry's dot is hollow while its object counts as handled). The tag says why, in one slot, so nothing moves: `ack <author>`; a downtime in effect with how long is left (`downtime 1h 48m`, `host downtime 1h 18m` when it comes from the host's downtime with all its services, `downtime, flexible 1h 12m`); `host down`; `flapping`; a downtime still to come (`downtime at 22:00`, `downtime at tomorrow 08:00`, `downtime at Sat 02:21`, `downtime, flexible, not started`). Checks that are overdue are marked `late 12m`. The summary bar counts the unhandled problems per state, the same whether handled rows show or not. At its right end, in a slot that never moves, `28 hidden · show` says how many handled problems are hidden (problems that are acknowledged, in downtime or behind a down host, and any object in a downtime in effect); a click shows them (hollow) and the slot says `28 handled · hide`, which always hides (with the settings hiding no kind, it hides every kind, on that view). Which kinds hide by default is set in Settings → appearance → *handled problems* (*hide acknowledged*, *hide in downtime*, *hide services of hosts that are down*); a view can set its own in the editor, and the slot's choice is saved with the view. With compact rows (Settings → appearance) a row is one line: a smaller circle, `service on host`, the tag, and the time at the right end; with clock times the time says since when (`13:58`) instead of how long. See [Appearance](#appearance).

Click a row (or press <kbd>Enter</kbd>) for the **pane** at the right. Its header has the object's kind, then `×` (close) and `↗ open as tab`; the `×` sits left of the link on every platform, so it never sits next to the window's own close button, and a tab keeps the link's place empty.
- **Downtimes** show in a banner fixed under the pane's header, above the scrolling body, so it stays in view. **Blue** (the accent) means the downtime is in effect: `In downtime 1h 48m left`, the window and since when (`fixed · 13:00 → 16:00 today · started 1h 12m ago`), who set it and why (two lines; the whole comment is in its tooltip), and a line along its bottom edge showing how much has passed. A service in its host's downtime (scheduled with *all services*) says `In downtime with its host` and names the host, a link to the host's pane. **Grey** means scheduled but not in effect yet, so a problem still counts as unhandled: `Flexible downtime not started` (how long it lasts once a problem starts it, and its window) or `Downtime scheduled starts in 7h 48m`. With several downtimes the banner shows the one in effect (else the next to begin) and its last line names the others, which are in the pane's *handling* thread below (the service pane: after the action buttons; the host pane: at the top of its services tab), each with its window, status, author and comment and a remove `×` that shows on hover; the banner's own downtime keeps its place in the thread as `the downtime in the banner above`. A downtime from the config (a `ScheduledDowntime`) shows a lock: Icinga refuses to remove it, so its remove is disabled and says so. The downtime's automatic comment isn't listed again. When a downtime starts while the pane is open, the banner comes in and the body moves down once.
- **A host in a downtime that doesn't cover its services** (scheduled without *all services*): as in Icinga Web, its services are not in downtime and still notify; a service's host line shows `host in downtime until 15:00` after the host's name, a link to the host and its banner.
- **Service:** state, host link, duration, hard/soft attempt; the action buttons (below); the plugin output and long output; the performance data with warning and critical thresholds (exceeded values coloured); the check (command, interval, last and next check, endpoint, zone, attempt, latency and runtime, Icinga's switches read-only, flapping, *notified*: whom Icinga notified and when); its handling thread (its acknowledgement, downtimes and comments, oldest first, with remove buttons and a field to add a comment); custom vars; groups; notes and links; the recent history from the local event log (downtimes in the accent blue, like acknowledgements).
- **Host:** state, address, uptime, output; actions; tabs *services* (paged like a host in a list: its problems, then OK services up to seven, `+ N more` for the rest), *history* (a downtime set on the host with all its services is one line, `DOWNTIME host and its 11 services`, not one per service), *vars* and *config* (the check configuration and Icinga's feature switches, read-only); parents and children from dependencies.
- **The action buttons** keep their places whatever the object's state: the first is *acknowledge* for an unacknowledged problem, *remove ack* for an acknowledged one, and a disabled *acknowledge* otherwise; then *downtime*, *check now* and *comment*. An action on its way shows `…` in place of its key until Icinga has done it (the row's tag says what: `ack pending…`). While rows are marked in the list, the keys act on them, so the buttons show no keys; a click on a button still acts on the pane's object.
- `↗ open as tab` pins the object in the sidebar's *open* section; tabs survive restarts.
- The pane's `···` menu holds the less common actions (submit check result, run command, remove downtimes), copies the name, the output, a filter expression or the banner's downtime's name (for another downtime's *triggered by*), and watches or mutes the object. Notes URLs and action URLs open in the browser.
- **Protected custom variables** show `***` in the vars and in links, never their value: names matching `*pw*`, `*pass*`, `*community*` (Icinga Web's defaults, `community` widened to `snmp_community`), `*secret*`, `*token*`, `*auth_pair*`, `*auth_key*` and `*priv_key*`, ignoring case, at any nesting level. Filters still see the real values.

## Handling and downtimes

Two view kinds show who is handling what. The sidebar's **cluster** section holds them for the whole environment (also from the palette: <kbd>⌘K</kbd>, then `handling` or `downtimes`; `acknowledged` and `comments` open handling on that chip), and a dashboard has them as views with a filter of their own (see [Views](#views)): a dashboard with only a handling view is a full handling page for its objects. They use the dashboards' keys, marks, pane and selection bar, and they come from what icygui already holds from Icinga's event stream: opening them sends no request, and they update as events arrive. Their matching is evaluated with the dashboards, from the objects the filter matches, as the engine's events come in.

Both group by object, the way a host with its services is grouped on a dashboard: a slim **band** per object (its state dot, a ring when it is handled; `service on host` or the host, its state and output; at the right what its thread holds, `in downtime · 3 comments`), then its entries. The band's chevron only folds and unfolds the object; a click anywhere else on the band, or on an entry, opens the object in the pane. Beside the pane, the names and hosts stay whole: the output and the details give way first. An object never has two bands.

- **Handling:** one thread per object, oldest first inside, so it reads like a chat: its acknowledgement (who, when, the comment; *sticky* and the expiry in fixed slots at the right, `expires 15:00, in 48m` in yellow when it is under two hours away), its downtimes in effect and to come (with their window, the time left or when they start), and its free-standing comments. Icinga's own comments (a downtime's or an acknowledgement's automatic comment, flapping) are never separate entries. Later entries are indented as replies. The chips filter the entries: *all*, *acknowledged*, *in downtime*, *upcoming*, *comments*; the band keeps saying what the whole thread holds. The default sort is *latest activity* (the thread someone touched last first; a downtime from the config is no one's activity). On *acknowledged* the sort is *expires soonest*, in sections: *expires within 2 hours* (then the problem is a problem again and notifies), *expires later*, and *no expiry* last. The header's sort menu also offers object and author. A long thread shows its first seven entries, then `+ N more`.
- **Downtimes:** a **timeline** (the default) or a **list**, switched in the header. The timeline has an axis of twelve hours around now (`16:00 to 04:00 · now 14:12`) and a line at now; each downtime is one line with its bar: in effect, a track with the elapsed part in the accent; upcoming, a faint track; flexible, a dashed line over its window; one that starts after the axis says when at the right edge (`→ Sat 06:00`, with a lock for one from the config). An object with one downtime is one line; a host with several has a band and one line per downtime. The list has two sections, *in effect* (ending soonest first) and *upcoming* (starting soonest first); an object sits in the section of its most current downtime and appears once. A lone downtime is one row with the object's name in it; a band with rows only for an object with several downtimes or a host's downtime with folded services. The chips: *all*, *in effect*, *upcoming*, *from config*. Downtimes from the config (`ScheduledDowntime`) show a lock and `from config`: they can be marked but not removed (Icinga refuses, and the config brings them back).
- **A host's downtime with all its services** is one group: under the host's downtime, `18 services, same downtime · folded: they are identical`. Click it (or <kbd>→</kbd>) to list the services, seven at a time with `+ N more`; <kbd>←</kbd> folds them again. A service with a downtime of its own (different from the host's) stays visible on its own.
- **Only mine** (the header's switch, in the `···` menu when the view is narrow, or <kbd>m</kbd>) shows what the environment's *author* set (default: the API user); the chips' end counts what it hides. <kbd>s</kbd> picks the next sort. The cluster section's entries keep their chip, sort, *only mine*, display (timeline or list) and rows per environment, across restarts; a dashboard's view keeps them with the view (*only mine* and rows are personal: exported dashboards leave them out). The header's `···` also folds or unfolds every object.
- **In the pane** the object's thread follows the action buttons: *handling 4 · oldest first*, the same entries (the downtime the banner shows says only `the downtime in the banner above`), a `×` on a comment or a downtime when you hover it, and **add a comment**: type and press <kbd>Enter</kbd> (or *comment*); <kbd>c</kbd> puts the keyboard in the field when the pane shows a thread, <kbd>Esc</kbd> clears it, then gives the keyboard back to the list. While you type, the list's and the pane's keys are the field's. The comment goes through the same path as the comment dialog (permissions, the pending marker, toasts); without the permission the field says which one is missing.
- **Removing:** mark entries (<kbd>x</kbd>, Shift-click, ⌘-click, <kbd>⌘A</kbd>; a band's marks are its entries) and press <kbd>⌫</kbd> (or the bar's *remove selected*, *remove downtimes*). Nothing is sent before you confirm: the dialog lists every target (the box scrolls), downtimes grouped by downtime (a host's downtime lists the host and each service, which Icinga removes with it), comments and acknowledgements under their own headings; it says which problems notify again, what it skips and why (downtimes from the config, kinds the API user may not remove), and counts what goes on its button (*remove 26 downtimes*, *remove all 9*). <kbd>Enter</kbd> sends one action per kind: downtimes and comments by name, up to 200 names a request, acknowledgements by object. Without the permission to remove any of them nothing opens and a message says which one is missing. Removing several acknowledgements from a dashboard asks with the same dialog.
- **Permissions:** without permission to read downtimes (`objects/query/Downtime`) the downtimes view says so; handling still shows what it can and says what it can't (`no comments: needs objects/query/Comment`).
- The bar's *copy names* and `···` → *copy filter expression* copy the marked entries' objects; the header's `···` copies all of them.

## Keyboard shortcuts

| Keys | Where | What |
|---|---|---|
| <kbd>j</kbd> / <kbd>↓</kbd>, <kbd>k</kbd> / <kbd>↑</kbd> | list | Next / previous row, through every view of a dashboard (a grid: the host a line down or up) |
| <kbd>Home</kbd>, <kbd>End</kbd>, <kbd>Page Up</kbd>, <kbd>Page Down</kbd> | list | First, last, a page up or down |
| <kbd>Shift J</kbd> / <kbd>Shift ↓</kbd>, <kbd>Shift K</kbd> / <kbd>Shift ↑</kbd> | list | Extend the marked rows |
| <kbd>x</kbd> | list | Mark or unmark the row (also: Shift-click for a range, ⌘-click / Ctrl-click) |
| <kbd>⌘A</kbd> | list | Mark every row |
| <kbd>Enter</kbd> | list | Open the pane (on a view's header: fold or unfold it; on `+ N more`: show the rest) |
| <kbd>Tab</kbd>, <kbd>Shift Tab</kbd> | dashboard | Next / previous view (its first row, else its header) |
| <kbd>←</kbd>, <kbd>→</kbd> | dashboard | On a view's header or a host's band: fold, unfold; on `+ N more`: show fewer, show all; on a grid: previous / next host. With nothing to fold (a row, an event, a folded band, the grid's first host), <kbd>←</kbd> goes to the row's band, else to its view's header, where <kbd>←</kbd> folds the view |
| <kbd>⌘Enter</kbd> | list | Open as a tab |
| <kbd>Esc</kbd> | list, pane | Close the pane, clear the marks, then the page's group filter |
| <kbd>a</kbd> | list, pane | Acknowledge |
| <kbd>d</kbd> | list, pane | Schedule downtime |
| <kbd>r</kbd> | list, pane | Check now |
| <kbd>c</kbd> | list, pane | Add comment |
| <kbd>⌫</kbd> | handling, downtimes | Remove the marked entries' downtimes, comments and acknowledgements (asks first) |
| <kbd>→</kbd>, <kbd>←</kbd> | handling, downtimes | Unfold, fold: an object's band, a host's folded services, `+ N more`; inside a group <kbd>←</kbd> folds it to its band |
| <kbd>m</kbd>, <kbd>s</kbd> | handling, downtimes | *Only mine* on or off; the next sort |
| <kbd>c</kbd> | handling, downtimes | With the pane showing a thread: the keyboard to its *add a comment* field (<kbd>Enter</kbd> sends, <kbd>Esc</kbd> clears, then leaves) |
| <kbd>←</kbd> <kbd>→</kbd> | remove downtime | *This service only* or *the host and its services* |
| <kbd>⌘K</kbd> | anywhere | Command palette |
| <kbd>⌘N</kbd> | anywhere | New dashboard |
| <kbd>⌘1</kbd> … <kbd>⌘9</kbd> | anywhere | Select dashboard 1 to 9 |
| <kbd>⌘B</kbd> | anywhere | Show or hide the sidebar |
| <kbd>Ctrl Tab</kbd>, <kbd>Ctrl Shift Tab</kbd> | anywhere | Next / previous tab |
| <kbd>⌘W</kbd> | anywhere | Close the tab (or the list) |
| <kbd>⌘,</kbd> | anywhere | Settings |
| <kbd>⌘⇧E</kbd>, then <kbd>↑</kbd> <kbd>↓</kbd> | settings | Move through the categories (<kbd>Enter</kbd> to the page's first control) |
| <kbd>⌘F</kbd> | settings | Search the settings |
| <kbd>Tab</kbd>, <kbd>Shift Tab</kbd> | settings | Next / previous control or field |
| <kbd>Space</kbd>, <kbd>Enter</kbd>; <kbd>←</kbd> <kbd>→</kbd> | settings | Switch or press the control; the previous / next choice |
| <kbd>Esc</kbd> | settings | Close the dropdown, clear the keymap filter or the search, then close |
| <kbd>⌘Q</kbd> | anywhere | Quit (also from the tray) |
| <kbd>⌘S</kbd>, <kbd>Esc</kbd> | dashboard editor | Save, discard |
| <kbd>↑</kbd> <kbd>↓</kbd>, <kbd>⌥↑</kbd> <kbd>⌥↓</kbd>, <kbd>⌘⌫</kbd> | dashboard editor (its views list) | Select the view above / below, move it up / down, remove it |
| <kbd>Tab</kbd>, <kbd>Shift Tab</kbd> | dialogs | Next / previous field (a dialog without fields keeps the keyboard) |
| <kbd>Enter</kbd> | dialogs | Send (<kbd>Shift Enter</kbd> for a new line in a comment) |
| <kbd>⌘Enter</kbd> | dialogs | Send from any field |
| <kbd>Esc</kbd> | dialogs, palette | Close |
| <kbd>↑</kbd> <kbd>↓</kbd> (or <kbd>Ctrl P</kbd> <kbd>Ctrl N</kbd>), <kbd>Enter</kbd> | palette | Choose, run |
| <kbd>Tab</kbd> | palette | Open the chosen object as a tab |
| <kbd>⌘Enter</kbd> | palette | Run a verb on all matches |

The action keys act on the marked rows if there are any, else on the pane's object, else on the row under the cursor (the pane's buttons show no keys while rows are marked).

**The command palette** (<kbd>⌘K</kbd>) searches hosts, services, dashboards and commands: switching environments, new dashboard and group, import and export, reload from Icinga, pause and resume notifications, the notification centre and settings, the [handling and downtimes views](#handling-and-downtimes) (`handling`, `downtimes`, `acknowledged`, `comments`), and the actions on the current object. Every row starts with a mark: an object's state dot for an action on it (`● Acknowledge · postgres-replication  on db-prod-03 · critical`), a stack in the worst state's colour for an action on several, a dashboard's dot, or a small icon for other commands. The current object's actions (the pane's object, else the marked rows or the row under the cursor) come first, with their keys (`a`, `d`, `r`, `c`). Start a query with a verb to act on what it finds: `ack db-prod-03`, `dt web`, `check mq-prod`, `comment …` (also `acknowledge`, `downtime`, `recheck`, `note`). The current object comes first if it matches, then problems; <kbd>⌘Enter</kbd> (the *all N matches* row) opens the action's dialog listing every match, a check too: nothing is sent before you confirm.

## Actions

All actions are runtime operations through Icinga's `/v1/actions`. icygui never changes configuration or object attributes (no `objects/modify`, no enabling or disabling checks or notifications in Icinga).

| Action | Options |
|---|---|
| **Acknowledge** (<kbd>a</kbd>) | Comment; sticky (stays until OK, through other problem states); persistent (keep the comment after the acknowledgement ends); expiry (1h, 4h, 1d, 08:00 tomorrow, or a time). Icinga is never asked to send notifications for it. *Remove acknowledgement* on acknowledged objects |
| **Schedule downtime** (<kbd>d</kbd>) | For a host, *all services* (on by default, Icinga's `all_services`) sits at the top, right above the box that lists what it targets: the host and each of its services (the box scrolls); off, the box lists the host alone. The title and the button count them (`k8s-node-04 and its 23 services`, *schedule 24 downtimes*). Then: comment; start and end with presets (30m, 1h, 2h, 4h, 8h, 1d, 1w, 08:00 tomorrow); fixed, or flexible with a duration; child hosts (none, triggered, non-triggered); a triggering downtime, picked from the current downtimes of the objects, their hosts and those hosts' parents (or any downtime's name, which a pane's `···` menu copies). **Removing** a downtime (the banner's *remove downtime*, an other downtime's `×`, *remove downtimes* for the selection) always asks first, listing every downtime it removes (a host's with all its services lists the host and each service) and counting them on the button; for a service in its host's downtime it asks whether to remove it for *this service only* or *the host and its 23 services* (click, or <kbd>←</kbd> <kbd>→</kbd>; the box keeps its size, so nothing moves). It says which problems notify again once their downtimes are gone, and names every downtime from the config it skips (Icinga refuses to remove them). Only the downtimes listed are removed, by name: one someone schedules while the dialog is open stays. An object whose only downtime comes from the config offers no removal (the pane's `···` shows it disabled, with the reason) |
| **Check now** (<kbd>r</kbd>) | Forced, at once. More than 20 objects ask first |
| **Add comment** (<kbd>c</kbd>) | Comment and an optional expiry. The pane's thread also has a field to add one (<kbd>c</kbd>, then <kbd>Enter</kbd>). Remove comments from the pane, or many at once from [handling](#handling-and-downtimes) |
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

**What a notification looks like:** `CRITICAL · postgres-replication on db-prod-03`, the first line of the output, and the group and dashboard (without the output when Settings → notifications → *show plugin output* is off, for shared screens and the lock screen: the title still names the object and its state); with more than one environment the environment's name comes first (`production · CRITICAL · postgres-replication on db-prod-03`). *Open* (or a click) brings the window back (it is recreated if you closed it), switches to the notification's environment and shows the object's pane. *Acknowledge* opens the acknowledge dialog for the object in its own environment without switching (its title names the environment), and the acknowledgement goes to that environment's Icinga, never to the one on screen.

**Rules** are inherited from top to bottom, and the most specific one wins:
1. **Environment default rule** (Settings → *notifications*; its *environment* dropdown picks whose rules the page shows, the one on screen when the settings open).
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

**Quiet hours** (Settings → notifications): a daily window (it may cross midnight, e.g. 22:00 to 07:00) on chosen days. During it, notifications are recorded in the centre but not shown, except critical and down ones if *critical and down still notify out loud* is on.

**Storm control:** when more notifications than the threshold arrive within the window [5 within 10 seconds], the rest become one summary (`14 new problems in prod-cluster`). They are all in the centre, marked *silent*.

**Pause:** 30 minutes, 1 hour, or until 08:00, from the notification centre, the palette, the settings or the tray; *resume* ends it. A pause holds for every environment (with several, the chips say *pause all*). One environment can be muted on its own instead (see [Environments](#environments)). While the environment on screen is paused or muted, the footer's clock turns into a bell-off.

**The notification centre** (the clock icon in the footer; its badge counts the unread notifications of the environment on screen) lists the recent notifications, newest first, under *now*, *last hour*, *earlier today*, *yesterday* and *older*:

- **Scope:** with several environments, the chips at the bottom pick whose: *all*, the environment on screen (where it opens, filled) or another one. A chip turns blue while its environment has unread notifications (*all*: another environment has); the heading counts the unread ones of the chip chosen. Environments that don't fit go behind `···`. The gear at the bottom right opens the notification settings.
- **Entries:** time, title, the output's first line and where it matched (`overview`, `databases / production`; in *all* with the environment in front: `staging · overview`). Unread ones have a bright title. Click one to open its object (switching to its environment first) and mark it read; opening an object anywhere marks its notifications read too. Click a label to show only that place's notifications, again to show all.
- **Silent entries** (no desktop notification) have a dimmer title and the reason: `silent · paused`, `silent · quiet hours`, `silent · storm`. An entry's dot is hollow while its object counts as handled (acknowledged, or in a downtime in effect), as in every list.
- **Storms:** what a storm held back collapses into its summary (`24 new problems in prod-cluster · 19 held back`; while the storm goes on, `19 notifications held back by a storm`); click it to list them, again to fold them.
- **Pause row:** *pause all* with 30 minutes, 1 hour and until 08:00 for every environment, or until when they are paused (*resume*); in the scope of an environment muted on its own, until when it is muted (*unmute*).
- The centre keeps its size and place while it is open, so nothing moves under the pointer when notifications arrive or you pick a scope or a label; in a short window its list is shorter. While the pointer is over the list, new notifications wait until it leaves (the heading counts them at once), so the entry under the pointer stays the one a click opens.
- *mark all read* marks what the list shows: the scope's environments, or only what a label left.

The history stays on this computer, for as long as Settings → icinga → *keep events for* says.

No notifications are sent for what's already wrong when icygui connects, but icygui keeps track of it: a service still critical from before its host went down waits for a fresh check (or five minutes) once the host is back, and an object that is flapping stays quiet until it stops. A problem that notified before icygui restarted (or before the environment's connection settings changed) still notifies its recovery, as long as the event log keeps it. Problems that a reconcile finds after a reconnect do notify, also one that recovered and failed again while your laptop slept.

Platform notes: on macOS, notifications come from the app bundle (allow them in System Settings → Notifications the first time, with sounds); a rule's sound is the system's alert sound, also while icygui is in front. On Linux they go to your desktop's notification server, with a sound by state (critical, warning, recovery) where the server plays sounds.

## In the background

- **Closing the window** keeps icygui running in the tray (Linux) or the menu bar (macOS), still connected to every environment and notifying for each (Settings → general → *keep running in the tray*, on by default). Where no tray icon can be shown (stock GNOME without the AppIndicator extension), closing the window quits, and the settings say so. With unsaved work in the window (dashboard editor changes, also those kept in another environment, environment editor changes, text typed in an action dialog) it asks first.
- **The tray icon** is the logo mark tinted with the worst unhandled state across all environments; its tooltip has a line per environment with its connection (`partial view: zone ams` while on a satellite, *muted* when muted on its own) and its counts; its menu has *Open*, *Pause notifications* (every environment), the environments and *Quit*. Choosing another environment there switches like the footer does.
- **Start at login** (Settings → general) starts icygui in the background, without a window, with every environment connected in [quiet mode](#quiet-mode) (a launch agent on macOS, an XDG autostart entry on Linux). If no tray shows its icon within 20 seconds (a panel that starts after icygui gets that long), the window opens instead.
- **One instance:** starting icygui again brings the running one's window forward.
- **Quit** from the tray, the app menu or <kbd>⌘Q</kbd>.

### Quiet mode

While nobody looks, icygui follows Icinga more quietly (Settings → general → *quiet mode when hidden*, on by default): every environment that isn't on screen, and the one on screen once the window has been hidden for half a minute: closed to the tray, or not shown at all as your system reports it (minimised, on another virtual desktop or Space, the display asleep; on macOS, and on X11 without a compositing window manager, also a window other windows cover completely). A window that is merely unfocused or partly covered, such as a dashboard on a second screen, stays fully live, and a quick look at another window changes nothing. The environment switcher shows `quiet` instead of the age of the last event for an environment in quiet mode, with a green dot: its stream can be silent for minutes without anything being wrong. The environment you switch to, or the one on screen when the window comes back, counts as live at once: the footer shows the age of its last event, never `quiet`.

- **Notifications are never delayed:** state changes, acknowledgements, downtimes, comments, flapping and Icinga's own notifications still arrive the moment they happen; icygui only leaves out the check results that change nothing (about 99 % of the event stream, one per check).
- **Stale while quiet:** check outputs (the text of a check that didn't change the state), last-check times and the *late* markers. States, counts, the tray icon and the notification centre stay current. After an acknowledgement or downtime ends, the "is it still a problem?" confirmation waits a few minutes instead of for the next check.
- **Less work for the master and your laptop:** the status is read every 5 minutes instead of every 30 seconds, the lean reload runs every 30 minutes or more, and nothing is fetched for rows nobody sees.
- **Waking up is instant:** when the window opens or you switch environments, what changed state is already there with its output; the full event stream comes back within a moment without missing anything (if the old stream may have held something back, icygui checks Icinga's state counts and reloads when they disagree), the object you open is fetched first (a notification you click was fetched when it was shown), then the rows on screen whose output may have changed, however many problems there are, then the other problems' outputs. A pane never waits: it shows what icygui has and, if fresher details take longer than about 300 ms, a small *updating…* beside `service` or `host` in its header, which goes away when they are in. Moving through the list with `j` and `k`, icygui asks only for the row you stop on.
- **Started at login** (in the background, with the tray), every environment starts quiet and loads after a short random wait that grows with the size of its Icinga; opening the window ends the wait. A problem that begins during the wait notifies as soon as the load is in (what was already wrong before stays quiet, as on any start).
- Turn it off to keep every environment fully live all the time (each then costs its Icinga a full event stream).

## Settings

<kbd>⌘,</kbd> (<kbd>Ctrl ,</kbd> on Linux), the app menu on macOS or the palette (*settings*, *notification settings*) open the settings: a large panel over the window, which dims behind it. <kbd>Esc</kbd>, the × or a click outside closes it.

On the left: a search field, the six categories and, under the one open, its sections (the one in view in blue; a click scrolls to it). <kbd>⌘⇧E</kbd> (*focus navbar*) moves the keyboard there (the open category gets a blue frame), then <kbd>↑</kbd> <kbd>↓</kbd> pick a category and <kbd>Enter</kbd> goes to its first control. On the right: the category's rows, each a name, one line of description and its control. In a small window or at a large interface size, a row too narrow for its control puts the control on a line under the name, and the header's *edit in settings file* shrinks to its icon.

**From the keyboard:** <kbd>Tab</kbd> and <kbd>Shift Tab</kbd> go from the search field through every control of the page in order (switches, choices, chips, dropdowns, buttons, text fields) and the header's buttons, then round again; the control with the keyboard has a blue frame. <kbd>Space</kbd> or <kbd>Enter</kbd> acts as a click (a switch or chip flips, a dropdown opens), <kbd>←</kbd> <kbd>→</kbd> move a choice or a dropdown to its neighbour. <kbd>Esc</kbd> closes an open dropdown, clears the keymap filter or the search, and otherwise closes the panel.

**Changes apply at once**, as in Zed: every switch, choice and chip is saved the moment you click it, and the header says *✓ saved* (*not saved*, in red, with the reason when the file can't be written). A text field applies on <kbd>Enter</kbd>, <kbd>Tab</kbd>, or when you click elsewhere or close the panel (and before the notifications page shows another environment); a value that doesn't read (`lots` hours, a storm threshold of 0, a reconcile interval under a minute) shows its problem in red in place of the row's description and changes nothing until you fix it (nothing moves meanwhile). A value you typed stays in its field until it applies, whatever else changes. There is no save or cancel.

**The search** looks through every category at once: the categories without a match dim, the others count their matches, and the page lists the matching rows under `category · section` with the match in blue. The rows work in place. A section's or category's name brings all of it (`quiet hours`, `keymap`); environments are found by name and host, shortcuts by what they do and their keys.

- **general:** *keep running in the tray*, *start at login* and *quiet mode when hidden* (see [In the background](#in-the-background)).
- **appearance:** *theme*, *interface size*, *row density*, *times in lists* and *handled problems* (which handled problems lists hide by default), with a preview of the selected dashboard's first rows as they look with the choices. See [Appearance](#appearance).
- **notifications:** the rules of one environment (pick it under *environment*; each environment notifies on its own), its *notifications for <name>* switch, *pause all*, *show plugin output*, the default rule, quiet hours, storm control, the watched and muted objects (*remove* ends a watch or mute) and every group's and dashboard's setting (inherit, on, off or custom, with the custom rule's conditions under it). See [Notifications](#notifications).
- **icinga:** *reconcile with Icinga* (adaptive, or a fixed interval in seconds or as `10m`), *keep events for* (hours), and every environment with its health dot, its URLs' hosts and what its connection does; the gear opens its editor over the settings, *add environment* a new one.
- **keymap:** every shortcut in effect (yours included), what it does, its keys and where it works, filtered by name or key. *edit keymap file* opens `keymap.toml` (created with commented examples the first time) in your editor.
- **advanced:** *log level* (applies at once), the log folder and the config folder (*open folder* opens them in your file manager), the version and *about*.

**The settings file.** *edit in settings file* (top right) opens `config.toml` in your default editor. An edited file is taken over when you come back to icygui's window, or as soon as icygui would write the file (a change in the panel, the tray's environment switch), whichever comes first, as a change in the panel would be: app-wide settings and the appearance, every environment's rules and dashboards; an environment whose URLs, login or TLS changed reconnects, a new one connects, one removed from the file disconnects (its password and event log stay); *start at login* adds or removes the login entry. icygui never writes over your edit: it merges it with what changed in the window meanwhile (each setting keeps whichever side changed it; one changed on both sides keeps the window's value) and writes the result, keeping your comments when the file already says it all. A file that doesn't read (a typo) is reported once in a banner, and the settings header says *file has errors* (the problem in its tooltip); nothing is written over it until it reads again, and what you change in icygui meanwhile is kept and added to the file once you fix it.

### Appearance

Every appearance setting applies the moment you choose it, to the whole window (the list, the panes, the sidebar, the dialogs, the palette, the settings themselves), and is kept in `config.toml` under `[appearance]`.

- **Theme:** *follow system* (the default) takes your desktop's light or dark mode and switches with it at once, while icygui runs; *dark* and *light* keep one whatever the desktop does. Settings from rc1, which had no theme choice and always wrote `dark`, follow the system too. The light theme has the dark one's structure: a white window, a slightly grey sidebar, the pane a shade off white; the state colours have two shades, the friendly one for circles, dots and bars and a darker one for words and numbers (`CRIT`, `late 3m`, a perfdata value over its threshold, the kinds in the history), so they read on white; the accent blue likewise has a darker shade for words (links, matches in the palette, `1h 48m left`) that reads at 4.5:1 or more even on the selected and marked rows. On Linux the desktop's mode comes from its colour-scheme setting (the XDG desktop portal: GNOME, KDE and most others); a desktop without one, or with no preference, counts as light. The tray icon follows the desktop, not this setting.
- **Interface size:** small, default or large: 90, 100 or 115 % of the design's text and spacing, everywhere in the window. The window itself keeps its size; at 115 % the summary bar shows counts only sooner, and the pane covers the list in a narrower window.
- **Row density:** *comfortable* rows have two lines (the output under the name) and the time under the state circle; *compact* rows are one line per object, a 14 px circle and the time at the right end, about twice the rows on screen. Compact also drops the second line in the host pane's services, the history and the notification centre. Long lists stay as fast either way.
- **Times in lists:** *relative* says how long the object has been in its state (`14m`, `2h`, `41d`); *clock* says since when: `13:58` today, the date on an earlier day (`Oct 3`), the year before this one (`2025`). The history and the notification centre always show when something happened as a clock time.
- **Handled problems:** Icinga's *handled* in three switches, all on by default: *hide acknowledged* (problems someone has acknowledged), *hide in downtime* (hosts and services whose downtime is in effect) and *hide services of hosts that are down* (the host's own problem covers them; the host still shows). They are the defaults of every dashboard list; a dashboard can show its handled problems (*N hidden · show* in its summary bar), and that choice is saved with it. A problem handled for several reasons hides when any of its switches is on. Filters on `acknowledged` or `downtime_depth` still work; the switches apply on top. The sidebar's counts never change with them: they count unhandled problems. Kept under `[appearance.hide_handled]` (`acknowledged`, `in_downtime`, `host_down`).

**The keymap file** adds your own bindings on top of the defaults, read again when you come back to the window:

```toml
# Bindings before the first table work everywhere.
"ctrl-shift-p" = "icygui::ToggleCommandPalette"

# Each table is a key context: Workspace (the window), DashboardView (the
# list), ObjectPane (an object open as a tab), CommandPalette,
# DashboardEditor, ActionDialog, SettingsPanel.
[DashboardView]
"space" = "icygui::ToggleMark"
"x" = "none"                                # switches the default off

[Workspace]
"alt-3" = ["icygui::SelectDashboard", 3]    # an action with an argument
```

A binding that can't be used (an unknown action, keys that don't parse) is skipped; the keymap page says how many and the log names each.

## Files and data

| | Linux | macOS |
|---|---|---|
| Settings | `~/.config/icygui/config.toml` (and `config.toml.bak`) | `~/Library/Application Support/io.github.alexykn.icygui/config.toml` |
| Your own key bindings | `~/.config/icygui/keymap.toml` | `~/Library/Application Support/io.github.alexykn.icygui/keymap.toml` |
| Window, open tabs, selected dashboards | `~/.local/share/icygui/state.toml` | `~/Library/Application Support/io.github.alexykn.icygui/state.toml` |
| Local event log (one per environment) | `~/.local/share/icygui/events-<id>.sqlite3` | `~/Library/Application Support/io.github.alexykn.icygui/events-<id>.sqlite3` |
| Log | `~/.local/state/icygui/logs/icygui.log` | `~/Library/Logs/io.github.alexykn.icygui/icygui.log` |
| Passwords | Secret Service keyring | Keychain |

The Linux paths follow `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_STATE_HOME`.

- The settings file is written atomically, readable only by you (0600), with the previous version kept as `config.toml.bak`. It never contains passwords.
- The event log keeps state changes, acknowledgements, comments, downtimes, flapping and notifications for 48 hours (Settings → icinga → *keep events for*). It feeds the notification centre and the history tabs, so history starts when icygui first connected.
- The log rotates at 10 MiB and keeps four old files. Passwords and authentication headers are never logged. Settings → advanced → *log level* sets how much goes in, at once (*debug* adds each request's method, path, status and time); `RUST_LOG`, when set, decides at start instead, e.g. `RUST_LOG=info,icygui=debug,ic_core=debug,ic_api=debug icygui` (start the list with `info`: without it, every other part's messages, warnings and errors included, are left out).
- *Reconcile with Icinga* (Settings → icinga): *adaptive* reloads the lean object list at an interval that grows with the installation (every 5 minutes up to about 10 000 hosts and services, about every 15 minutes at 30 000, at most hourly), and less often while the event stream has been running without a break and the reloads found nothing it missed (up to an hour); *fixed interval* lets you choose (at least a minute, and at least 5 minutes from 5 000 objects on). Quiet mode reconciles every 30 minutes at most.

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

**Something else.** The log file (see [files](#files-and-data)) has the details; run with `RUST_LOG=info,icygui=debug,ic_core=debug,ic_api=debug` for more. When reporting a problem, include the log, `icygui --version`, your platform, and Icinga's version (shown by *test connection*). The log contains host and service names but never passwords.
