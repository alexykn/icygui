# Findings ledger

Every confirmed review finding, and every rejected one, from 2026-10-10 on, tagged with the rule it broke. It feeds the invariants catalogue for the pre-rc behaviour hardening (PLAN.md 4.2, "Pre-rc behaviour hardening"): the catalogue grows from what actually broke, and findings that fit no rule show where a rule is missing.

**How to add an entry:** one row per finding, newest at the bottom. Use one or more tags from the list below. When no tag fits, write `untagged` and propose a new tag in the notes; untagged rows are the gaps the brainstorm looks at first.

Columns:
- **Date**, and **Where**: stage, part and commit.
- **Finding**: one line.
- **Sev**: major or minor.
- **Outcome**: fixed (commit), or rejected (with the reason).
- **Tags**.
- **Found by**: review, probe test, running app, screenshot, CI, user.

## Tags (provisional, until the catalogue replaces them)

| Tag | Rule |
|---|---|
| `no-false-green` | The UI never looks better than reality: summaries are the worst of their parts, ages come from timestamps on the UI clock, and silence or a dead stream never stays green. |
| `one-cause-one-alert` | One cause raises one alert and one notification, never two. |
| `nothing-silent` | Whatever stops reporting or disappears is shown, never just dropped. |
| `nothing-moves` | Nothing moves when state changes: fixed slots. |
| `keyboard-reach` | Every page, link and action can be reached and used by keyboard. |
| `link-does-something` | Every control visibly does what it says, in every state. |
| `request-economy` | No needless requests to Icinga; failures back off; refused or unsupported things aren't asked again. |
| `no-load-on-real` | No load or contract tests against real masters or workers. |
| `failure-visible` | Every failure path ends in a visible state and a log line at the right level. |
| `honest-docs` | Docs and comments describe what the code does. |
| `one-rule-one-place` | One rule lives in one function; no second copy that can drift. |
| `demo-shows-it` | The demo shows every feature in a believable setup. |
| `platform-parity` | Behaviour matches across Linux, macOS and Windows, or the difference is documented. |
| `no-secrets` | No secrets in config, logs or commits. |
| `mock-drift` | The mock behaves like real Icinga; behaviour not checked against a real Icinga is suspect. |
| `matches-mockup` | The built screen matches the approved mock-up (layout, sizes, words), or the difference is decided and recorded. |

## Entries

| Date | Where | Finding | Sev | Outcome | Tags | Found by |
|---|---|---|---|---|---|---|
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | False green: health page stayed green (endpoints, health line, beats 'on time' 15 intervals overdue) without live data | major | fixed: grey dots, 'as of', UI-clock ages, overdue beats late in 5bc9dee; the endpoint statuses (last known, 'as of', this node 'connection lost') fully fixed in the stage 4 follow-up (row below) | `no-false-green` | review (probe test + running app) |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Health page had no keyboard model (scroll, fold, links) | major | fixed (dashboard keys, UI test) | `keyboard-reach` | review (running app) |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Endpoint down also raised 'relay queue keeps growing': two always-on alerts for one cause | major | fixed: Docker Icinga 2.15.6 showed the relay queue stays flat; ic-mock was wrong | `one-cause-one-alert` `mock-drift` | review (running app log) |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | HA zone with one endpoint down shown red (page) while the alert said warning | minor | fixed: yellow when degraded, red when cut off or its beat is dead; sidebar follows | `no-false-green` `one-rule-one-place` | review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Docs and a recorded 'deviation' described a time budget the code doesn't use (code implements B2) | minor | fixed in the docs | `honest-docs` | review (code reading) |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Tray menu said 'live' while the footer was already yellow (stale) | minor | fixed ('no events Nm') | `no-false-green` | review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | 'show master-02' link did nothing when the view was already open | minor | fixed (scrolls, cursor on the row) | `link-does-something` | review + design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Heartbeat row status had no fixed slot, the age moved | minor | fixed (18 + 8 character slots) | `nothing-moves` | review (code) |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Failed features read retried every poll; refused types re-asked every 5 min | minor | fixed (refused remembered, transient errors keep the pace) | `request-economy` | review (code) |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Demo had no HA satellite zone: pinned satellite beats and 16s/16t/16a2 never shown | minor | fixed (sat-fra-02, new faults) | `demo-shows-it` | review + design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | User guide claimed a work-queue alert; stale architecture block; wrong footer comment | minor | fixed | `honest-docs` | review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Report.state/verdict: a second copy of the sidebar's rules read only by tests | minor | fixed (removed) | `one-rule-one-place` | review (code) |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Footer '+' moves when an environment goes blind (primed gap) | minor | rejected: doesn't reproduce in today's footer; recorded for stage 5's full-width bar | `nothing-moves` | review (screenshots) |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | No look at the tray's blind icon (Xvfb has no tray host) | minor | fixed: ignored test writes every tray look to PNG; checked against 16e | untagged (proposed: `visually-checked`) | review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | macOS: 'persistent' acts like 'notify' without telling the user | minor | fixed (guide sentence + macOS tooltip) | `platform-parity` | review + design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Demo: a cut-off zone's heartbeat kept beating, so 'zone cut off' could never show | major | fixed in ic-mock (zone checker picks a connected endpoint) | `no-false-green` `mock-drift` `demo-shows-it` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | 'Icinga runs no checks' alert quoted numbers the page contradicted | major | fixed (says only what the polls read; mock's stop_checks now stops results) | `one-rule-one-place` `mock-drift` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Trouble-alerts settings rows taller than 16b/16b2, '+ add' too large, disappeared text misplaced | minor | fixed | `matches-mockup` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Health editor cut view names that fit in 16k | minor | fixed | `matches-mockup` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Health page ··· menu lacks 'open as tab' (16j) | minor | rejected: topic 06 decided the health page is not a tab | `matches-mockup` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | For the first 2 minutes the page was red with nothing explaining why | minor | fixed (alert shown at once; the grace holds only the notification and log line) | `failure-visible` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Blind footer left a hole and dropped the node | minor | fixed | `matches-mockup` `nothing-moves` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Blind banner link 'Retry now' instead of 'retry now' | minor | fixed (all notice actions lowercase) | `matches-mockup` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Tray menu check-mark column differed from 16e | minor | fixed | `matches-mockup` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Zone band name 7px and dot 2-3px right of the mock | minor | fixed | `matches-mockup` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | Design review: keep the single lateness number (gap 3) | minor | rejected: the code implements B2 with latency and delivery apart | `honest-docs` | design review |
| 2026-10-10 | stage 4 health+heartbeat (5bc9dee) | One-view downtimes header cut its own title | minor | fixed (PaneHeader title keeps its width) | `matches-mockup` | fixer (final screenshots) |
| 2026-10-10 | stage 4 follow-up | Health page without live data: endpoint statuses still read current ('connected', normal tone, no marker), master-01 'connected · this node' while icygui's connection to it was lost; the checks view said 'last minute' and uptime kept counting | major | fixed: other statuses faint with '· as of 09:14' (a problem keeps its tone; in a short column the status keeps its first word, then the marker gives way), this node 'connection lost · this node' (or 'no live data · this node' while connected but silent) in the banner's tone, the tile and switch views say 'as of', uptime stops at the last status, and an old status counts as not current before the first endpoints poll too (right after a reconnect); unit and UI tests | `no-false-green` `failure-visible` | orchestrator (final screenshots) |
| 2026-10-10 | stage 4 follow-up | Outage gets milder over time: banner, footer and row red for the first 2 minutes ('connection lost'), then yellow once 'no live data' is raised | minor | open: question to the user (changes the approved 16d) | `no-false-green` | follow-up fixer (5f0945d) |
| 2026-10-10 | stage 4 follow-up | Sidebar health dot yellow while the banner and footer are red (first 2 minutes of an outage) | minor | open: next fixer (a summary is the worst of its parts) | `no-false-green` `one-rule-one-place` | follow-up fixer (5f0945d) |
| 2026-10-10 | stage 4 follow-up | Health line 'as of' always yellow, even while the banner is red | minor | open: next fixer | `one-rule-one-place` | follow-up fixer (5f0945d) |
| 2026-10-10 | stage 4 follow-up | Zones header count '● 5' marked only by its grey dot, no 'as of' | minor | open: next fixer | `no-false-green` | follow-up fixer (5f0945d) |
