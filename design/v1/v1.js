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
function sidebar({ activeGroup = 'overview', activeItem = 'overview', groups = SIDEBAR_GROUPS, foot = {}, after = '', before = '', search = '', slots = false } = {}) {
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
  html += `<div style="flex:1"></div>${sidebarFoot(foot)}</div>`;
  return html;
}

function sidebarFoot({ badge, open, env = 'prod-cluster', node = 'master-01', age = '0s', health = 'ok', bell } = {}) {
  return `<div class="sb-foot">
    <span class="ibtn">${icon('panel-left', 16)}</span>
    <span class="ibtn">${icon(bell ? 'bell-off' : 'clock', 16)}${badge ? `<span class="badge">${badge}</span>` : ''}</span>
    <span class="status${open ? ' open' : ''}">${dot(health, 'd6')}<span class="env">${env}</span><span class="faint">${node}</span><span class="faint">${age}</span><span class="faint chev">${icon('chevron-down', 10)}</span></span>
    <span class="grow"></span><span class="ibtn">${icon('plus', 16)}</span></div>`;
}

function listHeader({ title = 'overview', subtitle = 'service problems', demo = false, sort = 'severity ↓', extra = '', more = true } = {}) {
  return `<div class="hbar"><span class="title">${title}</span><span class="subtitle">${subtitle}</span><span class="grow"></span>
    ${extra}${demo ? '<span class="demo-badge">demo</span>' : ''}${sort ? `<span class="right">${sort}</span>` : ''}${more ? '<span class="glyph">···</span>' : ''}</div>`;
}

function summary(items = [['crit', 4, 'critical'], ['warn', 28, 'warning'], ['unk', 6, 'unknown']], end = '28 handled hidden') {
  return `<div class="sum">${items.map(([st, n, w]) => `<span class="it">${dot(st, 'd9')}${n} ${w}</span>`).join('')}<span class="end">${end}</span></div>`;
}

// A dashboard list row. r: [state, name, host, output, since] plus opts.
function row(r, { sel, marked, hover, handled, tag = '', late, cls = '', hostRow } = {}) {
  const [st, name, host, output, since] = r;
  const title = hostRow ? `<span class="n">${name}</span>` : `<span class="n">${esc(name)}</span><span class="on"> on </span><span class="h">${host}</span>`;
  const tagHtml = (late ? `<span class="late">${late}</span>` : '') + (tag ? `<span style="display:flex;align-items:center;gap:10px">${tag}</span>` : '');
  return `<div class="row${sel ? ' sel' : ''}${marked ? ' marked' : ''}${hover ? ' hover' : ''} ${cls}">
    <div class="lead">${circle(st, 22, handled)}<span class="since">${since}</span></div>
    <div class="text"><span class="t1">${title}</span><span class="t2">${esc(output)}</span></div>
    <span class="tag">${tagHtml}</span></div>`;
}

function rows(list = ROWS, opts = {}) {
  return list.map((r, i) => row(r, typeof opts === 'function' ? opts(r, i) : opts[i] || {})).join('');
}

function paneHeader(kind = 'service', { back = false, tab = true } = {}) {
  return `<div class="hbar pane-h">${back ? `<span class="muted">${icon('arrow-left', 13)}</span>` : ''}<span class="label">${kind}</span><span class="grow"></span>
    ${tab ? `<span class="quiet">↗ open as tab</span>` : ''}<span class="x">×</span></div>`;
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

function menu(items, { x, y, w, style = '' } = {}) {
  // As ic-ui-kit's Menu: when any item has a check slot, all get one.
  const slots = items.some((it) => typeof it === 'object' && it.chk !== undefined);
  const body = items.map((it) => {
    if (it === '-') return '<div class="msep"></div>';
    if (typeof it === 'string') return `<div class="mlabel">${it}</div>`;
    const { label, key, sel, hov, dis, chk, det, dotSt, ic, right } = it;
    return `<div class="mi${sel ? ' sel' : ''}${hov ? ' hov' : ''}${dis ? ' dis' : ''}">${slots ? `<span class="chk">${chk ? icon('check', 13) : ''}</span>` : ''}${dotSt ? dot(dotSt, 'd6') : ''}${ic ? `<span class="muted">${icon(ic, 12)}</span>` : ''}<span${det ? '' : ' class="grow"'}>${label}</span>${det ? `<span class="det grow">${det}</span>` : ''}${right || ''}${key ? kh(key) : ''}</div>`;
  }).join('');
  return `<div class="menu" style="left:${x}px;top:${y}px;${w ? `width:${w}px;` : ''}${style}">${body}</div>`;
}

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
function appWindow(main, sb = {}) {
  return `<div class="win">${sidebar(sb)}<div class="col" style="min-height:0">${main}</div></div>`;
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
