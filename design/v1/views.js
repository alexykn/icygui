// Multi-view dashboards (topic 04), shared with the host-group grid (05).

const DISPLAY_ICON = { list: 'list', grouped: 'rows-3', grid: 'layout-grid', tiles: 'chart-bar', stream: 'activity', handling: 'users', downtimes: 'calendar-clock' };
const DISPLAY_NAME = { list: 'list', grouped: 'grouped list', grid: 'host-group grid', tiles: 'summary tiles', stream: 'event stream', handling: 'handling', downtimes: 'downtimes' };

// A view's header: collapse chevron, display icon, name, filter summary,
// counts, its own sort and ···. focus: the accent bar of the view holding the
// cursor. picked: the view selected in the dashboard editor: the same bar plus
// a faint accent tint on the header; nothing around the view's body.
//
// handled: list and grouped-list views keep a fixed, right-aligned slot after
// the counts for their handled problems, with the summary bar's wording and
// behaviour: [n, shown] gives "2 hidden · show" or "2 handled · hide"; with
// nothing handled the slot stays empty, so nothing moves. The counts are
// unhandled counts; show and hide never change them.
function viewHeader({ name, display = 'list', ic = '', filter = '', counts = [], sort = 'severity ↓', collapsed = false, focus = false, picked = false, empty = '', live = false, sortOpen = false, more = true, moreOpen = false, handled = null, controls = '' }) {
  // a count is [state, n], or ready HTML for kinds that count other things (handling: ✓ 2)
  const c = counts.map((x) => (typeof x === 'string' ? `<span>${x}</span>` : `<span>${dot(x[0], 'd7')}${x[1]}</span>`)).join('');
  const hasSlot = display === 'list' || display === 'grouped';
  const hs = !hasSlot ? '' : `<span class="hs">${handled && handled[0] ? `${handled[1] ? `${handled[0]} handled` : `${handled[0]} hidden`}<span class="faint"> · </span><span class="hbtn">${handled[1] ? 'hide' : 'show'}</span>` : ''}</span>`;
  return `<div class="vh${focus || picked ? ' focus' : ''}${picked ? ' picked' : ''}"><span class="chev">${icon(collapsed ? 'chevron-right' : 'chevron-down', 12)}</span>
    <span class="vicon">${icon(ic || DISPLAY_ICON[display], 13)}</span><span class="vn">${name}</span>
    <span class="vf">${esc(filter)}</span>
    ${empty ? `<span class="empty">${empty}</span>` : ''}${c ? `<span class="vc">${c}</span>` : ''}${hs}${controls}
    ${live ? `<span style="display:flex;align-items:center;gap:6px;color:var(--t-faint)">${dot('ok', 'd6')}live</span>` : ''}
    ${sort ? sortSlot(display, sort, { open: sortOpen }) : ''}${more ? `<span class="glyph${moreOpen ? ' sel' : ''}">···</span>` : ''}</div>`;
}

// VIEW CONTROLS (topic 14, round 5; a rule for every view kind): a view has
// ONE set of controls and they live in its view header. On a one-view
// dashboard the view header is the page header (roomy: the summary bar is its
// second row); stacked, the same controls sit compactly in the 36px header,
// and "only mine" and the row density move into the view's ···.
//
// The sort sits in a fixed slot sized for its kind's longest sort label,
// right-aligned, so choosing another sort changes only the word (nothing left
// of it moves).
const SORT_CH = { list: 19, grouped: 19, grid: 11, tiles: 11, stream: 12, handling: 17, downtimes: 16 };
// When a stacked header is tight (beside a pane), the slot gives way down to
// its word after the filter summary is gone (the filter is cut first).
const sortSlot = (kind, sort, { open = false } = {}) => `<span class="vsort" style="width:${SORT_CH[kind] || 12}ch"><span${open ? ' class="glyph sel" style="font-size:12px"' : ''}>${sort}</span></span>`;

// ROW DENSITY PER VIEW: Settings → appearance → row density is the default;
// every list-like view (list, grouped list, handling, downtimes in list mode,
// event stream) follows it until a density is chosen on that view.
//   v: 'default' (as in settings), 'comfortable' or 'compact'
//   eff: the global value, shown while the view follows it
// One-view header: two icons in a fixed slot (left of the sort). Chosen on
// the view: the icon is filled, as a segmented control's "on". Following the
// settings: the icon of the global value has a dashed inset outline (dashed =
// not set here, as topic 10's preview outline). dim: kept for a control that
// cannot act (unused: the timeline has rows too).
function densityToggle({ v = 'default', eff = 'comfortable', dim = false } = {}) {
  const cur = v === 'default' ? eff : v;
  const cell = (d, ic) => `<span class="${d === cur ? (v === 'default' ? 'dflt' : 'on') : ''}">${icon(ic, 13)}</span>`;
  return `<span class="dens${dim ? ' dim' : ''}">${cell('comfortable', 'rows-2')}${cell('compact', 'rows-4')}</span>`;
}
// the editor's rows field of a list-like view: as in settings (naming the
// global value; new views start here), comfortable or compact
const rowsField = (v = 'default', eff = 'comfortable', note = '') => `<div class="field"><div class="lab">rows<span class="st faint">${note || (v === 'default' ? 'follows the settings' : 'set on this view')}</span></div>${select(v === 'default' ? `as in settings (${eff})` : v)}</div>`;
// the view's ··· (stacked): only mine (handling and downtimes), the rows, then
// the view's own items. rows: as densityToggle's v
// hostsAs: a host list's mode ('rows' or 'with services', topic 15)
function viewMoreMenu({ x, y, w = 268, mine = null, hostsAs = null, rows = 'default', eff = 'comfortable', hov = '' }) {
  const it = (label, o = {}) => ({ label, ...o, hov: hov === label });
  return menu([
    ...(mine === null ? [] : [it('only mine', { chk: mine, det: 'what I set' }), '-']),
    ...(hostsAs === null ? [] : ['hosts as', it('rows', { chk: hostsAs === 'rows', det: 'one row per host' }), it('with services', { chk: hostsAs === 'with services', det: 'each host a band' }), '-']),
    'rows',
    it('comfortable', { chk: rows === 'comfortable', ic: 'rows-2' }),
    it('compact', { chk: rows === 'compact', ic: 'rows-4' }),
    it('follow the default', { chk: rows === 'default', det: `settings: ${eff}` }),
    '-',
    it('collapse', { key: '←' }),
    it('edit view'),
  ], { x, y, w });
}

const DB_TILES = [
  ['crit', 'pg-orders', '3 hosts', [['crit', 1], ['warn', 2], ['ok', 38]]],
  ['warn', 'pg-billing', '3 hosts', [['warn', 1], ['ok', 38]]],
  ['unk', 'mysql-legacy', '3 hosts', [['unk', 1], ['warn', 1], ['ok', 31]]],
  ['ok', 'redis-cache', '3 hosts', [['ok', 18]]],
];
function tile([st, name, meta, parts], { on } = {}) {
  const total = parts.reduce((a, [, n]) => a + n, 0);
  const bar = parts.map(([s, n]) => `<span style="flex:${n};background:var(--${s})"></span>`).join('');
  const nums = parts.map(([s, n]) => `<span><b style="color:var(--${s}-text)">${n}</b> ${STATE_WORD[s]}</span>`).join('');
  return `<div class="tile${on ? ' on' : ''}"><div class="th">${dot(st)}<span class="nm">${name}</span><span class="meta">${meta}</span></div>
    <div class="bar">${bar}</div><div class="nums">${nums}</div></div>`;
}
function tilesBody(tiles = DB_TILES, cols = 4, onIndex = -1) {
  return `<div class="vbody"><div class="tiles" style="grid-template-columns:repeat(${cols},minmax(0,1fr))">${tiles.map((t, i) => tile(t, { on: i === onIndex })).join('')}</div></div>`;
}

const DB_EVENTS = [
  ['14:11', 'crit', 'CRITICAL', 'postgres-replication', 'db-prod-03', 'hard · CRITICAL - standby lag 412s (> 300s)'],
  ['14:09', 'warn', 'WARNING', 'pg-connections', 'db-prod-03', 'soft 2/3 · WARNING - 182 of 200 per-db limit (orders)'],
  ['14:02', 'accb', 'ACK', 'pg-autovacuum', 'db-prod-01', 'dba-oncall: vacuum running on orders, about 30 minutes'],
  ['13:58', 'ok', 'OK', 'mysql-replication', 'db-mysql-02', 'hard · OK - replica in sync, lag 0s'],
  ['13:55', 'accb', 'DOWNTIME', 'pg-locks', 'db-prod-05', 'dba-oncall: VACUUM FULL on orders_archive, flexible 2h'],
  ['13:41', 'pend', 'COMMENT', 'postgres-replication', 'db-prod-03', 'j.berg: failover drill on db-prod-01 at 15:00'],
  ['13:22', 'warn', 'WARNING', 'pg-bloat', 'db-prod-05', 'hard · WARNING - check_postgres degraded'],
  ['13:10', 'unk', 'UNKNOWN', 'mysql-replication', 'db-mysql-03', 'hard · UNKNOWN - connection refused'],
];
function eventRow([t, st, kind, name, host, note], { sel } = {}) {
  const color = st === 'pend' ? 'var(--t-faint)' : st === 'accb' ? 'var(--accent)' : `var(--${st}-text)`;
  return `<div class="ev${sel ? ' sel' : ''}"><span class="t">${t}</span><span class="d">${dot(st, 'd7')}</span>
    <div style="min-width:0"><div class="l1"><span class="k" style="color:${color}">${kind}</span> <span class="n">${name}</span><span class="faint"> on </span>${host}</div><div class="l2">${note}</div></div></div>`;
}

// The editor's "handled" field of a list view: follow the settings (the
// default), or set here (show, or hide the chosen kinds). The kinds row is
// always there, so nothing below it moves: dim and read-only while following
// the settings, editable with "hide", a faint line with "show".
function handledField(mode = 0, kinds = [true, true, true]) {
  const segs = ['as in settings', 'show', 'hide'].map((o, i) => `<span class="${i === mode ? 'on' : ''}" style="flex:1;padding:0 6px">${o}</span>`).join('');
  const chips = ['acknowledged', 'in downtime', 'host down'].map((k, i) => chip(k, { sel: kinds[i] })).join('');
  const second = mode === 1
    ? `<span class="faint" style="font-size:11.5px;height:24px;display:flex;align-items:center">every handled problem shows, hollow</span>`
    : `<div style="display:flex;gap:6px;align-items:center;height:24px${mode === 0 ? ';opacity:.55' : ''}"><span class="faint" style="font-size:11.5px">hide</span>${chips}</div>`;
  return `<div class="field"><div class="lab">handled<span class="st faint">${mode === 0 ? 'follows the settings' : 'set on this view'}</span></div><span class="seg" style="display:flex">${segs}</span>${second}</div>`;
}

// ---- the dashboard editor's dashboard fields (topic 14, round 5; every editor) ----
// The sidebar mark (state or icon) with its swatch, a field's height; with
// state the swatch is not clickable (no hover), with icon it opens the picker.
// The dashboard's fields: the name, full width; then two columns on the
// inspector's grid: the sidebar mark (swatch + dropdown filling the column,
// a short hint as the label's suffix) and the group.
const markField = ({ mode = 'state', st = 'crit', ic = 'users', sq = '', selOpen = false }) => {
  const swatch = mode === 'state'
    ? `<span class="mswatch dis"><span class="dot ${st}" style="width:10px;height:10px"></span></span>`
    : `<span class="mswatch${sq ? ' ' + sq : ''}">${icon(ic, 16)}</span>`;
  return `<div class="field"><div class="lab">sidebar mark</div>
    <div class="mfield">${swatch}<span class="msel${selOpen ? ' selopen' : ''}">${select(mode)}</span></div></div>`;
};
const dashFields = (name, group, mark, { nameFocus = false } = {}) => `<div class="field"><div class="lab">name</div><span class="input${nameFocus ? ' focus' : ''}">${nameFocus ? `<span style="background:var(--selection)">${name}</span><span class="caret" style="height:13px"></span>` : name}</span></div>
    <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px">${markField(mark)}<div class="field"><div class="lab">group</div>${select(group)}</div></div>`;

// the selected view's settings start with its name: the field's label carries
// where it is (view 2 of 4), so no separate title row
const viewNameField = (n, pos = '') => `<div class="field"><div class="lab">view name${pos ? `<span class="st faint">${pos}</span>` : ''}</div><span class="input">${n}</span></div>`;

// A host list's mode switch (topic 15), the VIEW CONTROLS rule: a small
// two-option segmented control in a one-view header (as downtimes' timeline |
// list), in the view's ··· when stacked, a row in the editor (host lists only).
const HOSTS_AS = ['rows', 'with services'];
const hostsAsSeg = (mode, { compact = false } = {}) => `<span class="seg${compact ? ' cmp' : ''}" style="${compact ? '' : 'height:26px;'}margin-right:6px">${HOSTS_AS.map((m) => `<span class="${m === mode ? 'on' : ''}"${compact ? '' : ' style="padding:0 12px"'}>${m}</span>`).join('')}</span>`;
const hostsAsField = (mode) => `<div class="field"><div class="lab">hosts as<span class="st faint">host lists only</span></div><span class="seg" style="display:flex">${HOSTS_AS.map((m) => `<span class="${m === mode ? 'on' : ''}" style="flex:1">${m}</span>`).join('')}</span></div>`;

// ---- LAYOUTS (topic 15): named presets of a view's fields, listed by the
// editor's display dropdown and by "add view". Every grouping exists from both
// sides: members grouped by their container (the filter picks members; a
// container shows when it has matching members: "services by host") and
// containers with their members (the filter picks containers; every picked
// container shows, even empty: "hosts with services"). A layout is not a view kind:
// it sets display, lists, group by and hosts as; the dropdown's label is
// derived from those fields, so editing them by hand changes the label.
// [name, icon, description, fields]
const LAYOUTS = [
  ['lists', [
    ['services', 'list', 'one row per service', { display: 'list', lists: 'services', groupBy: 'none' }],
    ['services by host', 'rows-3', 'a band per host, its services under it', { display: 'list', lists: 'services', groupBy: 'host' }],
    ['services by host group', 'folder', 'a band per host group, its services', { display: 'list', lists: 'services', groupBy: 'host group' }],
    ['services by service group', 'tag', 'a band per service group, its services', { display: 'list', lists: 'services', groupBy: 'service group' }],
    ['hosts', 'server', 'one row per host', { display: 'list', lists: 'hosts', groupBy: 'none', hostsAs: 'rows' }],
    ['hosts with services', 'layout-list', 'every picked host, its services under it', { display: 'list', lists: 'hosts', groupBy: 'none', hostsAs: 'with services' }],
    ['hosts by host group', 'folder-open', 'a band per host group, its hosts', { display: 'list', lists: 'hosts', groupBy: 'host group' }],
    ['host groups', 'layers', 'every picked host group, its hosts', { display: 'list', lists: 'host groups' }],
    ['service groups', 'box', 'every picked service group, its hosts', { display: 'list', lists: 'service groups' }],
  ]],
  ['overviews', [
    ['host-group grid', 'layout-grid', 'hosts as squares, by group', { display: 'grid' }],
    ['summary tiles', 'chart-bar', 'counts per group', { display: 'tiles' }],
  ]],
  ['activity · not counted in the sidebar, never notify', [
    ['event stream', 'activity', 'changes, acks, downtimes', { display: 'stream' }],
    ['handling', 'users', 'who is handling what', { display: 'handling' }],
    ['downtimes', 'calendar-clock', 'in effect and upcoming', { display: 'downtimes' }],
  ]],
];
const LAYOUT_ICON = Object.fromEntries(LAYOUTS.flatMap(([, ls]) => ls.map(([n, ic]) => [n, ic])));
// the layout menu: full (the display dropdown: tick, icon, name, description)
// or compact ("add view": icon and name; the focused entry's description in a tooltip)
function layoutMenu({ x, y, w, current = '', focus = '', compact = false, title = '' }) {
  const items = [];
  if (title) items.push(title);
  LAYOUTS.forEach(([sec, ls], i) => {
    if (i) items.push('-');
    items.push(compact ? sec.replace('not counted in the sidebar, never notify', 'not counted, never notify') : sec);
    ls.forEach(([n, ic, det]) => items.push({ label: n, ic, ...(compact ? {} : { det, chk: n === current }), hov: n === focus }));
  });
  return menu(items, { x, y, w });
}
