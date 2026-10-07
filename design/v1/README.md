# icygui v1 mock-ups

HTML/CSS mock-ups for every visual change in v1 (PLAN.md §4.2), in the medium
of the original design session (`design/project/`) and drawn to look like the
rc1 app as built (`docs/screenshots/`, `crates/ic-ui-kit`). Nothing here is
built until the user approves its mock-up.

## Files

- `v1.css`: the tokens of `crates/ic-ui-kit/src/theme.rs` as CSS variables
  (dark, and the proposed light theme), and the ic-ui-kit components as classes.
  Values that v1 would add to the theme are marked `NEW`.
- `v1.js`: the app's building blocks as HTML helpers (sidebar, list rows, pane
  title, buttons, menus, dialogs, toasts), named after ic-ui-kit. Every topic
  draws its chrome through them, so the frames stay consistent.
- `icons.js`: the Lucide icons the app uses (gpui-component's set), inline.
- `NN-topic.html`: one page per topic, each a column of 1440×900 frames (the
  app's default window) with a label. Open a page in a browser to view it.
- `00-baseline.html`: rc1's service pane redrawn with the kit, as a check
  that the kit matches the real app. It is not a proposal.
- `render.js`: renders every frame to PNG at 2x (`[data-shot]`, plus crops in
  `data-crops`):
  `NODE_PATH=…/node_modules node design/v1/render.js OUT_DIR 01-downtimes.html`
  (needs `playwright-core` and a Chromium; `PLAYWRIGHT_BROWSERS_PATH` or
  `CHROMIUM`).

The wall clock in every frame is Wednesday 7 October 2026, 14:12. The data is
the demo's prod-cluster.

---

## 01 Downtimes, very visible in the panes

**Shows** (`01-downtimes.html`): 1a a service in a fixed downtime (variant A);
1b the same with variant B; 1c a service in downtime with its host;
1d a flexible downtime that hasn't started; 1e a downtime scheduled for later
on an unhandled problem; 1f a host pane with three downtimes; 1g *remove
downtime* for a downtime set on the host; 1h the list's hint.

**Decisions**

- **Variant A (recommended): a banner under the pane header.** This is the
  existing `Banner` component in the Info tone (tint at 8 %, a 2px tone bar),
  with three lines and a `ProgressBar` (2px) on its bottom edge:
  1. icon `calendar-clock`, what (`In downtime`, `In downtime with its host`,
     `Flexible downtime`, `Downtime scheduled`), how long is left or when it
     starts (accent when in effect), and *remove downtime* on the right;
  2. the facts: fixed or flexible (a flexible one: `lasts 2h from the first
     problem` and its window), the window `13:00 → 16:00 today`, and for a
     host downtime the host as a link and `host and all its services`;
  3. author, time and the comment, at most two lines (the full text is in
     the history and the tooltip);
  4. only when there are more downtimes on the object: `+ 2 more below ·
     tonight 22:00, flexible · Sat 06:00, from config`.

  The banner sits between the pane header and the scrolling body, so it stays
  in view while the body scrolls. It replaces today's downtime note in the
  comments (and hides the downtime's automatic `↓` comment, which repeats it).
- **Variant B: the pane header becomes the downtime strip**: one line (`in
  downtime 1h 48m left until 16:00`), tinted, with the progress line as its
  bottom rule, and a `downtime` section first in the body with the details and
  *remove downtime*. Nothing moves when a downtime starts, because the header
  is always there, but there is less to read at a glance.
- **Tone:** accent (blue) means the downtime is in effect and the problem is
  handled. Grey means scheduled but not in effect yet (flexible and waiting, or
  in the future), so the problem still counts as unhandled. The state circle
  keeps its job: hollow while handled, as in rc1.
- **Several downtimes:** the banner shows the one in effect (else the next to
  start). The others are listed in an `other downtimes` section (host pane:
  above the services; service pane: where comments are), each with its
  window, fixed or flexible, status, author and comment, and a remove `×`
  in a fixed slot that shows on hover. A downtime from the config
  (`ScheduledDowntime`, `config_owned`) shows a lock instead of the calendar.
- **Remove:** *remove downtime* always opens a confirmation listing every
  downtime it removes (the box scrolls; the rule is that the operator sees
  every target). For a downtime set on the host with all services, a
  segmented choice picks `this service only` or `the host and its 23
  services`. The danger button counts what it removes. It also says which
  problems will notify again once their downtimes are gone.
- **List hint (1h):** no new colour or column. The row keeps the hollow
  circle and the tag in faint text, which now says more: `downtime 1h 48m`
  (time left), `host downtime 1h 18m`, `downtime, flexible 1h 12m`, and on an
  unhandled problem with a later downtime, `downtime at 22:00`. Same slot as
  `ack m.keller`, so nothing moves.

**Open questions**

1. Variant A or B? A is recommended for visibility. Its cost: when a downtime
   starts while the pane is open, the banner slides in and the body moves down
   once (a real change of state, not hover or progress).
2. History lines draw downtimes in the unknown purple (the turn-1 event
   stream). Should they switch to the accent, to match the banner?
3. A host in downtime *without* all services: should its services' panes show
   a grey `host in downtime` banner even though Icinga doesn't count them as
   handled? (Not drawn: 1c shows the common case, a host downtime with all
   services.)
4. New theme values: `accent-tint` (accent at 8 %) for the banner. The
   Banner component already derives it from its tone, so it may not need a
   token.

---

## 02 Settings window, in the style of Zed's

**Shows** (`02-settings.html`, `settings.js`): 2a the window in context, over
the main window; 2b general; 2c appearance; 2d notifications (top); 2e
notifications (scrolled); 2f icinga; 2g keymap; 2h advanced; 2i a search for
"quiet".

**Decisions**

- **A window of its own**, 1080×760, opened with ctrl-, (⌘, on macOS), the app
  menu or the palette; the main window stays usable beside it. It replaces the
  700px modal dialog.
- **Left: navigation.** The window's top bar holds the traffic lights and a
  search field (the main window's sidebar top, repeated). Below it come the
  categories as sidebar-style rows (30px, an icon in the fixed mark slot, the
  selected one with the sidebar's active background). Under the open category
  are its sections on a guide line; the section in view is marked in the
  accent colour. At the bottom: `focus navbar ctrl-shift-e`.
- **Right: the page.** A 40px header bar (the page name like a dashboard
  title, a faint scope such as `for prod-cluster · on this computer only`, the
  `✓ saved` status and *edit in settings file*). Below it, section labels and
  one row per setting: name (13px), a one-line description (12px muted,
  truncated rather than wrapped), and the control right-aligned. Rows are
  split by the list's row rules. Controls are today's `Switch` (without its
  label), `Segmented`, `Chip`, the bordered `TextField`, and a dropdown in the
  editor's style. Rows that depend on a switch above them are indented by
  20px.
- **Apply at once, Zed's model:** each change is written to settings.toml
  straight away. Text fields apply on Enter or blur; a bad value shows its
  problem under the row (critical text) and isn't applied. So the window has
  no save or cancel. The header's `✓ saved` confirms the write. (The demo
  shows `the demo saves nothing` there instead.)
- **Categories and contents** are the ones the user confirmed. Notifications
  starts with an `environment` dropdown, because rules are per environment
  (today's dialog uses the active one). The new switch *show plugin output*
  sits with the master switch. Icinga lists the environments with their health
  dot, a connection summary and a gear that opens today's environment editor.
  Keymap is a read-only, filterable table with *edit keymap file* in the
  header. Advanced holds the log level, *open folder* for logs and config, and
  about.
- **Appearance** has a live preview of the databases dashboard in the chosen
  density and time format. Compact rows are 32px plus the rule, with a 14px
  circle and the time at the right, and no output line.
- **Search** filters across categories: the nav dims the categories without a
  match and counts the matches of the others. The page lists the matching
  rows under `category · section` headings, with the match in the accent
  colour (as in the palette). The rows work in place. A section name that
  matches brings its whole section (`quiet hours`).

**Open questions**

1. Apply at once (drawn) or keep today's draft with save and cancel?
2. Key hints are drawn in Linux notation, like the docs screenshots (ctrl-,);
   macOS shows ⌘, and ⌘⇧E.
3. Interface size steps: 90 / 100 / 115 %?
4. New icons for the nav: `sun-moon`, `server`, `keyboard`, `wrench`,
   `file-code`, `folder-open` (all Lucide, from the set gpui-component
   ships).

---

## 03 Light theme

**Shows** (`03-light-theme.html`): 3a the main window (sidebar, overview
dashboard, service pane, footer with an unread badge); 3b every row state,
marked rows with the selection bar, and topic 01's downtime banner in a host
pane; 3c the palette over the dimmed window; 3d Settings → appearance with
*light* chosen; 3e a component sheet, dark beside light.

**Decisions**

- `Theme::light()` has the same fields as `Theme::dark()`. Every value is in
  `v1.css` under `.light`. Its structure copies dark: the window and sidebar
  share one surface (`#ffffff`), the pane is a band off it (`#f7f8f9`), and
  code blocks a band further (`#f2f4f6`). Rules are light greys
  (`#d5d9dd` window, `#dfe2e6` splits, `#e3e6e9` headers, `#eef0f2` rows).
- **Text keeps the dark theme's contrast steps:** muted `#687077` is about
  5:1 and faint `#899097` about 3.2:1 on white, like `#8b9094` and `#6c7175`
  on `#1d2125`. Strong and body text are near-black greys, never pure black.
- **State colours are the same hues, darker,** at about 4.3:1 or more on
  white, so `CRIT`, `late 3m` and perfdata values stay readable as text: ok
  `#2a8a4a`, warning `#b07408`, critical `#cf4646`, unknown `#8a5cc8`,
  pending `#c5cacf`, accent `#2f74c0` (on-accent text white).
- **Selection** is tinted towards the accent, as in dark: selected row
  `#e3ebf4`, marked row `#dbe8f7` with the 2px accent bar, hover `#f4f6f8`.
- **Shadows and backdrop** are softer: the modal backdrop is a light grey veil
  (`rgba(30,36,42,.28)`) instead of near-black. The traffic lights keep their
  colours.
- *follow system* (the default) switches live with the desktop's appearance.
  The tray icon follows the desktop, not this setting.

**Open questions**

1. Warning `#b07408` reads as dark amber. A brighter amber (`#c98a12`) looks
   friendlier as a filled circle but drops to 2.9:1 as text. Should there be
   a separate fill shade per state (circles) and text shade (labels,
   perfdata)?
2. Should light also get a slightly grey sidebar (Zed's light themes do), or
   keep one surface as in dark (drawn)?

---

## 04 Multi-view dashboards

**Shows** (`04-multi-view.html`, `views.js`): 4a the databases dashboard
with four views (summary tiles, a list, an empty view, an event stream);
4b a collapsed view, the cursor in the stream, the pane open and a view's own
sort menu; 4c the editor managing views; 4d *add view*; 4e a view's `···`
and the settings of an event stream view.

**Decisions**

- **A dashboard is a list of views stacked on one page,** which scrolls as a
  whole (each view sizes to its content; a long list virtualises inside the
  page's scroll). A single-view dashboard stays exactly as in rc1 (header and
  summary bar), so nothing changes for existing dashboards.
- **The dashboard header** keeps the title. Its subtitle says `4 views`, and
  sort moves into the views. With several views there is no summary bar:
  each view header carries its own counts.
- **The view header** is 36px (the summary bar's height) on the pane surface
  (`pane_background`), so it reads as a band between views and differs from
  a grouped list's group-header rows (`row_header`, darker, row height). In
  order: a collapse chevron, the display's icon (in the mark slot), the name
  (13px medium), the filter (faint, cut off first when narrow), the counts
  (state dot and number), `live` for a stream, the view's own sort, and `···`.
- **Empty view:** only the header, with `nothing to show` in place of the
  counts. There is no body and no empty box.
- **Keyboard:** one cursor for the whole page. j/k move through the rows and
  continue into the next view (they skip collapsed views); Tab and shift-Tab
  jump to the first row of the next or previous view. The view holding the
  cursor has a 2px accent bar on its header and its name in strong text. On a
  header, ←/→ collapse and expand it. ctrl-a marks the rows of the focused
  view only. Enter on an event opens its object in the pane.
- **Summary tiles:** one tile per host group (or custom var value). Each
  tile has the worst state's dot in the mark slot, the name, the host count,
  a stacked 6px bar and the counts in state colours. A click filters the page
  to that group (topic 05 shows the same on the grid).
- **Event stream:** the history tab's line format (time, dot, KIND in the
  state colour, `service on host`, the note) from the local event log,
  filtered by the view's filter and newest first, with `N lines` before it
  scrolls. Acks and downtimes are drawn in the accent colour (see topic 01,
  open question 2).
- **Editor:** the inspector (372px, as today) gets a `views` list. Each row
  has a drag handle, the display icon, the name, what it matches, and `···`
  (move up/down with alt-↑↓, duplicate, collapse by default, remove).
  `+ add view` asks for the display first (list, grouped list, host-group
  grid, summary tiles, event stream). Below a rule come the selected view's
  settings, which depend on its display. The preview shows the whole
  dashboard; the selected view has an accent ring, and clicking a view there
  selects it in the inspector. Removing a view needs no confirmation (the
  editor's *discard* undoes it).

**Open questions**

1. Should the dashboard keep a summary bar with totals across all views,
   counting each object once, above the first view?
2. Should notifications (and the sidebar's count and dot) count the
   dashboard's list views only (drawn), or also tiles, grid and stream?
3. ←/→ on a view header to collapse: or should space toggle it?

---

## 05 Host-group grid

**Shows** (`05-hostgroup-grid.html`): 5a the grid as a view above a list
(variant A, squares); 5b hover, the keyboard cursor and the host pane;
5c a click on a group filters the page; 5d variant B, labelled cells; 5e the
view's settings in the editor.

**Decisions**

- **Variant A (recommended): a square per host,** 12px with 3px gaps, grouped
  by host group in a grid of columns (three at full width, two beside the
  pane), worst groups first. Colour = the host's worst state, its own or its
  worst service's. **Healthy hosts are dim green** (ok at about 32 %), so 118
  healthy hosts don't drown the 6 that matter. Problems are full colour;
  handled problems are hollow (2px inset ring), like the list's circles;
  pending is the pending grey.
- **Group header:** the worst unhandled state's dot (mark slot), the name,
  the host count (faint), and how many hosts are in each problem state (dot
  and number, in the summary bar's order: critical, warning, unknown).
- **Interaction:** hover shows a tooltip (host, state, problem count, worst
  service and its output's first line). Arrows move a cursor (2px accent
  outline) and Enter or a click opens the host pane. A click on a group's
  name filters the whole page to that group: the group gets an accent ring,
  the others dim to 35 %, the other views show only that group, and the
  dashboard header shows a `host group edge-ams ×` chip (the filled chip
  style). The filter is temporary: Esc or × clears it, and it is never saved.
- **Variant B: labelled cells,** one per host (150px+, its state dot and name;
  a problem cell is filled and names its worst service, cut off with an
  ellipsis). It suits small groups and wastes space on large ones, so it is
  an option of the same view (`hosts as: squares | labelled cells`), not a
  separate display.
- **Settings:** group by host group (all, or picked ones) or by a custom var
  (`host.vars.site`), an optional filter, colour by `worst of host and
  services` or `host only`, squares or cells, `hide groups where every host
  is ok`, and whether a host in several groups shows in each.

**Open questions**

1. Dim green for healthy hosts (drawn), or full green like Icinga Web's grid?
2. Should a host in downtime show hollow even when it is OK? (Drawn: only
   problems are hollow, as in the list.)

---

## 06 Cluster health

**Shows** (`06-cluster-health.html`): 6a the entry in the footer switcher;
6b the page, healthy, as a tab; 6c the page with a satellite gone.

**Decisions**

- **Where it lives:** a `cluster health` row under the nodes in the footer
  switcher (a heart-pulse icon in the mark slot, a faint `zones, queues,
  checks/min` hint), and *cluster health* in the palette. Either opens the
  page as a tab in the sidebar's `open` section, named `cluster health ·
  prod-cluster`, with the environment's worst health as its dot. It is a
  page, not a modal, so it can stay open on a second screen.
- **Header:** `cluster health · prod-cluster · seen from master-01`, then
  `updated 12s ago · every 30s`. The summary bar counts connected and
  disconnected endpoints and ends with Icinga's version and uptime.
- **Zones and endpoints:** a table with zones as group-header bands (the
  worst endpoint's dot, the name in semibold, its parent, endpoint and host
  counts) and their endpoints under them. Columns: state dot, endpoint, zone,
  version, last message, messages in and out per second, status. The node
  icygui is connected to has the selected-row background and `this node`.
  Global zones are a faint line at the end (config only, no endpoints). A
  version older than the masters' says so (`older version`).
- **Numbers from /v1/status** as stat tiles (label, value, what it counts, a
  12-point trend in the faint colour with the latest point in the accent).
  Checks: active and passive checks per minute, average latency, average
  execution time, pending, late (from icygui's own late-check tracking). Queues
  and connections: API work queue, relay queue, cluster connections, HTTP
  clients, uptime. A value turns warning or critical only when it is wrong,
  always next to its label. **IcingaDB** gets a tile (connected, pending
  items) only when Icinga reports the IcingaDB feature as enabled. icygui
  never uses IcingaDB, and without it uptime takes the slot (drawn: the
  user's cluster doesn't run it).
- **Icinga's global switches** (`enable_notifications`, active checks, event
  handlers, flap detection, performance data) are read-only (D6): green
  `on`, or warning `off`. A globally disabled switch is one of the first
  things to check when monitoring seems quiet.
- **Degraded (6c):** the endpoint and its zone turn critical. A critical
  banner (today's Banner) says what it means for monitoring (`zone fra's
  results are stale · 1,204 checks late · relay queue growing`), with a link
  to the late checks, and the affected tiles colour themselves. The tab's dot
  and the switcher's node dot follow.
- **Cost:** none while the page is closed. It reads the 30-second status poll
  and the endpoints and zones icygui already loads; the trends are 30 minutes
  of those polls, kept in memory only (lost on restart). In quiet mode the
  poll runs every 5 minutes, and the page says `quiet: every 5 min`.

**Open questions**

1. Should the trend lines exist at all (in-memory history, lost on restart),
   or should the tiles show values only?
2. Messages per second and last message come from the endpoint objects on the
   connected node. Is that enough, or should a node's own view (its
   `/v1/status`) be fetched on demand when its row is clicked?
3. Should it be per environment only, or also have an `all environments`
   overview?

---

## 07 Lists of every comment and downtime

**Shows** (`07-comments-downtimes-lists.html`): 7a every downtime; 7b three
marked, with the selection bar and the pane; 7c the bulk removal
confirmation; 7d every comment, four marked, with the removal dialog
(an acknowledgement skipped); 7e a dashboard view that lists downtimes.

**Decisions**

- **Two lists across all objects,** opened from the palette (`downtimes`,
  `comments`) as tabs in the sidebar's `open` section, with an icon in the
  mark slot and a count. The data is already loaded (comments and downtimes
  are part of rc1's object store), so they cost nothing extra.
- **Rows are today's list rows.** The leading circle is the *object's*
  state (hollow while the downtime handles a problem), its caption the time in
  state or `host`. The title reads `service on host`, or the host name. The
  second line holds the window and the author's comment (`fixed · 13:00 → 16:00
  · j.berg: …`) or, for comments, `author time · text`. The tag holds a 56px
  progress line (accent) and the time left (`48m left` in the accent), or
  `not started`, `in 7h 48m`, `by 22:00`, `from config` with a lock.
- **No duplicate rows:** a host downtime with `all_services` is one row
  (`sw-core-ams-02 + 18 services`); → or a click unfolds its service
  downtimes, indented. The summary bar says how many are folded.
- **Summary bars:** downtimes count `in effect` (accent dot), `scheduled`
  (pending dot) and `from config` (lock). Comments count comments and
  acknowledgements. Downtime and flapping comments are hidden from the
  comment list, because they repeat the downtime list (the end of the
  summary bar says so; a click shows them).
- **Selection** works as in every list (x, shift-click, ctrl-click, ctrl-a,
  shift-j/k). The selection bar keeps its place and look, with this list's
  actions: `remove downtimes ⌫` or `remove comments ⌫` (primary), `copy
  names`, and `···` (copy filter expression).
- **Bulk removal always confirms,** listing every downtime it removes,
  grouped by downtime (a scrolling box; for a host downtime, the host and each
  service). It names the problems that will notify again, and counts what it
  removes on the danger button. What it can't remove is listed as skipped,
  with the reason: acknowledgement comments (remove the acknowledgement) and
  config downtimes (they return on the next reload; they are skipped unless
  the user ticks them explicitly).
- **As a view:** a dashboard view's `lists` gains `downtimes` and
  `comments`, and the filter knows `downtime.*` and `comment.*` beside the
  object's `host.*` and `service.*` ("my downtimes", "comments today").

**Open questions**

1. Should config downtimes (`ScheduledDowntime`) be removable at all from
   the list? Icinga recreates them, so the drawn choice is to skip them.
2. Is sorting by `ends soonest` (in effect, then scheduled by start) the right
   default, or newest first?

---

## 08 Sharing dashboards as YAML through the clipboard

**Shows** (`08-yaml-sharing.html`): 8a *copy as YAML* in the dashboard's
`···`; 8b *copy group as YAML* in the group's `···` and the toast; 8c the
palette; 8d *paste dashboards* in another icygui (staging); 8e the import
preview with clashes; 8f a broken paste.

**Decisions**

- **Copy:** `copy as YAML` (ctrl-shift-c on the selected dashboard) after
  `duplicate` in the dashboard menu, with `export to file` under it. In the
  group menu, `copy group as YAML` above `export group to file` (today's
  `export group`). The palette has `Copy as YAML · <dashboard>` (state dot),
  `Copy group as YAML · <group>` (folder icon), `Paste dashboards` and
  `Export all dashboards`, found by "yaml", "copy" or "share".
- **Toast** (OK tone, check): `Copied overview as YAML`, `4 dashboards · 7
  views · 112 lines`, `Paste with ctrl-v on a sidebar.` The clipboard is the
  only place the text goes.
- **Format:** `format: icygui-dashboards`, `version: 2`, a comment saying
  where and when it was copied, then `groups → dashboards → views`. Each view
  carries its display, lists, filter (folded `>-` when long), sort and the
  display's own settings; dashboards carry their notification setting.
  Environment names, ids and secrets are never in it. The exported file uses
  the same YAML; rc1's TOML exports still import.
- **Paste:** ctrl-v while the sidebar has the keyboard, `paste dashboards` in
  the footer's + menu, or the palette. It **always opens the preview**, never
  imports straight away.
- **Import preview** (a 1040px dialog): on the left, what the YAML holds as a
  tree (groups with a folder icon, dashboards with a dot and view count),
  each with its status on the right: `new` (OK colour), `exists here ·
  merge`, `name taken · keep both` (warning colour, with the chosen action
  faint). The selected row opens a detail with the choice (segmented: replace
  the one here, keep both, skip; for a group: merge, new group), what it
  leads to (`imported as databases 2`), and what doesn't fit this
  environment (host groups that don't exist here). Below: the format check
  (`version 2 · all 7 filters parse`) and a switch for whether notification
  settings come along. On the right, the YAML read-only, with the selected
  dashboard's lines marked in the accent. The button counts what will be
  imported (`import 3 dashboards`); undo with ctrl-z.
- **Broken paste (8f):** the dialog shows the YAML around the problem, with
  the line marked critical and the character underlined (the filter field's
  error style from topic 09), plus `line 10, column 42: …` and where it is
  (`platform / certificates, the view's filter`). It imports nothing. A
  newer format version says *update icygui to import this*. Text that isn't
  icygui YAML at all only gets a toast.

**Open questions**

1. Defaults for clashes: `keep both` for dashboards and `merge` for groups
   (drawn), or ask with nothing preselected?
2. Should notification settings come along by default (drawn: on), given
   that a colleague's notification habits may differ?
3. ctrl-shift-c for copy-as-YAML: or only menus and the palette?
