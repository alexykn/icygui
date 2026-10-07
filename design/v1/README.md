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
