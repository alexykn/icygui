// icygui v1 mock-ups: a small kit of the app's building blocks (sidebar,
// list rows, pane parts, buttons, menus, dialogs) as HTML strings, so every
// topic draws the same chrome the same way. Names follow ic-ui-kit.
//
// Usage in a topic page:
//   shot('1a', '01-downtimes-a-fixed', 'Label shown above the frame', html, { theme: 'dark' })
// render.js screenshots every [data-shot] element at 2x.

const NOW = '14:12'; // the mock-ups' wall clock: Wednesday 2026-10-07 14:12

// ---- data: the demo's prod-cluster --------------------------------------

const SIDEBAR_GROUPS = [
  { name: 'overview', items: [['overview', 'crit', 38], ['production', 'crit', 38], ['databases', 'crit', 7], ['host problems', 'crit', 7]] },
  { name: 'platform', items: [['network', 'unk', 5], ['kubernetes', 'crit', 9], ['certificates', 'warn', 1], ['all services', 'crit', 38]] },
  { name: 'lab', items: [['sandbox', 'pend', '']] },
];

// [state, service, host, output, since, extra]
const ROWS = [
  ['crit', 'rabbitmq-queue', 'mq-prod-01', 'CRITICAL - queue orders.retry depth 18,402', '6m'],
  ['crit', 'uplink edge-ams', 'sw-core-ams-02', 'CRITICAL - Interface Po12 (uplink edge-ams) is down', '11m'],
  ['crit', 'postgres-replication', 'db-prod-03', 'CRITICAL - standby lag 412s (> 300s)', '14m'],
  ['crit', 'disk /var', 'k8s-node-07', 'DISK CRITICAL - /var 97% used (1.2 GiB free)', '38m'],
  ['unk', 'kubelet', 'k8s-node-04', 'UNKNOWN - connection refused (10.0.4.24:10250)', '4m'],
  ['unk', 'borg-last-run', 'backup-01', 'UNKNOWN - repository lock held by PID 4412', '52m'],
  ['unk', 'kafka-consumer-lag', 'kafka-01', 'UNKNOWN - plugin timed out after 60 seconds', '54m'],
  ['unk', 'minio-health', 'minio-02', 'UNKNOWN - no data received from agent', '1h'],
  ['unk', 'mysql-replication', 'db-mysql-03', 'UNKNOWN - connection refused', '2h'],
  ['unk', 'smart-disks', 'store-fra-01', 'UNKNOWN - no data received from agent', '2h'],
  ['warn', 'haproxy-backend', 'lb-prod-02', 'WARNING - backend api: 2/6 servers down', '9m'],
  ['warn', 'http-latency', 'api-gw-01', 'WARNING - p95 1.84s (> 1.5s)', '21m'],
  ['warn', 'pg-connections', 'db-prod-03', 'WARNING - 182 of 200 per-db limit (orders)', '22m'],
  ['warn', 'pg-bloat', 'db-prod-05', 'WARNING - check_postgres degraded', '50m'],
  ['warn', 'load', 'db-prod-01', 'WARNING - load average 14.2, 12.8, 11.1', '1h'],
];

const STATE_WORD = { crit: 'critical', warn: 'warning', unk: 'unknown', ok: 'ok', pend: 'pending', down: 'down', up: 'up', unr: 'unreachable' };
const STATE_SHORT = { crit: 'CRIT', warn: 'WARN', unk: 'UNKN', ok: 'OK', pend: 'PEND' };

// ---- small parts -----------------------------------------------------

const esc = (s) => String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const dot = (st, cls = '') => `<span class="dot ${st} ${cls}"></span>`;
const circle = (st, size = 22, ring = false) => `<span class="circ c${size} ${st}${ring ? ' ring' : ''}"></span>`;
const kh = (k) => `<span class="kh">${k}</span>`;

function btn(label, { key, primary, danger, dis, hov, icon: ic, sm, cls = '' } = {}) {
  const c = ['btn', primary && 'primary', danger && 'danger', dis && 'dis', hov && 'hov', sm && 'sm', cls].filter(Boolean).join(' ');
  return `<span class="${c}">${ic ? icon(ic, 13) : ''}${label}${key ? `<span class="kh">${key}</span>` : ''}</span>`;
}
const chip = (label, { sel, filled, marked } = {}) => `<span class="chip${sel ? ' sel' : ''}${filled ? ' filled' : ''}${marked ? ' marked' : ''}">${label}</span>`;
const sw = (on, label = '') => `<span class="sw${on ? ' on' : ''}"><span class="trk"></span>${label}</span>`;
const seg = (opts, on) => `<span class="seg">${opts.map((o, i) => `<span class="${i === on ? 'on' : ''}">${o}</span>`).join('')}</span>`;
const select = (value, w) => `<span class="select" style="${w ? `width:${w}px` : ''}"><span>${value}</span>${icon('chevron-down', 13)}</span>`;
const seclabel = (t) => `<span class="seclabel">${t}</span>`;

// ---- window chrome ---------------------------------------------------

function trafficLights(off = false) {
  return off
    ? `<span class="tl off"></span><span class="tl off"></span><span class="tl off"></span>`
    : `<span class="tl c"></span><span class="tl m"></span><span class="tl z"></span>`;
}

// slots: reserve the fixed slot for the notification-times mark (topic 12)
// after each group's name and before each dashboard's count; g.mark and an
// item's 5th field fill it.
function sidebar({ activeGroup = 'overview', activeItem = 'overview', groups = SIDEBAR_GROUPS, foot = {}, after = '', before = '', search = '', slots = false, noFoot = false } = {}) {
  let html = `<div class="sb">
    <div class="sb-top">${trafficLights(foot.inactive)}<span class="vdiv"></span><span class="muted">${icon('search', 13)}</span>
      <span class="sb-search">${search || 'Search dashboards…'}</span></div>${before}`;
  for (const g of groups) {
    const on = g.name === activeGroup;
    html += `<div class="sb-group"><div class="sb-gh${on ? ' active' : ''}"><span class="name">${g.name}</span>${slots ? `<span class="nslot">${g.mark || ''}</span>` : ''}
      ${on ? `<span class="chev">${icon('chevron-down', 12)}</span><span class="grow"></span><span class="gicons">${icon('plus', 14)}<span class="${g.menu ? 'glyph sel' : ''}" style="letter-spacing:1px;font-size:13px">···</span></span>` : ''}</div>`;
    for (const it of g.items) {
      const [label, st, count, extra, mark] = it;
      const act = on && label === activeItem;
      html += `<div class="sb-item${act ? ' active' : ''}">${dot(st)}<span class="label">${label}</span>${extra || ''}${slots ? `<span class="nslot">${mark || ''}</span>` : ''}<span class="count">${count}</span></div>`;
    }
    html += `</div>`;
  }
  html += after;
  html += `<div style="flex:1"></div>${noFoot ? '' : sidebarFoot(foot)}</div>`;
  return html;
}

function sidebarFoot({ badge, open, env = 'prod-cluster', node = 'master-01', age = '0s', health = 'ok', bell, plusOpen = false } = {}) {
  return `<div class="sb-foot">
    <span class="ibtn">${icon('panel-left', 16)}</span>
    <span class="ibtn">${icon(bell ? 'bell-off' : 'clock', 16)}${badge ? `<span class="badge">${badge}</span>` : ''}</span>
    <span class="status${open ? ' open' : ''}">${dot(health, 'd6')}<span class="env">${env}</span><span class="faint">${node}</span><span class="faint">${age}</span><span class="faint chev">${icon('chevron-down', 10)}</span></span>
    <span class="grow"></span><span class="ibtn${plusOpen ? ' sel' : ''}">${icon('plus', 16)}</span></div>`;
}

// the sort is sized to its current word (the view-header rule)
function listHeader({ title = 'overview', subtitle = 'service problems', demo = false, sort = 'severity ↓', extra = '', more = true } = {}) {
  return `<div class="hbar"><span class="title">${title}</span><span class="subtitle">${subtitle}</span><span class="grow"></span>
    ${extra}${demo ? '<span class="demo-badge">demo</span>' : ''}${sort ? `<span class="right">${sort}</span>` : ''}${more ? '<span class="glyph">···</span>' : ''}</div>`;
}

// The summary bar's handled slot (rc1's "N handled hidden" toggle, now a
// button in a fixed, right-aligned slot): hiding, "28 hidden · show"; after a
// click, "28 handled · hide". The view saves the choice, as rc1 did.
function handledEnd(n, shown = false) {
  return `<span class="hend">${shown ? `${n} handled` : `${n} hidden`}<span class="faint"> · </span><span class="hbtn">${shown ? 'hide' : 'show'}</span></span>`;
}
function summary(items = [['crit', 4, 'critical'], ['warn', 28, 'warning'], ['unk', 6, 'unknown']], end = handledEnd(28)) {
  return `<div class="sum">${items.map(([st, n, w]) => `<span class="it">${dot(st, 'd9')}${n} ${w}</span>`).join('')}<span class="end">${end}</span></div>`;
}

// A dashboard list row. r: [state, name, host, output, since] plus opts.
function row(r, { sel, marked, hover, handled, tag = '', late, cls = '', hostRow, compact = false } = {}) {
  const [st, name, host, output, since] = r;
  const title = hostRow ? `<span class="n">${name}</span>` : `<span class="n">${esc(name)}</span><span class="on"> on </span><span class="h">${host}</span>`;
  const tagHtml = (late ? `<span class="late">${late}</span>` : '') + (tag ? `<span style="display:flex;align-items:center;gap:10px">${tag}</span>` : '');
  // compact (row density, Settings → appearance or the view's own): one line,
  // the 14px circle, the time at the right
  if (compact) return `<div class="row cmp${sel ? ' sel' : ''}${marked ? ' marked' : ''}${hover ? ' hover' : ''} ${cls}">
    <div class="lead">${circle(st, 14, handled)}</div>
    <div class="text"><span class="t1">${title}</span></div>
    <span class="tag">${tagHtml}<span class="since">${since}</span></span></div>`;
  return `<div class="row${sel ? ' sel' : ''}${marked ? ' marked' : ''}${hover ? ' hover' : ''} ${cls}">
    <div class="lead">${circle(st, 22, handled)}<span class="since">${since}</span></div>
    <div class="text"><span class="t1">${title}</span><span class="t2">${esc(output)}</span></div>
    <span class="tag">${tagHtml}</span></div>`;
}

function rows(list = ROWS, opts = {}) {
  return list.map((r, i) => row(r, typeof opts === 'function' ? opts(r, i) : opts[i] || {})).join('');
}

// The pane's × sits LEFT of "↗ open as tab", in a fixed slot, on every
// platform, so it never sits next to a window's close button (topic 13). A
// pane that is already a tab keeps the tab slot, empty, so the × never moves.
function paneHeader(kind = 'service', { back = false, tab = true } = {}) {
  return `<div class="hbar pane-h">${back ? `<span class="muted">${icon('arrow-left', 13)}</span>` : ''}<span class="label">${kind}</span><span class="grow"></span>
    <span class="x">×</span><span class="quiet"${tab ? '' : ' style="visibility:hidden"'}>↗ open as tab</span></div>`;
}

function paneTitle({ st = 'crit', label = 'CRIT', ring = false, name, sub }) {
  return `<div class="ptitle"><div class="lead">${circle(st, 34, ring)}<span class="slabel c-${st}">${label}</span></div>
    <div class="col" style="gap:4px;min-width:0"><span class="name">${name}</span><span class="sub">${sub}</span></div></div>`;
}

function actionButtons({ first = 'acknowledge', firstPrimary = true, firstDis = false, keys = true } = {}) {
  return `<div class="btns">${btn(first, { key: keys ? 'a' : '', primary: firstPrimary && !firstDis, dis: firstDis })}${btn('downtime', { key: keys ? 'd' : '' })}${btn('check now', { key: keys ? 'r' : '' })}${btn('comment', { key: keys ? 'c' : '' })}<span class="glyph">···</span></div>`;
}

function kv(title, pairs, keyWidth = 140) {
  return `<div class="kv">${title ? `<div class="ttl">${seclabel(title)}</div>` : ''}${pairs.map(([k, v]) => `<div class="r" style="grid-template-columns:${keyWidth}px minmax(0,1fr)"><span>${k}</span><span>${v}</span></div>`).join('')}</div>`;
}

function perf(rowsData) {
  return `<div class="perf"><div class="r h"><span>performance data</span><span>value</span><span>warn</span><span>crit</span></div>${rowsData.map(([k, v, w, c, st]) => `<div class="r"><span class="sec">${k}</span><span style="color:${st ? `var(--${st}-text)` : 'var(--t)'}">${v}</span><span class="faint">${w}</span><span class="faint">${c}</span></div>`).join('')}</div>`;
}

function codeBlock(text, label = 'plugin output') {
  return `<div class="col" style="gap:8px">${label ? seclabel(label) : ''}<div class="code">${text}</div></div>`;
}

function note({ mk = '#', author, meta = [], body }) {
  return `<div class="note"><span class="mk">${mk}</span><div class="col" style="gap:4px"><div class="hd"><span class="au">${author}</span>${meta.map((m) => `<span class="meta">${m}</span>`).join('')}</div><span>${body}</span></div></div>`;
}

// ---- ONE dropdown system (README "Dropdowns") --------------------------
// An entry is its icon (where the menu has icons) and its name, nothing else:
// no descriptions (user, 2026-10-07). The check slot sits at the right, so it
// never pushes the text; when any item has one, all get it.
function mItem(it, slots, { current = '', focus = '' } = {}) {
  if (it === '-') return '<div class="msep"></div>';
  if (typeof it === 'string') return `<div class="mlabel">${it}</div>`;
  const { label, key, sel, dis, chk, dotSt, ic, right } = it;
  const hov = it.hov || (focus && label === focus);
  const ticked = chk || (current && label === current);
  return `<div class="mi${sel ? ' sel' : ''}${hov ? ' hov' : ''}${dis ? ' dis' : ''}">${dotSt ? dot(dotSt, 'd6') : ''}${ic ? `<span class="mic">${icon(ic, 13)}</span>` : ''}<span class="ml">${label}</span>${right || ''}${key ? kh(key) : ''}${slots ? `<span class="chk">${ticked ? icon('check', 13) : ''}</span>` : ''}</div>`;
}
// An ACTION MENU (···, add view, header menus): anchored to its trigger's edge
// with a 4px gap (anchor: a CSS selector for the trigger inside the same
// frame; align: 'right' for right-side triggers; up: opens above), sized to
// its longest item (180 to 280). The trigger is drawn pressed by its owner.
// Without an anchor (older rounds) x, y and w place it as before.
function menu(items, { x, y, w, style = '', anchor = '', align = 'left', up = false, compact = false } = {}) {
  const slots = items.some((it) => typeof it === 'object' && it.chk !== undefined);
  const body = items.map((it) => mItem(it, slots)).join('');
  const pos = anchor ? 'left:0;top:0;' : `left:${x}px;top:${y}px;${w ? `width:${w}px;` : ''}`;
  return `<div class="menu${anchor ? ' am' : ''}${compact ? ' cmp' : ''}"${anchor ? ` data-anchor="${anchor}" data-align="${align}"${up ? ' data-up="1"' : ''}` : ''} style="${pos}${style}">${body}</div>`;
}
// A SELECT, open: the field and its list as one shape (the list opens from
// the field, exactly as wide, joined to its bottom edge, or its top edge when
// up). items: names, { label, ic, dis }, { section: 'word' }, '-'. The current
// value is ticked; focus is the highlighted row. Max 10 rows, then it scrolls.
function selectOpen(value, items, { current = '', focus = '', up = false, compact = false } = {}) {
  const rowsHtml = items.map((it) => (it === '-' ? mItem('-') : it.section ? mItem(it.section) : mItem(typeof it === 'string' ? { label: it } : it, true, { current, focus }))).join('');
  return `<span class="selwrap"><span class="select open${up ? ' up' : ''}"><span style="min-width:0;overflow:hidden;text-overflow:ellipsis">${value}</span>${icon('chevron-down', 13)}</span><div class="sellist${up ? ' up' : ''}${compact ? ' cmp' : ''}">${rowsHtml}</div></span>`;
}
// place anchored menus by their triggers and give overflowing lists a thumb
function anchorMenus() {
  document.querySelectorAll('.menu[data-anchor]').forEach((m) => {
    const frame = m.closest('[data-shot]'); const trig = frame && frame.querySelector(m.dataset.anchor);
    if (!trig) { console.error('menu trigger not found: ' + m.dataset.anchor); return; }
    const op = m.offsetParent.getBoundingClientRect(), t = trig.getBoundingClientRect(), r = m.getBoundingClientRect();
    m.style.left = Math.round((m.dataset.align === 'right' ? t.right - r.width : t.left) - op.left) + 'px';
    m.style.top = Math.round((m.dataset.up ? t.top - 4 - r.height : t.bottom + 4) - op.top) + 'px';
  });
  // a tooltip beside an item (a cut-off name in a select), placed after the menus
  document.querySelectorAll('.tip[data-anchor]').forEach((tp) => {
    const frame = tp.closest('[data-shot]'); const trig = frame && frame.querySelector(tp.dataset.anchor);
    if (!trig) { console.error('tooltip anchor not found: ' + tp.dataset.anchor); return; }
    const op = tp.offsetParent.getBoundingClientRect(), t = trig.getBoundingClientRect(), r = tp.getBoundingClientRect();
    if (tp.dataset.side === 'top') { // above the trigger, right edges aligned (a status bar symbol)
      tp.style.left = Math.round(t.right - r.width - op.left) + 'px'; tp.style.top = Math.round(t.top - 6 - r.height - op.top) + 'px'; return;
    }
    const left = tp.dataset.side === 'left' ? t.left - 8 - r.width : t.right + 8;
    tp.style.left = Math.round(left - op.left) + 'px'; tp.style.top = Math.round(t.top + (t.height - r.height) / 2 - op.top) + 'px';
  });
  document.querySelectorAll('.sellist').forEach((l) => {
    if (l.scrollHeight <= l.clientHeight + 1 || l.querySelector('.sthumb')) return;
    const th = document.createElement('span'); th.className = 'sthumb';
    th.style.top = '4px'; th.style.height = Math.round((l.clientHeight - 8) * l.clientHeight / l.scrollHeight) + 'px';
    l.appendChild(th);
  });
}
window.addEventListener('load', () => document.fonts.ready.then(anchorMenus));

function modal(width, inner, { top, center = true, style = '' } = {}) {
  const pos = top !== undefined ? `style="padding-top:${top}px"` : '';
  return `<div class="backdrop${top === undefined && center ? ' center' : ''}" ${pos}><div class="modal" style="width:${width}px;${style}">${inner}</div></div>`;
}

function dialog({ title, what = '', body, footStart = '', actions = '' }) {
  return `<div class="dlg-h"><span>${title}</span>${what ? `<span class="what">${what}</span>` : ''}</div>
    <div class="dlg-b">${body}</div>
    <div class="dlg-f"><span class="start">${footStart}</span><span class="grow"></span>${actions}</div>`;
}

function toast({ title, lines = [], tone = 'ok', ic = 'check', x, y, right, bottom }) {
  const pos = right !== undefined ? `right:${right}px;bottom:${bottom}px` : `left:${x}px;top:${y}px`;
  return `<div class="toast" style="--tone:var(--${tone});${pos}"><span style="color:var(--${tone});padding-top:1px">${icon(ic, 14)}</span>
    <div class="col" style="gap:4px;min-width:0"><span class="ti">${title}</span>${lines.map((l) => `<span class="tl2">${l}</span>`).join('')}</div><span class="tx">${icon('x', 12)}</span></div>`;
}

// ---- the canvas -------------------------------------------------------

function shot(id, name, label, html, { theme = 'dark', w = 1440, h = 900, crops = '', note: extra = '' } = {}) {
  const host = document.querySelector('.shots');
  const div = document.createElement('div');
  div.className = 'shot';
  div.innerHTML = `<div class="shot-label"><span class="shot-id">${id}</span><span>${label}</span></div>
    <div class="frame ${theme}" data-shot="${name}" ${crops ? `data-crops="${crops}"` : ''} style="width:${w}px;height:${h}px">${html}</div>${extra}`;
  host.appendChild(div);
  return div;
}

// Window: sidebar + main content.
// statusBar (topic 16, NEW): the footer becomes a slim full-width status bar
// across the window's bottom (Zed-like): { foot, diag, log, hov } where diag is
// the active engine's state ('ok' | 'warn' | 'crit') and hov the hovered symbol
function appWindow(main, sb = {}) {
  if (sb.statusBar) return `<div class="win sbar">${sidebar({ ...sb, noFoot: true })}<div class="col" style="min-height:0">${main}</div>${statusBar(sb.statusBar === true ? {} : sb.statusBar, sb.foot)}</div>`;
  return `<div class="win">${sidebar(sb)}<div class="col" style="min-height:0">${main}</div></div>`;
}
// LEFT as today's footer (sidebar toggle, history, the environment switcher,
// +); RIGHT small symbols with tooltips: the active environment's engine
// (gauge, dot = its state) and the app's diagnostics (monitor, as the page's
// 'app' view); both open the one
// diagnostics page (the engine with its row selected, the log with nothing
// selected); room for later ones
function statusBar({ diag = 'ok', hov = '', diagOpen = false, logOpen = false, appIcon = '' } = {}, foot = {}) {
  const { badge, open, env = 'prod-cluster', node = 'master-01', age = '0s', health = 'ok', bell } = foot;
  const sym = (ic, name, { st = '', on = false, h = false } = {}) => `<span class="ibtn stb${on || h ? ' sel' : ''}" data-sym="${name}">${ic.startsWith('<svg') ? ic : icon(ic, 14)}${st && st !== 'ok' ? `<span class="sdot ${st}"></span>` : st ? `<span class="sdot ok"></span>` : ''}</span>`;
  return `<div class="statusbar">
    <span class="ibtn stb">${icon('panel-left', 14)}</span>
    <span class="ibtn stb">${icon(bell ? 'bell-off' : 'clock', 14)}${badge ? `<span class="badge">${badge}</span>` : ''}</span>
    <span class="status${open ? ' open' : ''}">${dot(health, 'd6')}<span class="env">${env}</span><span class="faint">${node}</span><span class="faint">${age}</span><span class="faint chev">${icon('chevron-down', 10)}</span></span>
    <span class="ibtn stb">${icon('plus', 14)}</span>
    <span class="grow"></span>
    ${sym('gauge', 'diagnostics', { st: diag, on: diagOpen, h: hov === 'diagnostics' })}${sym(appIcon || 'monitor', 'app', { on: logOpen, h: hov === 'app' })}
  </div>`;
}

// ---- the downtime banner (topic 01) ------------------------------------
function dtBanner({ quiet = false, ic = 'calendar-clock', what, left = '', action = '', l2 = '', l3 = '', l4 = '', progress = 0 }) {
  return `<div class="dtb${quiet ? ' quiet' : ''}">
    <div class="l1">${icon(ic, 14)}<span class="what">${what}</span>${left ? `<span class="left">${left}</span>` : ''}<span class="grow"></span>${action}</div>
    ${l2 ? `<div class="l2">${l2}</div>` : ''}${l3 ? `<div class="l3">${l3}</div>` : ''}${l4 ? `<div class="l4">${l4}</div>` : ''}
    <div class="prog"><span style="width:${progress}%"></span></div></div>`;
}
const S = '<span class="sep"> · </span>';
const au = (who, at, text) => `<span style="color:var(--t)">${who}</span> <span class="faint">${at}</span> ${text}`;
function dte({ ic = 'calendar-clock', d1, au, meta = '', body, hov }) {
  return `<div class="dte">${icon(ic, 13)}<div class="col" style="min-width:0"><span class="d1">${d1}</span><span class="d2"><span class="au">${au}</span> <span class="meta">${meta}</span> ${body}</span></div><span class="rm${hov ? ' hov' : ''}">${icon('x', 12)}</span></div>`;
}

// ---- the sidebar's cluster section (topic 14, round 3; used by 06 and 14):
// fixed at the top, above the groups: the environment's handling, downtimes
// and events (the same views without a filter, round 5) and cluster health, each with its mark in the dot slot and its count in the
// count slot (health: the cluster's state dot, no count).
const sbIconSlot = (ic) => `<span class="faint" style="width:8px;display:flex;justify-content:center">${icon(ic, 11)}</span>`;
// env: the environment the section belongs to
const clusterSection = ({ active = '', health = 'ok', env = 'prod-cluster' } = {}) => `<div class="sb-group" style="padding:0 0 6px;margin-bottom:6px;border-bottom:1px solid var(--bd-header)">
  <div class="sb-gh"><span class="name">cluster</span><span class="faint" style="font-size:12px">${env}</span></div>
  <div class="sb-item${active === 'handling' ? ' active' : ''}">${sbIconSlot('users')}<span class="label">handling</span><span class="count">20</span></div>
  <div class="sb-item${active === 'downtimes' ? ' active' : ''}">${sbIconSlot('calendar-clock')}<span class="label">downtimes</span><span class="count">5</span></div>
  <div class="sb-item${active === 'events' ? ' active' : ''}">${sbIconSlot('activity')}<span class="label">events</span><span class="count"></span></div>
  <div class="sb-item${active === 'health' ? ' active' : ''}">${dot(health)}<span class="label">health</span><span class="count"></span></div></div>`;
// platform; with round 3's group entries ('both', 'handling'), or a plain folder ('none', round 5)
const platformGroup = (on = 'both') => ({ name: 'platform', items: [...(on === 'both' || on === 'handling' ? [['handling', 'ic-users', 6]] : []), ...(on === 'both' ? [['downtimes', 'ic-calendar-clock', 3]] : []), ['network', 'unk', 5], ['kubernetes', 'crit', 9], ['certificates', 'warn', 1], ['all services', 'crit', 38]] });
const clusterGroups = (on = 'both') => [SIDEBAR_GROUPS[0], platformGroup(on), SIDEBAR_GROUPS[2]];
// a group's own entries carry an icon in the mark slot instead of a state dot
const sbWithIcons = (html) => html.replace(/<span class="dot ic-([a-z-]+) *"><\/span>/g, (m, n) => sbIconSlot(n));
// the main window with the cluster section and the round-3 groups
const clusterWindow = (main, { cluster = '', health = 'ok', activeGroup = '', activeItem = '', on = 'none', menuOn = false, foot = {}, env = 'prod-cluster', statusBar = null } = {}) => sbWithIcons(appWindow(main, {
  groups: clusterGroups(on).map((g) => (menuOn && g.name === 'platform' ? { ...g, menu: true } : g)), activeGroup, activeItem, foot, statusBar, before: clusterSection({ active: cluster, health, env }) }));

// ---- host bands: ONE component for every host-with-services view ----------
// 10h/10j/10k/10l (a service list grouped by host, the combined view) and 15
// (a host list "with services") draw their hosts with these, so they cannot
// drift apart. A host is a group-header band (36px, compact 30px; the
// collapse chevron at its left, the state mark in the rows' mark column, the
// name, the address and the output faint, the per-state counts of the
// services under it at the right); its services are ordinary list rows,
// paged by count (HOST_PREVIEW, then "+ N more" / "− show fewer").
//   h: { name, addr, out, counts, total, problems, okRows,
//        st ('ok' by default), ring (handled: hollow), word (a host state
//        word before the output, e.g. "down 3m", in the state's text colour),
//        note (a faint line instead of the rows: "4 services hidden · host
//        down", "no services", "no hosts", "all ok"), none (nothing under
//        it: no chevron), members ('hosts' for a host group's band) }
// The same band is a host group's or a service group's (topic 15's
// container lists, 10's grouped lists): name, a faint count, the counts.
//   problems: [state, service, output, since, { handled, tag }]
//   okRows: rows or a function returning [state, service, output, since]
const HOST_PREVIEW = 7;
function hostBand(h, { marked, sel, collapsed, sticky, compact } = {}) {
  const c = (h.counts || []).map(([st, n]) => `<span>${dot(st, 'd7')}${n}</span>`).join('');
  const st = h.st || 'ok';
  const word = h.word ? `<span style="color:var(--${st}-text)">${h.word}</span> · ` : '';
  return `<div class="ghb${compact ? ' cmp' : ''}${marked ? ' marked' : ''}${sel ? ' sel' : ''}${sticky ? ' sticky' : ''}"><span class="chev">${h.none ? '' : icon(collapsed ? 'chevron-right' : 'chevron-down', 12)}</span><div class="lead">${dot(st, h.ring ? 'ring' : '')}</div>
    <div class="t"><span class="n">${h.name}</span><span class="a">${h.addr}</span><span class="o">${word}${h.out}</span></div>
    <span class="cs">${c}</span></div>`;
}
// The rows of one host: problems (never paged away), then OK ones up to the
// preview, then the paging row. expanded: every service and "− show fewer".
// marked: 'problems' (every problem row) or a list of service names.
function hostRows(h, { marked = '', expanded = false, from = 0, compact = false } = {}) {
  if (h.note) return `<div class="morerow hnote${compact ? ' cmp' : ''}"><span></span><span>${h.note}</span></div>`;
  const oks = typeof h.okRows === 'function' ? h.okRows() : (h.okRows || []);
  const keep = expanded ? h.total : Math.min(h.total, Math.max(HOST_PREVIEW, h.problems.length));
  const list = h.problems.concat(oks).slice(0, keep).slice(from);
  const isMarked = (n, st) => (marked === 'problems' ? st !== 'ok' : Array.isArray(marked) && marked.includes(n));
  // members: a host's services ("service on host"), a host group's hosts
  // (host rows; h.members = 'hosts') or a service group's services (x.host)
  let out = list.map(([st, n, o, t, x = {}]) => row([st, n, x.host || h.name, o, t], { marked: isMarked(n, st), handled: x.handled, tag: x.tag || '', compact, hostRow: h.members === 'hosts' })).join('');
  const label = expanded ? '− show fewer' : `+ ${h.total - keep} more`;
  if (h.total > HOST_PREVIEW) out += `<div class="morerow${compact ? ' cmp' : ''}"><span></span><span>${label}</span></div>`;
  return out;
}

// ---- the host pane (rc1's, as in 10l and 15k): title, actions, the tabs, the
// services paged by count. chip: an optional removable filter at the top of
// the services tab (15k: "service group databases ×"; × shows all again).
function hostPaneView({ title, tab, rows, more = '', chip = '' }) {
  return `<div class="pane">${paneHeader('host')}<div class="col" style="gap:20px;padding:20px 24px 0;flex:none">
  ${paneTitle(title)}
  ${actionButtons({ firstDis: true })}
  <div class="subtabs"><span class="on">${tab}</span><span>history</span><span>vars</span><span>config</span></div></div>
  <div class="col">${chip ? `<div style="display:flex;padding:12px 24px 4px">${chip}</div>` : ''}${rows.map(([st, n, o, t]) => `<div class="crow">${circle(st, 14)}<div class="col" style="gap:2px;min-width:0"><span class="n">${n}</span><span class="o">${o}</span></div><span class="s">${t}</span></div>`).join('')}
  ${more ? `<div style="padding:10px 24px 12px;font-size:12px;color:var(--t-faint)">${more}</div>` : ''}</div></div>`;
}
