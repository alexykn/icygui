// Multi-view dashboards (topic 04), shared with the host-group grid (05).

const DISPLAY_ICON = { list: 'list', grouped: 'rows-3', grid: 'layout-grid', tiles: 'chart-bar', stream: 'activity' };
const DISPLAY_NAME = { list: 'list', grouped: 'grouped list', grid: 'host-group grid', tiles: 'summary tiles', stream: 'event stream' };

// A view's header: collapse chevron, display icon, name, filter summary,
// counts, its own sort and ···.
function viewHeader({ name, display = 'list', filter = '', counts = [], sort = 'severity ↓', collapsed = false, focus = false, empty = '', live = false, sortOpen = false, more = true }) {
  const c = counts.map(([st, n]) => `<span>${dot(st, 'd7')}${n}</span>`).join('');
  return `<div class="vh${focus ? ' focus' : ''}"><span class="chev">${icon(collapsed ? 'chevron-right' : 'chevron-down', 12)}</span>
    <span class="vicon">${icon(DISPLAY_ICON[display], 13)}</span><span class="vn">${name}</span>
    <span class="vf">${esc(filter)}</span>
    ${empty ? `<span class="empty">${empty}</span>` : ''}${c ? `<span class="vc">${c}</span>` : ''}
    ${live ? `<span style="display:flex;align-items:center;gap:6px;color:var(--t-faint)">${dot('ok', 'd6')}live</span>` : ''}
    ${sort ? `<span class="${sortOpen ? 'glyph sel' : ''}" style="font-size:12px">${sort}</span>` : ''}${more ? '<span class="glyph">···</span>' : ''}</div>`;
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
