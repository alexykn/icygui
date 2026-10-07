# icygui v1 mock-ups: handoff

HTML/CSS mock-ups for every visual change in v1 (PLAN.md §4.2), in the medium
of the original design session (`design/project/`) and drawn to look like the
rc1 app as built (`docs/screenshots/`, `crates/ic-ui-kit`).

**Status.** The user reviewed every topic on 2026-10-07 (PLAN.md §4.2,
*Mock-up review*) and approved all of them, some with revisions. The
revisions are drawn here and the user accepted them the same day. Each section
below gives the topic's status, what its frames show and the decisions to
build. Where a decision is in this file and not drawn, the section says so.
The open questions from the review are settled; nothing here waits for an
answer.

## Files

- `v1.css`: the tokens of `crates/ic-ui-kit/src/theme.rs` as CSS variables
  (dark, and the light theme of topic 03), and the ic-ui-kit components as
  classes. Values that v1 adds to the theme are marked `NEW`.
- `v1.js`: the app's building blocks as HTML helpers (sidebar, list rows, pane
  title, buttons, menus, dialogs, toasts), named after ic-ui-kit. Every topic
  draws its chrome through them, so the frames stay consistent.
- `icons.js`: the Lucide icons the app uses (gpui-component's set), inline.
- `settings.js` (the settings panel, topics 02, 03 and 12) and `views.js`
  (dashboard views, topics 04, 05 and 12): parts shared by several topics.
- `NN-topic.html`: one page per topic, each a column of 1440×900 frames (the
  app's default window) with a caption. Open a page in a browser to view it.
- `00-baseline.html`: rc1's service pane redrawn with the kit, as a check
  that the kit matches the real app. It is not a proposal.
- `render.js`: renders every frame to PNG at 2x (`[data-shot]`, plus crops in
  `data-crops`):
  `NODE_PATH=…/node_modules node design/v1/render.js OUT_DIR 01-downtimes.html`
  (needs `playwright-core` and a Chromium; `PLAYWRIGHT_BROWSERS_PATH` or
  `CHROMIUM`).

Topic 13 draws native Windows parts in Segoe UI; where Segoe UI isn't
installed, its frames fall back to Open Sans (the renders here used Open Sans
through a fontconfig file; `render.js` is unchanged).

The wall clock in every frame is Wednesday 7 October 2026, 14:12. The data is
the demo's prod-cluster. Rendered PNGs are named `NN-topic-x-state.png`, one
per frame, plus `-zoom` crops of the details.

| # | Topic | Page | Frames | Status |
|---|---|---|---|---|
| 01 | Downtimes in the panes | `01-downtimes.html` | 11 (+6 zooms) | approved: variant A, with revisions |
| 02 | Settings panel | `02-settings.html` | 10 | approved; handled defaults added |
| 03 | Light theme | `03-light-theme.html` | 5 | approved with revisions |
| 04 | Multi-view dashboards | `04-multi-view.html` | 5 (+1) | approved; editor selection revised |
| 05 | Host-group grid | `05-hostgroup-grid.html` | 5 (+1) | approved; editor selection revised |
| 06 | Cluster health | `06-cluster-health.html` | 3 (+1) | approved |
| 07 | Comment, downtime and acknowledged lists | `07-comments-downtimes-lists.html` | 7 | revised and approved |
| 08 | YAML sharing | `08-yaml-sharing.html` | 7 (+1) | approved with revisions |
| 09 | Filter autocomplete | `09-filter-autocomplete.html` | 7 (+7) | revised and approved |
| 10 | Palette multi-select | `10-palette-multiselect.html` | 12 (+11) | approved with two changes; combined view rebuilt, hosts page by count |
| 11 | Read-only config | `11-config-tab.html` | 4 (+4) | approved, both parts |
| 12 | Notifications: on or off, and when | `12-notification-times.html` | 7 (+4) | design approved; revised for opt-in notifications |
| 13 | Windows: installer, window, tray, toasts | `13-windows.html` | 14 (+3) | drawn; the open points decided by the coordinator, for the user's review |

---

## Rules that span the topics

These come from the review and PLAN.md §4.3, and hold in every frame.

- **Hollow circle = handled.** An object whose downtime is in effect counts
  as handled whatever its state, so every list, the host-group grid and the
  downtime list draw it as a hollow circle (or square) in its state colour: an
  OK host in downtime is a hollow green ring. Acknowledged problems are
  hollow too. A downtime that is not in effect yet keeps the filled circle.
- **Host-with-services views** (dashboards grouped by host, the palette's
  combined multi-host view, a grouped-list view on a multi-view dashboard):
  each host is a slim **group-header band** (36px, `row_header`): its state
  dot in the rows' mark column, the name, the address and status faint, and
  the per-state counts on the right. Its services are standard list rows
  under it, **with no indent**. Every host **collapses**: a chevron at the
  band's left (at the x of 04's view-header chevron), a click on it, or ←/→
  with the cursor on the band. A collapsed host keeps its band and its counts.
  Collapsing only changes what shows: summaries, counts, *mark all problems*
  and ctrl-a still cover every host, and a collapsed host with marked rows
  gets the marked tint and bar on its band, so no mark is out of sight. This
  **replaces rc1's grouped-list header** (a full-height host row with a small
  circle and indented service rows) in every host-with-services view (10h,
  10j, 10k).
- **Hosts page by count, not by state** (the rule of rc1's host pane,
  `HOST_SERVICES_PREVIEW = 7`), in every host-with-services view and in the
  host pane: a host shows up to 7 service rows, its problems first (worst
  first, never hidden, even when there are more than 7), then OK services in
  name order to fill the 7. A `+ N more` row follows (an all-OK host shows
  its first 7 and `+ 12 more`); a click, Enter or → on it shows the whole host
  in place (the list is virtualised), and the same slot then reads `− show
  fewer` (click, Enter or ←), so nothing else moves. While an expanded host
  scrolls, its band sticks to the top of the list. A click on a host band's
  name opens the host in the pane; the chevron and ←/→ collapse it. Paging
  and collapsing only change what shows (summaries, counts and *mark all
  problems* cover every service). rc1's `+ N more ok` becomes `+ N more`.
- **Handled problems: shown or hidden per kind.** Icinga's *handled* has
  three parts, and each is its own switch: hide acknowledged, hide in
  downtime, hide services of hosts that are down. The defaults are in
  Settings → appearance → *handled problems* (all on, 2c). Every list view
  follows them unless the dashboard editor sets its own (*handled: as in
  settings / show / hide*, with the kinds to hide; 4c, 9a–g). The summary
  bar keeps rc1's place for it, as a button in a fixed, right-aligned slot:
  `28 hidden · show`, and after a click `28 handled · hide`, saved with the
  view as rc1's toggle was (2j). On a multi-view dashboard (no summary bar)
  every list or grouped-list **view header** has the same button in a fixed,
  right-aligned slot after its counts (`2 hidden · show` / `2 handled ·
  hide`, 4a, 4b); a view with nothing handled keeps the slot empty, so the
  headers line up and nothing moves. Shown handled rows are hollow. Filters
  on `acknowledged` or `downtime_depth` still work; the switches apply on top.
- **The per-state counts are unhandled counts**, in the summary bar and in
  every view header: show and hide never change them (2j keeps `4 critical`
  after *show*), and they match the sidebar's counts, which are unchanged.
- **The pane's ×** sits left of `↗ open as tab`, in a fixed slot, on every
  platform, so it never sits next to a window's close button (topic 13). A
  pane that is already a tab keeps the tab slot, empty. The combined view's
  `×` (back to the dashboard) likewise sits left of `↗ pin as tab`.
- **The view selected in the dashboard editor** is marked on its header
  only: the 2px accent bar that marks the focused view on a dashboard (4b),
  plus a faint accent tint on the header (`accent-tint`, 8 % dark, 7 % light).
  Nothing is drawn around the view's body (no outline or ring).
- **Selection** is the selected-row background, never a check-mark column.
  Every list and palette row has a mark in its fixed dot slot. Object labels
  read `service on host`. The same object never gets two rows.
- **Nothing moves with state:** fixed slots for hints, badges and marks (the
  pane's `updating` hint, the sidebar's notification mark, the combined
  view's bulk bar, the pin slot). **A label that changes width gets a slot
  sized for its longest value**, and what follows it starts at a fixed x:
  the import preview's status and choice columns (08), the selection bar's
  count (`N selected`, 12ch; `N services marked`, 19ch), the palette footer's
  count, the comment list's kind and detail (07), the bulk bar's problem
  count (10). Choosing another option changes only the word in its slot.
  (rc1's selection bar has no fixed count slot yet: its buttons shift when
  the count gains a digit.)
- **No trailing "…"** on button, menu or command labels; an ellipsis only
  where text is cut off, and on progress text.
- **The operator sees every target** before anything is sent: bulk dialogs
  and removal confirmations list every object (the box scrolls) and the
  button counts them.

---

## 01 Downtimes, very visible in the panes

**Status: approved, variant A, with revisions** (the host dialog, history
colour and host-only downtimes; accepted).

**Shows** (`01-downtimes.html`): 1a a service in a fixed downtime (variant A,
chosen); 1b variant B (not chosen, kept for the record); 1c a service in
downtime with its host; 1d a flexible downtime that hasn't started; 1e a
downtime scheduled for later on an unhandled problem; 1f a host pane with
three downtimes; 1g *remove downtime* for a downtime set on the host; 1h the
list rows (hollow = handled, the tag hint); 1i the host pane's downtime
dialog with *all services*; 1j a host downtime without *all services*; 1k the
history tab.

**Decisions**

- **Variant A: a banner under the pane header,** fixed between the header and
  the scrolling body, so it stays in view while the body scrolls. It is the
  existing `Banner` in the Info tone (tint at 8 %, a 2px tone bar; the
  component derives the tint from its tone, so no new token is needed), with
  a 2px `ProgressBar` on its bottom edge showing how much of the downtime has
  passed. Lines:
  1. icon `calendar-clock`, what (`In downtime`, `In downtime with its host`,
     `Flexible downtime`, `Downtime scheduled`), how long is left or when it
     starts (accent when in effect), and *remove downtime* on the right;
  2. the facts: fixed or flexible (a flexible one: `lasts 2h from the first
     problem` and its window), the window `13:00 → 16:00 today`, and for a
     host downtime the host as a link and `host and all its services`;
  3. author, time and the comment, at most two lines (the full text is in
     the history and the tooltip);
  4. only when the object has more downtimes: `+ 2 more below · tonight
     22:00, flexible · Sat 06:00, from config`.

  It replaces today's downtime note in the comments (and hides the
  downtime's automatic comment, which repeats it). **When a downtime starts
  while the pane is open, the banner slides in and the body moves down once;
  the user accepted this one move.**
- **Tone:** accent (blue) means the downtime is in effect and the object is
  handled. Grey means scheduled but not in effect yet (flexible and waiting,
  or in the future), so a problem still counts as unhandled.
- **Several downtimes:** the banner shows the one in effect (else the next to
  start). The others are listed in an `other downtimes` section (host pane:
  above the services; service pane: where the comments are), each with its
  window, fixed or flexible, status, author and comment, and a remove `×` in
  a fixed slot that shows on hover. A downtime from the config
  (`ScheduledDowntime`, `config_owned`) shows a lock instead of the calendar.
- **Remove:** *remove downtime* always confirms, listing every downtime it
  removes (the box scrolls). For a downtime set on the host with all
  services, a segmented choice picks `this service only` or `the host and its
  23 services`. The danger button counts what it removes, and the dialog says
  which problems will notify again.
- **Host pane, *downtime* (d) (1i):** schedules the downtime on the whole
  host. Today's downtime dialog, with an **`all services` switch on by
  default** at the top, right above the target box, which lists the host and
  each of its services (it scrolls). Off, the box lists the host alone and the
  button says `schedule downtime`. The rest is today's form.
- **A host downtime without all services (1j):** as in Icinga Web: the
  services are **not** in downtime (no banner, filled circle, the problem
  still notifies). The host line under the service's name shows the host's
  downtime marker (the calendar icon in the accent, `host in downtime until
  15:00`); the host name opens the host pane with its banner.
- **History (1k):** `DOWNTIME` lines use the downtime accent (the banner's
  blue), like acknowledgements, not the unknown purple. `DOWNTIME ENDED`
  stays faint like the other "ended" lines. The same applies to topic 04's
  event stream.
- **List rows (1h):** no new colour or column. Hollow = handled (see the
  rules above). The tag, in faint text, says more: `downtime 1h 48m` (time
  left), `host downtime 1h 18m`, `downtime, flexible 1h 12m`, and on an
  unhandled problem with a later downtime, `downtime at 22:00`. Same slot as
  `ack m.keller`, so nothing moves.

---

## 02 Settings panel, in the style of Zed's

**Status: approved,** including the extra settings the designer added. The
notifications page is revised with topic 12 (2d, 2e).

**Shows** (`02-settings.html`, `settings.js`): 2a the panel over the main
window; 2b general; 2c appearance; 2d notifications (top); 2e notifications
(scrolled: the default times and which groups and dashboards are on); 2f
icinga; 2g keymap; 2h advanced; 2i a search for "quiet"; 2j the summary
bar's handled slot, hiding and after a click on *show*.

**Decisions**

- **A panel inside the main window, not a separate OS window.** It opens
  over the main window like a large modal (1080×760 in the 1440×900 window,
  the window dimmed behind it, as behind any modal), with ctrl-, (⌘, on
  macOS), the app menu or the palette. The main window stays the active
  window (its traffic lights keep their colours); × at the panel's top right
  or Esc closes it. It replaces today's 700px two-tab dialog.
- **Left: navigation.** The panel's top bar holds a search field (no traffic
  lights; the panel has none). Below it come the categories as sidebar-style
  rows (30px, an icon in the fixed mark slot, the selected one with the
  sidebar's active background). Under the open category are its sections on
  a guide line; the section in view is marked in the accent colour. At the
  bottom: `focus navbar ctrl-shift-e`.
- **Right: the page.** A 40px header bar (the page name like a dashboard
  title, a faint scope such as `for prod-cluster · on this computer only`, the
  `✓ saved` status and *edit in settings file*). Below it, section labels and
  one row per setting: name (13px), a one-line description (12px muted,
  truncated rather than wrapped), and the control right-aligned. Rows are
  split by the list's row rules. Controls are today's `Switch` (without its
  label), `Segmented`, `Chip`, the bordered `TextField`, and a dropdown in the
  editor's style. Rows that depend on a switch above them are indented 20px.
- **Changes apply at once** (Zed's model): each change is written to
  settings.toml straight away. Text fields apply on Enter or blur; a bad
  value shows its problem under the row (critical text) and isn't applied.
  There is no save or cancel; the header's `✓ saved` confirms the write, and
  the dashboard behind the panel follows the change. (The demo shows `the
  demo saves nothing` there instead.)
- **Categories and contents** (as confirmed by the user):
  - **General:** keep running in the tray, start at login, quiet mode when
    hidden; the longer explanations sit in one note under the rows.
  - **Appearance:** theme (follow system, dark, light), **interface size
    90 / 100 / 115 %** (small, default, large), row density (comfortable or
    compact), times in lists (relative or clock), **handled problems** (three
    switches, all on: hide acknowledged, hide in downtime, hide services of
    hosts that are down; the defaults for every view, which a view can
    override in the editor), and a live preview of the databases dashboard. Compact rows are 32px plus the rule, with a 14px
    circle and the time at the right, and no output line.
  - **Notifications:** an `environment` dropdown first (rules are per
    environment), the environment's master switch (off silences every group
    and dashboard of that environment; it never turns one on), pause, *show
    plugin output* (the output's first line in desktop notifications; off for
    shared screens and the lock screen), the default rule, the **default
    notification times** and the list of groups and dashboards that have
    notifications on (topic 12), storm control, watched and muted objects.
  - **Icinga:** reconcile (adaptive or a fixed interval), the event log's
    retention, the environments with their health dot, a connection summary
    and a gear that opens today's environment editor, and *add environment*.
  - **Keymap:** a read-only, filterable table (action, keys, where) with
    *edit keymap file* in the header.
  - **Advanced:** the log level, *open folder* for logs and config, about.
- **Search** filters across categories: the nav dims the categories without a
  match and counts the matches of the others. The page lists the matching
  rows under `category · section` headings, with the match in the accent
  colour (as in the palette). The rows work in place. A section name that
  matches brings its whole section. rc1's *quiet hours* are now the default
  notification times, and searching "quiet hours" still finds them.
- **Notes for building:** key hints are drawn in Linux notation (ctrl-,);
  macOS shows ⌘, and ⌘⇧E. The nav uses Lucide icons from gpui-component's
  set: `settings`, `sun-moon`, `bell`, `server`, `keyboard`, `wrench`, plus
  `file-code` and `folder-open` on buttons.

---

## 03 Light theme

**Status: approved with two revisions** (a fill shade and a text shade per
state colour; a slightly grey sidebar), drawn and accepted.

**Shows** (`03-light-theme.html`): 3a the main window (sidebar, overview
dashboard, service pane, footer with an unread badge); 3b every row state,
marked rows with the selection bar, and topic 01's banner in a host pane
(hollow = handled); 3c the palette over the dimmed window; 3d Settings →
appearance with *light* chosen; 3e a component sheet, dark beside light.

**Decisions**

- `Theme::light()` has the same fields as `Theme::dark()`. Every value is in
  `v1.css` under `.light`. Its structure copies dark: the window is white
  (`#ffffff`), the pane is a band off it (`#f7f8f9`), code blocks a band
  further (`#f2f4f6`). Rules are light greys (`#d5d9dd` window, `#dfe2e6`
  splits, `#e3e6e9` headers, `#eef0f2` rows).
- **The sidebar is slightly grey** (`#f4f5f7`), with its active rows a step
  darker, as in Zed's light themes.
- **Text keeps the dark theme's contrast steps:** muted `#687077` is about
  5:1 and faint `#899097` about 3.2:1 on white. Strong and body text are
  near-black greys, never pure black.
- **Two shades per state colour** (new theme fields; in dark both shades are
  the same value):
  - **fill**, for circles, dots, bars and grid squares: ok `#34a058`, warning
    `#e0a020`, critical `#e04848`, unknown `#9a68dc`; pending `#c5cacf`;
  - **text**, for words and numbers in a state colour (`CRIT`, `late 3m`,
    perfdata values, KIND in the history), 5:1 or more on white: ok
    `#237a3f`, warning `#9a6200`, critical `#c03535`, unknown `#7a4cbc`.
- **Accent** `#2f74c0` (on-accent text white). **Selection** is tinted
  towards the accent, as in dark: selected row `#e3ebf4`, marked row `#dbe8f7`
  with the 2px accent bar, hover `#f4f6f8`.
- **Shadows and backdrop** are softer: the modal backdrop is a light grey veil
  (`rgba(30,36,42,.28)`) instead of near-black. The traffic lights keep their
  colours.
- *follow system* (the default) switches live with the desktop's appearance.
  The tray icon follows the desktop, not this setting.

---

## 04 Multi-view dashboards

**Status: approved.** The marking of the selected view in the editor was
revised on 2026-10-07 at the user's request (no more accent ring).

**Shows** (`04-multi-view.html`, `views.js`): 4a the databases dashboard
with four views (summary tiles, a list, an empty view, an event stream);
4b a collapsed view, the cursor in the stream, the pane open and a view's own
sort menu; 4c the editor managing views; 4d *add view*; 4e a view's `···`
and the settings of an event stream view.

**Decisions**

- **A dashboard is a list of views stacked on one page,** which scrolls as a
  whole (each view sizes to its content; a long list virtualises inside the
  page's scroll). A single-view dashboard stays exactly as in rc1 (header and
  summary bar), so existing dashboards don't change.
- **The dashboard header** keeps the title; its subtitle says `4 views`, and
  sort moves into the views. **There is no dashboard-wide summary bar:** each
  view header carries its own counts.
- **Counting:** every view whose objects are problems counts toward the
  sidebar's count and dot and toward notifications, each object once even
  when several views show it. A view header's counts are unhandled counts.
- **The view header** is 36px (the summary bar's height) on the pane surface
  (`pane_background`), so it reads as a band between views and differs from
  a grouped list's group-header bands (`row_header`, darker). In order: a
  collapse chevron, the display's icon (in the mark slot), the name (13px
  medium), the filter (faint, cut off first when narrow), the counts (state
  dot and number), `live` for a stream, the view's own sort, and `···`.
- **Empty view:** only the header, with `nothing to show` in place of the
  counts. There is no body and no empty box.
- **Keyboard:** one cursor for the whole page. j/k move through the rows and
  continue into the next view (they skip collapsed views); Tab and shift-Tab
  jump to the first row of the next or previous view. The view holding the
  cursor has a 2px accent bar on its header and its name in strong text.
  **←/→ on a view header collapse and expand it** (a click on the chevron
  too); a collapsed view keeps its header and counts. ctrl-a marks the rows of
  the focused view only. Enter on an event opens its object in the pane.
- **Summary tiles:** one tile per host group (or custom var value): the worst
  state's dot in the mark slot, the name, the host count, a stacked 6px bar
  and the counts in state colours. A click filters the page to that group
  (as on the grid, 5c).
- **Event stream:** the history tab's line format (time, dot, KIND in the
  state colour, `service on host`, the note) from the local event log,
  filtered by the view's filter, newest first, with `N lines` before it
  scrolls. Acks and downtimes are in the accent colour (topic 01).
- **Editor:** the inspector (372px, as today) gets a `views` list. Each row
  has a drag handle, the display icon, the name, what it matches, and `···`
  (move up/down with alt-↑↓, duplicate, collapse by default, remove).
  `+ add view` asks for the display first (list, grouped list, host-group
  grid, summary tiles, event stream); the new view starts from the
  dashboard's filter and goes under the selected one. Below a rule come the
  selected view's settings, which depend on its display (display and lists
  share a row). A list view's **handled** field: *as in settings* (the
  default; a dim row shows the kinds the settings hide), *show* (a faint
  line: every handled problem shows, hollow) or *hide* (the kinds to hide as
  chips: acknowledged, in downtime, host down); the kinds row is always
  there, so nothing below it moves (4c; 9a–g show *show*).
- **The handled button in the view header** (4a, 4b): a multi-view dashboard
  has no summary bar, so each list or grouped-list view header has a fixed,
  right-aligned slot after its counts, with the summary bar's wording and
  behaviour: `2 hidden · show`, and after a click `2 handled · hide` with the
  handled rows shown hollow (4b); the choice is saved with the view. The
  counts beside it are unhandled counts and don't change. A view with
  nothing handled (replication lag) keeps the slot empty. Tiles, grid and
  stream views have no slot. In 4b the pane is 560px, so the header keeps
  its counts, the slot, the sort and `···`; the filter summary is cut first. The preview on the
  left shows the whole dashboard. **The selected view is marked on its header
  only: the focus bar plus a faint accent tint, nothing around its body**
  (4c, 4d, 4e; class `.vh.picked` in `v1.css`). Clicking a view in the
  preview selects it in the inspector. Removing a view needs no confirmation
  (the editor's *discard* undoes it).

---

## 05 Host-group grid

**Status: approved,** with hollow squares for hosts in downtime (accepted)
and the editor's selection revised as in 04.

**Shows** (`05-hostgroup-grid.html`): 5a squares, the default, as a view
above a list; 5b hover, the keyboard cursor and the host pane; 5c a click on
a group filters the page; 5d labelled cells, the other option; 5e the view's
settings in the editor.

**Decisions**

- **A square per host** (12px, 3px gaps), grouped by host group in columns
  (three at full width, two beside the pane), worst groups first. Colour =
  the host's worst state, its own or its worst service's (or the host only, a
  setting). **Healthy hosts are dim green** (ok at about 32 %, 38 % in light),
  so the few problems stand out. Problems are full colour; **handled hosts are
  hollow** (2px inset ring in the state colour): acknowledged, or in downtime
  whatever the state, so an OK host in downtime is a hollow green square.
  Pending is the pending grey.
- **Group header:** the worst unhandled state's dot (mark slot), the name,
  the host count (faint), and how many hosts are in each problem state (dot
  and number: critical, warning, unknown).
- **Interaction:** hover shows a tooltip (host, state, problem count, worst
  service and its output's first line). Arrows move a cursor (2px accent
  outline) and Enter or a click opens the host pane. A click on a group's
  name filters the whole page to that group: the group gets an accent ring,
  the others dim to 35 %, the other views show only that group, and the
  dashboard header shows a `host group edge-ams ×` chip (the filled chip
  style). The filter is temporary: Esc or × clears it, and it is never saved.
- **Squares or labelled cells is an option of the view** in the editor
  (`hosts as: squares | labelled cells`, **squares by default**), not a
  separate display. Cells: one per host (its state dot and name; a problem
  cell is filled and names its worst service, cut off with an ellipsis).
- **Settings (5e):** group by host group (all, or picked ones) or by a custom
  var (`host.vars.site`), an optional filter, colour by `worst of host and
  services` or `host only`, squares or cells, `hide groups where every host
  is ok`, and whether a host in several groups shows in each. The selected
  view is marked on its header as in 04.

---

## 06 Cluster health

**Status: approved.**

**Shows** (`06-cluster-health.html`): 6a the entry in the footer switcher;
6b the page, healthy, as a tab; 6c the page with a satellite gone.

**Decisions**

- **One page per environment.** A `cluster health` row under the nodes in the
  footer switcher (a heart-pulse icon in the mark slot, a faint `zones,
  queues, checks/min` hint), and *cluster health* in the palette. Either
  opens the page as a tab in the sidebar's `open` section, named `cluster
  health · prod-cluster`, with the environment's worst health as its dot. It
  is a page, not a modal, so it can stay open on a second screen.
- **Header:** `cluster health · prod-cluster · seen from master-01`, then
  `updated 12s ago · every 30s`. The summary bar counts connected and
  disconnected endpoints and ends with Icinga's version and uptime.
- **Zones and endpoints:** zones as group-header bands (the worst endpoint's
  dot, the name in semibold, its parent, endpoint and host counts) with their
  endpoints under them. Columns: state dot, endpoint, zone, version, last
  message, messages in and out per second, status. The node icygui is
  connected to has the selected-row background and `this node`. Global zones
  are a faint line at the end. A version older than the masters' says so.
  The numbers come from the endpoint objects the connected node reports;
  **clicking a node makes no extra request.**
- **Numbers from /v1/status** as stat tiles (label, value, what it counts,
  and a **trend line**: the last 30 minutes of the existing 30-second status
  poll, kept in memory only and lost on restart; faint, with the latest point
  in the accent). Checks: active and passive checks per minute, average
  latency, average execution time, pending, late (from icygui's own
  late-check tracking). Queues and connections: API work queue, relay queue,
  cluster connections, HTTP clients, uptime. A value turns warning or
  critical only when it is wrong, always next to its label. **The IcingaDB
  tile shows only when Icinga reports the IcingaDB feature as enabled**;
  icygui never uses IcingaDB, and without it uptime takes the slot.
- **Icinga's global switches** (`enable_notifications`, active checks, event
  handlers, flap detection, performance data) are read-only: green `on`, or
  warning `off`.
- **Degraded (6c):** the endpoint and its zone turn critical. A critical
  banner says what it means for monitoring (`zone fra's results are stale ·
  1,204 checks late · relay queue growing`), with a link to the late checks,
  and the affected tiles colour themselves. The tab's dot and the switcher's
  node dot follow.
- **Cost:** none while the page is closed. It reads the status poll and the
  endpoints and zones icygui already loads. In quiet mode the poll runs every
  5 minutes, and the page says `quiet: every 5 min`.

---

## 07 Lists of every downtime, comment and acknowledgement

**Status: revised and approved.**

**Shows** (`07-comments-downtimes-lists.html`): 7a every downtime, in two
sections; 7b three marked, with the selection bar and the pane; 7c the bulk
removal confirmation; 7d every comment, four marked (an acknowledgement
skipped); 7e the downtime list with *only mine* on; 7f the acknowledged
list, three marked; 7g the remove-acknowledgements confirmation.

**Decisions**

- **Three lists across all objects:** downtimes, comments and **acknowledged**
  problems, all in the same style. They open from the palette as tabs in the
  sidebar's `open` section only (icon in the mark slot, a count). There are
  no sidebar dashboards or dashboard views for them, and **no "my downtimes"
  entry** in the sidebar.
- **"only mine"** is a `Switch` in each list header, before the sort. It keeps
  what the environment's author (by default the API user) set, and the
  subtitle says so (`prod-cluster · set by j.berg`). The summary bar's end
  counts what it hides.
- **The state circle shows the downtime:** hollow while it is in effect,
  whatever the state (rules above). Upcoming and not-started downtimes keep
  the object's normal circle. Acknowledged problems are always hollow.
- **Running and upcoming downtimes are separated inline** by section headers
  in the list, not only by sort order: `in effect · 4` (`handled now · ending
  soonest first`) and `upcoming · 6` (`not in effect yet · starting soonest
  first`), in the style of the grouped list's group-header bands (`row_header`,
  an icon in the mark slot, semibold name). Flexible downtimes that haven't
  started belong to *upcoming*. **Default sort: ends soonest.**
- **Rows** are today's list rows. Downtimes: the window and the comment on the
  second line; in the tag, a 56px progress line and the time left (accent), or
  `not started`, `in 7h 48m`, `by 22:00`. **Config downtimes** have a lock in
  the progress slot and `from config`, and **can't be removed**. A host
  downtime with `all_services` is one row (`+ 18 services`, unfolds on → or a
  click). Comments: `author time · text`; the tag has two fixed,
  left-aligned slots: the kind with its icon (`✓ acknowledgement` or
  `comment`) and the detail (`sticky`, `expires Fri 12:00`), so the icons
  line up. Downtime and flapping comments are left out (the summary bar says
  so). Acknowledged: `who time, how long ago · comment`, and
  in the tag fixed slots for `sticky` (or empty) and the expiry (`expires Thu
  08:00`, `no expiry`).
- **Selection** works as in every list; the selection bar has this list's
  actions: `remove downtimes ⌫`, `remove comments ⌫` or `remove
  acknowledgements ⌫` (primary), `copy names`, and `···`. Its count sits in
  a fixed slot, so the buttons never move.
- **Every removal confirms** (bulk removal), listing every target: downtimes
  grouped by the downtime they belong to (a host downtime lists the host and
  each service), comments with author and time, acknowledgements with who,
  sticky and expiry. The dialog says what follows (the problems notify again
  and count as unhandled; acknowledgement comments go unless persistent),
  lists what it skips with the reason (acknowledgement comments: remove the
  acknowledgement; config downtimes), and the danger button counts.

---

## 08 Sharing dashboards as YAML through the clipboard

**Status: approved,** with the clash defaults and the notification rule
below (accepted). The menus in 8a and 8b carry topic 12's notifications row.

**Shows** (`08-yaml-sharing.html`): 8a *copy as YAML* in the dashboard's
`···`; 8b *copy group as YAML* in the group's `···` and the toast; 8c the
palette; 8d *paste dashboards* in another icygui (staging); 8e the import
preview with clashes; 8f a broken paste; 8g a clashing group selected.

**Decisions**

- **Copy:** `copy as YAML`, **ctrl-shift-c** on the selected dashboard, after
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
  display's own settings. Environment names, ids and secrets are never in it.
  The exported file uses the same YAML; rc1's TOML exports still import. The
  settings file stays TOML.
- **Notification settings are never part of a shared dashboard:** not
  exported, not imported, and the preview asks nothing about them. Imported
  groups and dashboards arrive with notifications **off** (topic 12:
  notifications are opt-in), and use this icygui's default times once
  someone turns them on.
- **Paste:** ctrl-v while the sidebar has the keyboard, `paste dashboards` in
  the footer's + menu, or the palette. It **always opens the preview**, never
  imports straight away.
- **Import preview** (a 1040px dialog): on the left, what the YAML holds as a
  tree (groups with a folder icon, dashboards with a dot and view count),
  each with its status and choice on the right **in two fixed-width,
  left-aligned columns**: the status (`new` in the OK colour, `exists here`
  or `name taken` in the warning colour), sized for `exists here`, then the
  choice (`· merge`, `· keep both`, `· replace`, `· skip`, faint), sized for
  `· keep both`; `new` rows leave the choice column empty. Switching a choice
  changes only its word; nothing else moves. The selected row opens a detail with the choice, what it leads to
  (`imported as databases 2`), and what doesn't fit this environment (host
  groups that don't exist here). **A clashing dashboard name defaults to
  keep both** (or replace, skip). **A clashing group defaults to merge** (its
  dashboards go into the group already here, each clashing dashboard with its
  own choice), **with keep both offered** (a new group `overview 2`) (8g).
  Below: the format check (`version 2 · all 7 filters parse`). On the right,
  the YAML read-only, with the selected row's lines marked in the accent. The
  button counts what will be imported (`import 3 dashboards`); undo with
  ctrl-z.
- **Broken paste (8f):** the dialog shows the YAML around the problem, with
  the line marked critical and the character underlined (the filter field's
  error style from topic 09), plus `line 10, column 42: …` and where it is.
  It imports nothing. A newer format version says *update icygui to import
  this*; unknown fields are listed and ignored. Text that isn't icygui YAML
  at all only gets a toast.

---

## 09 Filter autocomplete, in the style of Zed

**Status: revised and approved** (a smaller popup under the word being
typed, never outside the field; syntax colouring and fuzzy matching
approved).

**Shows** (`09-filter-autocomplete.html`, each frame with a zoomed crop):
9a attributes after `host.`; 9b custom variables after `host.vars.`; 9c
values inside a string; 9d functions; 9e signature help inside a call; 9f
hover help; 9g a parse error.

**Decisions**

- **The popup** is the app's menu card (element background, window border,
  the menus' shadow) at the menu's text size: 272px wide, 24px rows, **8 rows
  visible** with a thin scroll thumb when there are more. A kind icon in the
  mark slot (`T` string, `#` number, `[]` array, `{}` dictionary, a toggle for
  bool, a square-function for functions, a quote for values, `(x)` for custom
  variables), the label with the fuzzy-matched characters in the accent
  colour, faint extras (values in use), and the type or count right-aligned.
  The selected row has the selected-row background.
- **Placement:** right under the line being typed, its left edge at the
  column where the word being completed starts, or at the field's inner left
  edge when it wouldn't fit there. **It never sticks out of the field on the
  left.**
- **The documentation card** (256px, same card style) beside the list explains
  the selected item: its signature and type, what it means, an example in a
  code chip, and what is in use in this environment. It sits to the right of
  the list and flips to the left when there is no room (always, from the
  inspector).
- **Sources:** attributes per object type with their descriptions (a static
  table); custom variables and values from the live object store, ranked by
  how many objects have them; host group, service group, host and service
  names and check commands for strings in those positions; functions and
  methods with signatures; the state constants. Protected variables
  (passwords, tokens) are listed by name, never with values.
- **Fuzzy matching** (`r` matches `backup_retention`), ranked by match, then
  by how many objects have the name.
- **Keys:** completion opens as you type after `.`, inside a string, and on
  an identifier, or with ctrl-space. ↑↓ choose, Tab or Enter accept, Esc
  closes. Typing goes on filtering.
- **Signature help** floats above the line inside a call, with the current
  argument underlined and its description; completions for that argument show
  below at the same time.
- **Hover help** shows the documentation card for the token under the
  pointer (the token gets the hover background), after the tooltip delay.
- **Parse error:** a critical wavy underline at the position, the field's
  border turns critical, and the status reads `line 3, column 23 · expected a
  value`. Hovering the underline shows the full message and a hint. The
  preview keeps the last valid result, and the summary bar says so.
- **Syntax colours** in the field: attributes muted, strings in the OK
  colour, numbers in the unknown colour, functions in the accent, operators
  faint. They are all theme tokens, so the light theme follows.
- The inspector in these frames also shows topic 12's notifications row.

---

## 10 Palette multi-select

**Status: approved with two changes** (editing the query clears the
selection; the combined multi-host view arrives with nothing marked and
offers *mark all problems*), **and the combined view rebuilt as a grouped
list** (approved: "this looks perfect"), with collapsible hosts. It also works
for a selection of services only.

**Shows** (`10-palette-multiselect.html`, the palette frames with zoomed
crops): 10a no selection; 10b one block (shift-↓ twice); 10c several blocks
(ctrl-click); 10d ctrl-a; 10e the cursor on *all N matches*; 10f the one
bulk dialog; 10g hosts selected without a verb; 10h the combined multi-host
view, paged by count, one host collapsed; 10i pinned as a tab, and *save as
dashboard*; 10j *mark all problems*; 10k a host expanded in place by `+ N
more` (no pane, full width, sticky band, `− show fewer`); 10l a click on a
host's name opens the host pane (a paged host; the pane pages its own
services, `+ 16 more`).

**Behaviour**

- **What can be selected:** object rows only, meaning hosts, services, and a
  verb query's action rows (`Acknowledge · postgres-replication`). Commands,
  dashboards, environments and the *all N matches* row are skipped by
  shift-↑↓ and ignore ctrl-click.
- **Keys and clicks:** shift-↑/↓ extends or shrinks the selection from the
  anchor (the row where it started) to the cursor. ctrl-click (⌘-click)
  toggles a row and moves the cursor there; shift-click selects the range from
  the anchor. ctrl-a (⌘A) selects every match. A plain ↑↓ moves the cursor and
  keeps the selection. **Editing the query clears the selection.** Esc clears
  the selection; a second Esc closes the palette.
- **Enter:** with a selection and a verb query, it opens ONE dialog for every
  selected object (today's action dialog). With a selection and no verb, it
  opens the combined view; Tab opens it as a tab directly. Without a
  selection, Enter runs the cursor row as today, and ctrl-↵ runs the verb on
  all matches as today.
- **Verb queries list every match** (rc1: the best 4 services and 2 hosts).
  The list scrolls inside the palette's 420px. *All N matches* moves to the
  top of the section, so it is always in view.
- **The all-matches row shows what it counts:** with the cursor on it, the
  rows it counts get a dashed outline (a preview, not a selection; no
  counter).

**Drawing**

- **One rounded outline per block** of contiguous selected rows: the block
  has the lists' marked tint (`row_marked`) and a 1px accent outline with the
  rows' 6px radius. The outline is drawn as an overlay above the rows, so the
  cursor row can't cover it. Rows inside a block lose their own corners.
- **The cursor inside a block** keeps a row background of its own
  (`element_hover`, lighter and neutral), so it stands out from the blue
  tint. Outside a block it is the selected-row background, as today.
- **Nothing moves:** the block wraps the rows without adding size; section
  labels break a block in two; there is never a check column.
- **Footer:** `3 selected` in the accent, in a fixed slot (14ch, so `all 7
  selected` fits), then what Enter does (`↵ acknowledge all 3`, `↵ open
  together`); at the right, `ctrl-a select all 7` and `esc clear`. A
  growing count or action pushes nothing. Without a selection it shows the
  keys as today, plus `shift-↑↓ select`.

**The bulk dialog (10f):** today's action dialog. The title counts the
targets (`Acknowledge · 7 services`), and the target box lists **every**
object with its state dot (it scrolls; it no longer stops at five with `+ N
more`). Skipped objects are listed with the reason, as today. The send button
counts (`acknowledge 7`).

**The combined view (10h, 10j, 10k):** in the main area, like a dashboard.
The header reads `3 hosts · db-prod-01, db-prod-02, db-prod-03`, then `×`
(back to the dashboard), `↗ pin as tab` and *save as dashboard*. The summary bar
counts the services by state and ends with `3 hosts · 63 services`.

- **A grouped list by host,** built from the list's parts (the
  host-with-services rule above): each host is a slim group-header band
  (36px, `row_header`; the host's state dot in the rows' mark column, the
  name, the address and its check output faint, the per-state counts on the
  right); its services are standard list rows under it **with no indent**
  (the same circle, time and text column as on every dashboard). A selected
  service shows under its host; **a selection of services only** opens the
  same way, grouped by their hosts.
- **Paged by count (10h, 10k):** up to 7 rows per host, problems first and
  never hidden, OK services in name order fill the rest; `+ N more` in the
  text column (db-prod-02, all OK: 7 rows and `+ 12 more`). A click, Enter or
  → shows the whole host in place, downward, and the slot reads `− show
  fewer`; nothing opens, and the expansion lasts while the view is open.
  While an expanded host scrolls, its band sticks to the top of the list
  (10k). A click on the host's **name** (or Enter on its band) opens the
  host in the pane, which pages its own services the same way (10l): the
  name opens the pane, `+ N more` only expands in place.
- **Collapsible hosts:** the collapse chevron at the left of each band, at the
  x of 04's view-header chevron (the dot, the name and the counts keep their
  places). A click on the chevron, or ←/→ with the cursor on the band,
  collapses or expands the host. Collapsed (db-prod-01 in 10h), the band stays
  with its counts on the right and its rows hide. Collapsing only changes what
  shows: the summary, the counts and *mark all problems* still cover every
  host, and a collapsed host whose rows are marked gets the marked tint and
  bar on its band.
- **It arrives with nothing marked:** the palette's selection chose what to
  show, not what to act on. The bulk bar under the list is always there in
  this view, so nothing moves when rows get marked. Empty, it reads `nothing
  marked · x marks a row · ctrl-a marks all` and, at the right, the count of
  problems (`3 services`: the services with an unhandled problem, and a host
  that is down or unreachable) in a fixed right-aligned slot before **mark
  all problems**, so the button stays put when the count changes. With marks
  (10j), it turns in place into the selection bar: the count (`3 services
  marked`, in a fixed slot), acknowledge, downtime, check now, comment,
  `···`, and `clear esc`. Esc clears the marks and the bar returns to *mark
  all problems*.
- Everything else is the normal list: x, shift-↓, ctrl-a, Enter for the pane.
  In 10j, db-prod-02 (all OK) is collapsed, so every marked row is in view.
- **This band replaces rc1's grouped-list header** (the full-height host row
  with a small circle and indented rows) in every host-with-services view.

**Pin and save (10i):** `↗ pin as tab` adds the view to the sidebar's `open`
section (worst-state dot, `3 hosts db-prod-01, …`), where it survives
restarts. Its slot in the header stays, empty, so nothing moves. *save as
dashboard* asks for a name and a group, and whether to keep **these 3
hosts** (`host.name in [...]`) or **the query** (`match("db-prod*",
host.name)`). The result is a grouped list by host, editable later.

---

## 11 Read-only config: command line and who Icinga notifies

**Status: approved, both parts.**

**Shows** (`11-config-tab.html`): 11a the host pane's config tab; 11b the
service pane's check section; 11c both without the extra permissions; 11d
11b with the argument table unfolded.

**Decisions**

- **Each part shows when its permission is granted;** otherwise a dashed box
  with a lock names the permission it needs, links to the user guide's API
  user section, and shows what rc1 already knows (11c). The two can be
  granted separately; the UI works with either, both or neither. Nothing is
  greyed out without a reason.
- **Command line** (`objects/query/CheckCommand`): a code block with the
  command line as Icinga would run it, every macro resolved and the resolved
  values tinted in the accent. Each argument (flag and value, e.g.
  `--host=10.0.2.11,10.0.2.13`) is unbreakable, so a long command line wraps
  only between arguments, never inside one. A copy button sits at the right of the section
  label, which also names the CheckCommand and where it runs. **The argument
  table folds under the command line, folded by default** (11a, 11b): a 28px
  fold row right under the code block, with a chevron in a fixed 12px slot
  (`›` folded, `⌄` open), `N arguments` and a faint `where each value comes
  from`. A click on the row, or →/← with the cursor on it, unfolds and folds
  it. Unfolded (11d), the table opens below the row: argument, value, and
  `from` (`$check_address$ → host.address`, `host.vars.pg_user`, `command
  default`, `set in the command`), one row per argument of the command line.
  The row and everything above it never move; only what is below the table
  moves down. Protected variables show `***` in both the command line and
  the table, and the table says `protected`.
- **Who Icinga notifies** (`objects/query/User`, `UserGroup`, `TimePeriod`):
  one block per Notification object (name, the apply rule, the command), its
  user groups with their members indented, each user's email and pager, and
  their own period at the right. A line of conditions closes each block:
  period, states, types, delay, interval. The section label sums up *now*
  (`now: dba-oncall by mail at once; dba-pager from 18:00`).
- **Without the permissions (11c):** the command is shown by name, and the
  Notification objects with the users and groups they name (rc1 already
  reads `Notification`).
- **Where:** the host pane's config tab (after the check table, before
  Icinga's switches) and the service pane's check section (after the check
  table). A tab gets the same in its wide layout.

---

## 12 Notifications: on or off per group and dashboard, and when

**Status: the design is approved; revised on 2026-10-07 for the user's
decision that notifications are opt-in** (PLAN.md §4.2, *Notifications are
opt-in* and *Notification times per group*). New in v1.

**Shows** (`12-notification-times.html`): 12a Settings → notifications, the
default times and which groups and dashboards are on; 12b a group's
notification settings, off (the default); 12c the same group turned on, with
custom times; 12d the dashboard editor's notifications row, on and custom;
12e the sidebar's marks and the hover; 12f the group's `···` menu with the
quick switch; 12g a dashboard's `···` menu, following its group.

**Decisions**

- **Notifications are opt-in.** Nothing notifies until someone turns
  notifications on for a group or a dashboard. **Groups are off by default.**
  A dashboard follows its group unless it is set itself (on or off). Nothing
  called "inherit" or "default" ever switches notifications on by itself.
  (rc1 inherited the environment's master switch, which was on; rc1 was never
  deployed, so no one relies on that.)
- **Two separate things per group and per dashboard:**
  1. **on or off**: a switch;
  2. **when**: `default` (the default times and rule from the settings
     page) or `custom` (its own, set in its notification settings).

  Turning notifications on uses whichever *when* applies. A dashboard's
  `default` is what its group uses: the group's custom times if it has them,
  otherwise the environment's defaults. Custom times replace the defaults for
  that group or dashboard only, and a group's custom times pass on to its
  dashboards that use `default`.
- **No fixed presets:** engineers set the times themselves. A team's "9 to 5"
  or "24/7" is just the times they set.
- **The times controls** (the same everywhere): `notify` as `always`, `at set
  times` or `never` (segmented); at set times, one row per window: day chips
  `mo`…`su` and `from → to` fields, a remove `×`, and `+ add times`;
  `critical and down at any time` (switch, on by default). Outside the times,
  notifications go silently into the notification centre; `never` sends
  none to the desktop. A window may cross midnight.
- **Settings → notifications (12a)** holds the **defaults** per environment
  (the `environment` dropdown at the top of the page): the default rule
  (states, events, hard states only, skip handled, only after, sound) and the
  default notification times (rc1's quiet hours, turned into the times
  notifications are active). The section says explicitly that these are
  defaults, used by the groups and dashboards that have notifications turned
  on, unless one has custom times, and that nothing notifies until a group
  or dashboard is turned on. Below it, **`turned on`** lists per environment
  the groups and dashboards whose notifications are on (a group with how many
  of its dashboards are on; a dashboard set on by itself), each with what
  it uses: `default times` (faint) or `custom: <its times>`, and `open ↗`,
  which opens its notification settings. The environment line counts them
  (`4 on · 3 with custom times`); an environment with nothing on says that
  it doesn't notify.
- **A group's notification settings (12b, 12c):** a dialog from the group's
  `···` → `notification settings`. First row: **notifications**, with the
  switch and a one-line summary of what applies (`on · default times:
  Mon–Fri 07:00 → 22:00 · Sat–Sun 09:00 → 20:00, critical any time`; `on ·
  custom times: Mon–Fri 09:00 → 17:00, critical and down at any time`; `off ·
  nothing in platform notifies, except dashboards turned on themselves`).
  Then **when**: `default | custom` (segmented). `default` shows a read-only
  box with the environment's default times and rule, whether they are
  notifying now (or, while the group is off, `used once notifications are
  on`), and `change the defaults ↗` (opens the settings panel at them).
  `custom` drops down, in place and indented, the same controls as the
  settings page, for this group only, with a faint line saying what they
  replace. The dialog lists the group's dashboards and what each one does,
  in fixed columns: name, `on` or `off`, where that comes from (`as
  platform` or `set on the dashboard`), and the times it uses
  (`platform's times`, `custom: always`). The *when* controls stay usable
  while the group is off, so times can be set before turning it on. Changes
  apply at once; `done` closes.
- **A dashboard's notifications (12d):** a row in the dashboard editor's
  inspector, with the dashboard's name and group (it belongs to the
  dashboard, not to a view): the label says where the state comes from (`as
  overview`, or `set on this dashboard`); then the switch with `on`/`off`, a
  faint note on the group (`overview is off`) and, when set on the
  dashboard, `follow overview` at the right to hand the choice back to the
  group (12d; 09's frames show a dashboard following its group); then `when:
  default | custom`, the custom controls stacked for the narrow column. A
  faint line says what applies now. It is saved with the dashboard, like the
  rest of the editor.
- **Quick switch in the `···` menus (12f, 12g):** the group's and the
  dashboard's menus get a `notifications` row with a switch (the `Switch`
  without its label) at the right, directly above `notification settings`
  (no trailing "…"), in their own section. The settings entry's faint detail
  says `default times` or `custom times`. In a dashboard's menu, while it
  follows its group, the row's faint detail says `as overview`; flipping the
  switch sets the dashboard itself. A dashboard's `notification settings`
  opens the editor at the notifications row.
- **The sidebar mark (12e):** one fixed slot after a group's name and before
  a dashboard's count. A **faint bell** when the group or dashboard notifies
  with the default times (for a dashboard: its group's), a **faint clock**
  instead when it notifies with custom times set on it, and **nothing when
  it is off**. The slot is always reserved, so turning notifications on or
  setting times moves nothing. Hovering the mark says what applies and
  whether it is notifying now (`platform: on, its own times` / `Mon–Fri
  09:00 → 17:00 · critical and down at any time` / `notifying now, until
  17:00 · 3 of its 4 dashboards on`).
- **The environment's master switch** (top of the notifications page) stays
  as a way to silence a whole environment; off overrides every group and
  dashboard, and on never turns any of them on.
- **Never shared:** on/off and times stay with each person's icygui (per
  environment, on this computer); YAML sharing neither exports nor imports
  them (topic 08), so imported groups and dashboards arrive off.
- **Replaces** rc1's four notification choices in the group menu (inherit,
  on, off, custom rule) and rc1's quiet hours.

---

## 13 icygui on Windows: the installer, the window, the tray and toasts

**Status: drawn, its open points decided; for the user's review** (PLAN.md
§4.2, *Windows support*).

**Shows** (`13-windows.html`): the installer, each page in Windows' dark and
light mode side by side: 13a welcome; 13b install for me or for everyone;
13c destination; 13d additional tasks; 13e ready; 13f installing; 13g
finished; 13h uninstall; 13i the images we ship. The app: 13j the main
window with Windows' caption buttons (zooms of both header ends); 13k every
state of the caption buttons, dark and light, and the sidebar header per
platform; 13l the tray icon's tooltip and menu; 13m a toast and Windows'
notification centre; 13n the tray menu and a toast in light mode.

**The installer** (Inno Setup 6.6; only what Inno does)

- **Style:** `WizardStyle=modern dynamic`: Inno's modern wizard, which
  follows Windows between dark and light. Inno fixes the layout (welcome and
  finished pages with the tall image on the left; inner pages with the header
  band, title, subtitle and the small image at the top right; Back, Next and
  Cancel at the bottom right), the native controls and Segoe UI. About 700 ×
  540 px at 100 %.
- **What we ship** (generated from `assets/logo/` by `cargo xtask icons`,
  like the app icons, each at 100, 125, 150, 200 and 250 %, listed
  comma-separated so Inno picks the closest to the screen's scaling):
  - `WizardImageFile` / `WizardImageFileDynamicDark` (240 × 459 at 100 %):
    the logo's mark (its node in the theme's accent, `#2f74c0` light,
    `#74ade8` dark), `icygui` in IBM Plex Mono, `Icinga 2 on your desktop`,
    and a corner of the host-group grid (topic 05) fading in at the bottom,
    on a light grey gradient or the app icon's dark tile gradient;
  - `WizardSmallImageFile` / `…DynamicDark` (58 × 58): the mark alone,
    transparent around it;
  - `SetupIconFile` and `UninstallDisplayIcon`: `icygui.ico` (16 to 256 px),
    the same icon the executable embeds.
- **Texts** in `[Messages]` and `[CustomMessages]`; the welcome page says the
  install is per user and needs no administrator rights.
  `ButtonBrowse=&Browse` drops Inno's "Browse...", per the app's label rule.
- **Install mode (13b):** `PrivilegesRequired=lowest` with
  `PrivilegesRequiredOverridesAllowed=dialog`, so Inno asks first, in its
  own dialog and words: **for me only** (preselected, no administrator
  rights, `%LOCALAPPDATA%\Programs\icygui`) or **for all users** (elevation,
  shield, `C:\Program Files\icygui`); `DefaultDirName={autopf}\icygui`
  covers both. `/CURRENTUSER` and `/ALLUSERS` skip the question.
- **Destination (13c):** shown on a first install only (`DisableDirPage=auto`).
- **Tasks (13d), `[Tasks]`:** *create a desktop shortcut* (off, `Flags:
  unchecked`) and *start icygui in the tray when you sign in* (on). The
  second writes the same per-user Run entry as Settings → general → start at
  login (`HKCU\…\Run`, `icygui.exe --background`). The Start menu shortcut
  is always made, with the app's AppUserModelID (the toasts need it).
- **Ready (13e), installing (13f):** Inno's pages. A running icygui is closed
  by the restart manager (`CloseApplications=yes`) and started again after an
  update if it was running; updates keep every setting.
- **Finished (13g):** *launch icygui*, checked (`[Run] … Flags: postinstall
  nowait skipifsilent`); a silent install doesn't launch.
- **Uninstall (13h):** Inno's confirmation (`ConfirmUninstall`, *No* the
  default), then our question from the uninstaller's `[Code]`
  (`TaskDialogMsgBox` with our button labels): **keep them** (default) or
  **remove them**: the settings in `%APPDATA%\icygui`, the event history in
  `%LOCALAPPDATA%\icygui`, and the passwords icygui saved in Windows
  Credential Manager. Program files, Start menu, desktop and sign-in entries
  always go.
- **Also:** a portable `.zip`; unsigned unless a code-signing certificate is
  configured (SmartScreen warns on first start, documented).

**The app on Windows**

- **Caption buttons (13j, 13k):** Windows 11's minimise, maximise (restore
  when maximised) and close, 46 px wide each and the full height of the 40 px
  header row, in a fixed slot at the top right of the right-most header (the
  pane's, or the list's when no pane is open; the pane's × sits left of
  `↗ open as tab`), so the header's own items keep their places and nothing moves when the window becomes active, inactive or
  maximised. Glyphs in the theme's strong text colour (faint while the
  window is inactive); hover: a faint fill; close: `#c42b1c` with a white
  glyph, pressed `#c83c31`. Our theme decides their colours, as for the rest
  of our window. Snap layouts on the maximise button come from Windows.
- **Top left:** the slot of the traffic lights (52 px) holds the app's mark
  (16 px); a click on it opens the window menu (as alt-space does); the
  search keeps its place. Dragging a header moves the window, a double click
  maximises it (as on Linux; `chrome.rs` gets a third `Controls` variant).
- **Tray (13l, 13n):** the mark tinted with the worst unhandled state of every
  environment, as today. Windows cuts tooltips at 128 characters, so the
  tooltip is one short line per environment (`prod-cluster: 38 unhandled, 4
  critical`). Right click: the native Windows 11 menu with today's items
  (open, pause notifications ▸, environment ▸ with a check at the active one,
  quit); left click opens the window. Native menus follow Windows' dark or
  light mode.
- **Toasts (13m, 13n):** app icon and name from the Start menu shortcut's
  AppUserModelID; title and body as today (`CRITICAL · postgres-replication
  on db-prod-03`, the output's first line, where it matched); the state
  circle as the toast's image (`appLogoOverride`, cropped to a circle);
  **Acknowledge** and **Open** buttons; a click on the body opens the object.
  They stay in Windows' notification centre, grouped under icygui; icygui's
  own notification centre keeps the full history.

**Decided** (by the coordinator, for the user's review of the frames)

1. The pane's × moves left of `↗ open as tab` on every platform (a fixed
   slot), so it never sits next to Windows' close button (all pane frames
   re-rendered).
2. The app's mark sits in the traffic-light slot on Windows.
3. *Remove them* in the uninstaller also deletes the saved passwords in
   Credential Manager.
4. The toast's image is the state circle.
5. The tall wizard image: the mark, the name, `Icinga 2 on your desktop` and
   the grid corner.
6. `Browse` without Inno's trailing dots.

