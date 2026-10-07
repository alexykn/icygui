# First run against a production Icinga

A short checklist for the first time icygui connects to the real Icinga at work. It takes about fifteen minutes, most of it waiting for a config deployment. The [user guide](user-guide.md) has the details behind every step.

## Before you start

- [ ] **Install** the release candidate on the laptop you use for on-call:
  ```sh
  curl -fsSL https://raw.githubusercontent.com/alexykn/icygui/main/install.sh | bash -s -- --version 0.1.0-rc.1
  ```
  (Without `--version` the script takes the newest release, a release candidate while there is no full release.) Or the `.dmg`, `.deb` or `.tar.gz` from the [pre-release](https://github.com/alexykn/icygui/releases), or [build from source](development.md) (`cargo xtask install`) if the release isn't published yet. `icygui --version` prints the version you'll report.
- [ ] **Try the demo once** (`icygui --demo`) to learn the keys without touching anything real.
- [ ] **Nothing else points at production.** The contract tests (`contract/`) and the load and scale tests (`ic-mock`'s `large` scenario, `contract/scale/benchmark.sh`) only ever run against the disposable Docker Icinga on your own machine; the contract tests refuse any other instance. Against production, the only test is using the app.

## 1. A least-privilege API user

- [ ] Add the `ApiUser` from the [user guide](user-guide.md#the-api-user) through your usual config management (the Ansible role), with a long random password from your password manager.
  - For a first look, **leave out the `actions/*` lines**: icygui is then read-only, and its action buttons are disabled with the reason. Add them once you trust it.
  - `actions/execute-command` isn't in the list, and should stay out unless you need *run command*: together with macros it can run any command on your agents (see [the user guide](user-guide.md#the-api-user)).
  - Don't give it `filter-expression` permissions or any permission beyond the list; icygui needs none.
- [ ] Deploy and reload Icinga (`icinga2 daemon -C` first, as always).

## 2. Certificate: CA or pin

- [ ] Get the master's CA certificate (`/var/lib/icinga2/certs/ca.crt`) and keep it somewhere stable on the laptop, e.g. `~/.config/icygui/icinga-ca.crt`; or
- [ ] get the fingerprint of the API certificate on **each master (and satellite) you will list as a URL**, to compare in step 3: pins are per URL, and a URL whose certificate isn't trusted can't take over when the others fail:
  ```sh
  openssl x509 -noout -fingerprint -sha256 -in /var/lib/icinga2/certs/$(hostname -f).crt
  ```
- [ ] If you connect through a load balancer, an alias, an IP address or an SSH tunnel, note the name in the certificate for *server name*.
- [ ] Several masters behind one address (a load balancer or round-robin DNS in front of an HA zone): use the CA file, **not** a pin. Each master has its own certificate, a pin trusts only one, and the connection stops whenever the other one answers. Icinga's node certificates name only their own node: with the CA file, the masters' certificates must include the load balancer's name, or connect to one master by its own name.

## 3. Connect

- [ ] Start icygui. In the onboarding form: name (`prod`), URL `https://<master>:5665` (an HA zone: *+ add URL* for the second master; satellites, if you want a fallback, after the masters: see [clusters](user-guide.md#clusters-which-urls-to-list)), the API user and password, your own name as *author*. *test all URLs* should show `full view` for every master.
- [ ] TLS: set the CA file (one file for every URL), **or** press *test connection* (*test all URLs* with several), compare each URL's SHA-256 with the one from step 2 for that node, character by character, and only then *trust this certificate* for it. Every URL needs its own trusted certificate; check that none is left untrusted.
- [ ] *test connection* shows the API user, Icinga's version and the permissions. Under *client* it lists what the user lacks: `actions/execute-command` (opt-in, left out on purpose) and, with the read-only user, exactly the `actions/*` permissions you left out, nothing else.
- [ ] *connect*.

## 4. What normal looks like

- **One lean load, then the stream.** A thin progress bar under the header while hosts, services and problem details load (a few seconds at 30 000 services), then the footer turns green: `● prod master-01 0s ⌄` (the environment, the node icygui is connected to, the age of the last event). From then on icygui only listens to the event stream; while the window is open the age stays at a few seconds because check results keep arriving.
- **Gentle on the master.** One event stream per laptop and environment (about 75 KB/s at 30 000 services while you look at it, under 1 KB/s in quiet mode); a status query every 30 seconds (every 5 minutes in quiet mode) and one small request for the cluster nodes' states (with every status query for the environment on screen, every 5 minutes for the others); a lean reconcile at an interval that grows with the installation (5 minutes for a small one, about 15 at 30 000 services, up to 60 while the stream runs without a break, at least 30 in quiet mode). No polling, no periodic full reloads. Icinga keeps the memory it allocated for a large API answer, so a one-time rise after the first load is normal; a steady climb isn't.
- **Quiet mode** (Settings → general → *quiet mode when hidden*, on by default). Half a minute after you close the window to the tray or menu bar, minimise it or move it to another desktop, and for every environment that isn't on screen, icygui switches to an event stream without check results. On the master that shows as the event stream reconnecting (a new `/v1/events` connection, briefly two) whenever you hide or show the window or switch environments, and that is expected. What changes for you:
  - Notifications still come at once: state changes, acknowledgements, downtimes and Icinga's notifications are in the quiet stream.
  - Stale while quiet: check outputs that didn't change the state, last-check times and the *late* markers. The switcher shows `quiet` instead of the age for such an environment, with a green dot.
  - Coming back (opening the window, switching environments) counts as live at once: the footer shows the age, the full stream returns within a moment, and the outputs on screen refresh first. A pane whose details take longer than about 300 ms says *updating…* in its header until they are in.
- **No notification flood.** Problems that already exist when icygui connects don't notify; only new ones do, with the default rule (hard critical, unknown and down, recoveries of notified problems, handled ones skipped).
- **Counts match Icinga Web.** The *problems* dashboard should show the same unhandled service problems as Icinga Web's problem view; *host problems* the same hosts.

## 5. What to look at

- [ ] Open a few problems: the service pane's output, performance data, check details, comments and the *notified* row (whom Icinga notified and when).
- [ ] Open a host: its services, vars and config tabs, parents and children.
- [ ] Build a dashboard for your team with a filter you know from Icinga Web or `assign where` (<kbd>⌘N</kbd> / <kbd>Ctrl N</kbd>); compare its count with Icinga Web.
- [ ] Leave it running for a shift. Close the window: it should keep running in the tray or menu bar, go quiet after half a minute, and still notify at once.
- [ ] After a while with the window closed, click a notification: the window opens with the object's pane complete (output and details), in a dashboard that lists it. Then look at the rows on screen: their outputs and last-check times should be current within a moment, and an *updating…* hint should not stay.
- [ ] **On a Mac, look closely:** the menu-bar icon, notifications, launch at login, keychain access and quiet mode have only been tested on Linux so far. Check that the menu-bar icon appears and its menu works after closing the window, that macOS asks to allow notifications and a notification's *Acknowledge* and *Open* buttons work, that *start at login* starts icygui in the menu bar, that the keychain asks once per environment and not again after an update, and that a minimised window (and one on another Space) goes quiet after half a minute while a window on a second screen stays live. The full list is in [spikes.md](spikes.md#still-to-run-on-a-mac); report what you find.
- [ ] Once you've added the `actions/*` permissions: acknowledge one real problem you'd acknowledge anyway, and check it in Icinga Web (author, comment, no notification sent by Icinga for it).

## 6. What to report, and where

Write down, for anything odd: what you did, what you expected, what happened, and the time.

- **The log file:** `~/.local/state/icygui/logs/icygui.log` (Linux) or `~/Library/Logs/io.github.alexykn.icygui/icygui.log` (macOS). It contains host and service names, never passwords. For more detail, start icygui with `RUST_LOG=info,icygui=debug,ic_core=debug,ic_api=debug` (start the list with `info`, or every other part's warnings and errors are left out).
- `icygui --version`, your OS, and Icinga's version (from *test connection*).
- Especially interesting: counts that differ from Icinga Web, rows marked *late* that aren't (with the window open: quiet mode leaves late markers stale while hidden), a yellow footer while Icinga is healthy, reconnects other than the stream's switch when you hide or show the window or switch environments, slow loads (how long until the lists were complete), memory use of the icygui process after a few hours, and notifications you expected but didn't get (or got but didn't want), or that came late while the window was closed.
- Quiet mode: outputs that are still stale a minute after you opened the window, an *updating…* hint that stays, a notification click that opened an incomplete pane or a tab instead of a dashboard listing the object, and on a Mac whether a window that other windows cover completely went quiet (the log says `quiet mode … quiet=true`) and whether that bothered you.
- Screenshots help; they may contain internal names, so share them like the log.

If anything misbehaves, quitting icygui (<kbd>⌘Q</kbd> / <kbd>Ctrl Q</kbd>, or *Quit* in the tray) closes its connection immediately. Removing the `ApiUser` from Icinga cuts it off for good.
