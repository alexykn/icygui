// The cluster health page (topic 06), shared with topic 16 (which adds the
// heartbeat row and the Icinga health alerts, item E). With the default
// options it draws exactly topic 06's page.
function spark(points, w = 132, h = 22, color = 'var(--t-faint)', last = 'var(--accent)') {
  const max = Math.max(...points), min = Math.min(...points);
  const xs = points.map((_, i) => (i / (points.length - 1)) * (w - 4) + 2);
  const ys = points.map((p) => h - 3 - ((p - min) / (max - min || 1)) * (h - 6));
  const path = xs.map((x, i) => `${i ? 'L' : 'M'}${x.toFixed(1)},${ys[i].toFixed(1)}`).join(' ');
  const n = xs.length - 1;
  return `<svg width="${w}" height="${h}" viewBox="0 0 ${w} ${h}"><path d="${path}" fill="none" stroke="${color}" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"/><path d="M${xs[n - 1].toFixed(1)},${ys[n - 1].toFixed(1)} L${xs[n].toFixed(1)},${ys[n].toFixed(1)}" stroke="${last}" stroke-width="2" stroke-linecap="round"/><circle cx="${xs[n].toFixed(1)}" cy="${ys[n].toFixed(1)}" r="2.5" fill="${last}"/></svg>`;
}
function kpi(label, value, sub, points, { cls = '', last, span = 1 } = {}) {
  return `<div class="kpi"${span > 1 ? ` style="grid-column:span ${span}"` : ''}><span class="lb">${label}</span><span class="v ${cls}">${value}</span><span class="ks">${sub}</span>${points ? spark(points, 132, 22, 'var(--t-faint)', last) : '<svg width="132" height="22"></svg>'}</div>`;
}
const er = (st, name, zoneTxt, ver, last, traffic, status, { sel, mine } = {}) =>
  `<div class="er${sel ? ' sel' : ''}">${dot(st, 'd7')}<span><span class="strong">${name}</span></span><span class="muted">${zoneTxt}</span><span class="sec">${ver}</span><span class="sec">${last}</span><span class="muted">${traffic}</span><span class="${st === 'crit' ? 'c-crit' : 'muted'}">${status}</span></div>`;
const zr = (st, name, txt) => `<div class="zr">${dot(st, 'd7')}<span class="zn">${name}</span><span>${txt}</span></div>`;

// heartbeat: '' (none, as 06) or a HEARTBEAT key; alerts: Icinga health
// alerts (topic 16, E) drawn in 06's banner, one system: the worst in full
// (two lines and an action), the others one line each in the same block,
// worst first, since in a fixed slot. stopped: Icinga runs no checks (the
// tiles show it). Nothing changes when these are left out.
// The page is a built-in dashboard (topic 16): its views are the health kinds
// below. pick: the view selected in the editor (its header marked); off: views
// switched off; menuOn: the page's ··· pressed; only: one view's header and
// body (a health kind added to another dashboard).
function healthPage({ broken = false, heartbeat = '', alerts = null, stopped = false, master2 = false, pick = '', off = [], menuOn = false, only = '', beats = null } = {}) {
  const head = `<div class="hbar"><span class="title">cluster health</span><span class="subtitle">prod-cluster · seen from master-01</span><span class="grow"></span>
    <span class="quiet">updated 12s ago · every 30s</span><span class="glyph">···</span></div>`;
  const sum = beats ? summary(beats.sum, 'Icinga r2.14.3-1 · up 41d 6h') : master2
    ? summary([['ok', 2, 'connected'], ['crit', 2, 'not connected']], 'Icinga r2.14.3-1 · up 41d 6h')
    : broken
    ? summary([['ok', 3, 'connected'], ['crit', 1, 'not connected']], 'Icinga r2.14.3-1 · up 41d 6h')
    : summary([['ok', 4, 'connected'], ['pend', 0, 'not connected']], 'Icinga r2.14.3-1 · up 41d 6h');
  const banner = broken ? `<div class="banner" style="--tone:var(--crit);--tint:var(--crit-tint)"><span class="c-crit">${icon('triangle-alert', 14)}</span><div class="col" style="gap:2px;min-width:0">
      <span style="font-size:12.5px;color:var(--t-strong)">sat-fra-01 has not been connected for 3m 12s: zone fra’s results are stale.</span>
      <span class="trunc" style="font-size:12px;color:var(--t-muted)">1,204 checks of 214 hosts in zone fra are late; the relay queue for fra is growing (18,402 messages).</span></div><span class="grow"></span><span class="acc" style="font-size:12px;white-space:nowrap">show the late checks</span></div>` : '';
  const top = beats ? hbSummary(beats.hb) + (alerts ? alertBlock(alerts) : '') : (heartbeat ? heartbeatRow(heartbeat) : '') + (alerts ? alertBlock(alerts) : banner);
  const zonesHdr = viewHeader({ picked: pick === 'zones and endpoints', name: 'zones and endpoints', display: 'list', filter: beats ? '3 zones · 5 endpoints · 2 global zones' : '3 zones · 4 endpoints · 2 global zones', counts: beats ? beats.counts : master2 ? [['ok', 2], ['crit', 2]] : broken ? [['ok', 3], ['crit', 1]] : [['ok', 4]], sort: '', more: false });
  const table = beats ? beatsTable(beats.zones) : `<div class="etab">
    <div class="er h"><span></span><span>endpoint</span><span>zone</span><span>version</span><span>last message</span><span>messages in / out</span><span>status</span></div>
    ${zr('ok', 'master', 'top level · 2 endpoints · checks shared between them')}
    ${er('ok', 'master-01', 'master', 'r2.14.3-1', '0s ago', '412/s · 388/s', 'connected · this node', { sel: true, mine: true })}
    ${master2 ? er('crit', 'master-02', 'master', 'r2.14.3-1', '6m 40s ago', '0/s · 0/s', 'not connected · retrying') : er('ok', 'master-02', 'master', 'r2.14.3-1', '0s ago', '398/s · 405/s', 'connected')}
    ${zr('ok', 'ams', 'parent master · 1 endpoint · 188 hosts')}
    ${er('ok', 'sat-ams-01', 'ams', 'r2.14.3-1', '1s ago', '201/s · 12/s', 'connected')}
    ${zr(broken ? 'crit' : 'ok', 'fra', 'parent master · 1 endpoint · 214 hosts')}
    ${broken ? er('crit', 'sat-fra-01', 'fra', 'r2.14.2-1', '3m 12s ago', '0/s · 0/s', 'not connected · retrying') : er('ok', 'sat-fra-01', 'fra', 'r2.14.2-1', '0s ago', '187/s · 9/s', 'connected · older version')}
    <div class="zr" style="background:transparent;border-bottom:0;height:32px"><span class="faint">${icon('layers', 12)}</span><span>global zones: global-templates, director-global (config only, no endpoints)</span></div>
  </div>`;
  const checksHdr = viewHeader({ picked: pick === 'checks', name: 'checks', display: 'tiles', filter: 'last minute, from /v1/status', sort: '', more: false });
  const bz = broken || !!(beats && beats.lateZone);
  const lat = bz ? [4, 4, 5, 4, 4, 5, 4, 4, 5, 9, 31, 64] : [4, 4, 5, 4, 4, 5, 4, 4, 5, 4, 4, 4];
  const kpisChecks = `<div class="kpis">
    ${stopped ? kpi('active checks / min', '0', 'none since 02:11', [3270, 3290, 3281, 3275, 3288, 3279, 3284, 3280, 3277, 1410, 0, 0], { cls: 'v-bad', last: 'var(--crit)' }) : kpi('active checks / min', bz ? '2,104' : '3,283', bz ? 'of 3,516 objects' : 'of 3,516 objects', bz ? [3270, 3290, 3281, 3275, 3288, 3279, 3284, 3280, 3277, 3012, 2390, 2104] : [3270, 3290, 3281, 3275, 3288, 3279, 3284, 3280, 3277, 3286, 3279, 3283], bz ? { cls: 'v-warn', last: 'var(--warn)' } : {})}
    ${kpi('passive checks / min', '212', '31 senders', [208, 214, 209, 212, 210, 215, 211, 209, 213, 210, 212, 212])}
    ${kpi('average latency', bz ? '0.064s' : '0.004s', bz ? 'max 4.81s' : 'max 0.92s', lat, bz ? { cls: 'v-warn', last: 'var(--warn)' } : {})}
    ${kpi('average execution', '1.32s', 'max 9.81s', [1.3, 1.28, 1.35, 1.31, 1.29, 1.33, 1.34, 1.3, 1.32, 1.31, 1.33, 1.32])}
    ${kpi('pending', '3', '3 services', null)}
    ${stopped ? kpi('late', '3,516', 'every check, rising', [0, 0, 0, 0, 0, 0, 0, 0, 0, 160, 1980, 3516], { cls: 'v-bad', last: 'var(--crit)' }) : kpi(bz ? 'late' : 'late', bz ? '1,204' : '0', bz ? 'all in zone fra' : 'overdue checks', bz ? [0, 0, 0, 0, 0, 0, 0, 0, 0, 160, 720, 1204] : null, bz ? { cls: 'v-bad', last: 'var(--crit)' } : {})}
  </div>`;
  const qHdr = viewHeader({ picked: pick === 'queues and connections', name: 'queues and connections', display: 'tiles', filter: 'ApiListener, JsonRpc', sort: '', more: false });
  const kpisQ = `<div class="kpis">
    ${kpi('API work queue', '0', '412 items/s', [0, 1, 0, 0, 2, 0, 0, 1, 0, 0, 0, 0])}
    ${kpi('relay queue', broken ? '18,402' : '0', broken ? 'for fra, growing' : 'for other zones', broken ? [0, 0, 0, 0, 0, 0, 0, 0, 0, 2100, 9800, 18402] : [0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0], broken ? { cls: 'v-bad', last: 'var(--crit)' } : {})}
    ${beats && beats.conn ? kpi('cluster connections', beats.conn[0], 'JSON-RPC endpoints', null, beats.conn[1] ? { cls: 'v-bad' } : {}) : master2 ? kpi('cluster connections', '2 of 4', 'JSON-RPC endpoints', null, { cls: 'v-bad' }) : kpi('cluster connections', broken ? '3 of 4' : '4 of 4', 'JSON-RPC endpoints', null, broken ? { cls: 'v-bad' } : {})}
    ${kpi('HTTP clients', '4', 'API sessions', null)}
        ${kpi('uptime', '41d 6h', 'since Wed 26 Aug 08:14, the r2.14.3 upgrade', null, { span: 2 })}
  </div>`;
  const flagsHdr = viewHeader({ picked: pick === 'Icinga’s global switches', name: 'Icinga’s global switches', display: 'list', filter: 'read-only: icygui never changes them', sort: '', more: false });
  const fl = (on, t) => `<span>${dot(on ? 'ok' : 'warn', 'd6')}${t} <span class="${on ? 'faint' : 'c-warn'}">${on ? 'on' : 'off'}</span></span>`;
  const flags = `<div class="flags">${fl(1, 'notifications')}${fl(1, 'active host checks')}${fl(1, 'active service checks')}${fl(1, 'event handlers')}${fl(1, 'flap detection')}${fl(broken ? 0 : 1, 'performance data')}</div>`;
  const views = [['zones and endpoints', zonesHdr + table], ['checks', checksHdr + kpisChecks], ['queues and connections', qHdr + kpisQ], ['Icinga’s global switches', flagsHdr + flags]];
  if (only) return views.find(([n]) => n === only)[1];
  const headHtml = menuOn ? head.replace('<span class="glyph">···</span>', '<span class="glyph sel">···</span>') : head;
  return headHtml + sum + top + views.filter(([n]) => !off.includes(n)).map(([, h]) => h).join('');
}

// ---- topic 16 additions ------------------------------------------------------
// The heartbeat row: directly under the summary line, the same height (36px)
// and left edge; every part in a fixed slot sized for its longest value.
const HEARTBEAT = {
  ok: { st: 'ok', obj: 'icygui-heartbeat!beat', word: 'on time', age: '12s' },
  late: { st: 'warn', obj: 'icygui-heartbeat!beat', word: '1 interval late', age: '48s' },
  dead: { st: 'crit', obj: 'icygui-heartbeat!beat', word: 'dead · last 02:11', age: '6m 12s' },
  off: { st: 'pend', obj: 'not set', word: 'off', age: '' },
  missing: { st: 'pend', obj: 'icygui-heartbeat!beat', word: 'not found', age: '' },
};
const heartbeatRow = (k) => { const h = HEARTBEAT[k];
  return `<div class="hbrow"><span style="display:flex;align-items:center;gap:7px;flex:none">${dot(h.st, 'd9')}<span class="hl">heartbeat</span></span><span class="ho">${h.obj}</span><span class="hs2 ${h.st === 'crit' ? 'c-crit' : h.st === 'warn' ? 'c-warn' : ''}">${h.word}</span><span class="ha">${h.age}</span><span class="grow"></span><span class="faint">${k === 'off' ? '' : 'every 30s · notify'}</span><span class="acc">settings</span></div>`; };
// Icinga health alerts in 06's banner: [tone, title, detail, action, since]
function alertBlock(list) {
  const [w, ...rest] = list;
  const since = (t) => `<span class="muted" style="font-size:12px;width:11ch;text-align:right;flex:none">${t}</span>`;
  const first = `<div style="display:flex;align-items:center;gap:12px"><span class="c-${w[0]}">${icon('triangle-alert', 14)}</span><div class="col" style="gap:2px;min-width:0">
      <span style="font-size:12.5px;color:var(--t-strong)">${w[1]}</span>
      <span class="trunc" style="font-size:12px;color:var(--t-muted)">${w[2]}</span></div><span class="grow"></span><span class="acc" style="font-size:12px;white-space:nowrap">${w[3]}</span>${since(w[4])}</div>`;
  const more = rest.map(([tone, t, , , s]) => `<div style="display:flex;align-items:center;gap:12px;height:26px;border-top:1px solid var(--bd-row)"><span class="c-${tone}" style="display:flex">${icon('triangle-alert', 14)}</span><span style="font-size:12.5px;color:var(--t)">${t}</span><span class="grow"></span>${since(s)}</div>`).join('');
  return `<div class="banner" style="--tone:var(--${w[0]});--tint:var(--${w[0]}-tint);display:block;flex:none;padding-top:9px;padding-bottom:3px">${first}${more ? `<div style="margin-top:8px">${more}</div>` : ''}</div>`;
}

// ---- B3: heartbeats per zone and per endpoint (topic 16) ---------------------
// a heartbeat slot: [state, age] (the age of the last OK beat; the colour is the
// state), or null for a row without a heartbeat (the slot stays, empty)
const beatCell = (b) => (b ? `<span class="hbc">${dot(b[0], 'd7')}<span class="${b[0] === 'crit' ? 'c-crit' : b[0] === 'warn' ? 'c-warn' : 'sec'}">${b[1]}</span></span>` : '<span class="hbc"></span>');
// zones: [[st, name, text, beat, endpoints]], an endpoint: [st, name, zone, version, last, traffic, status, beat, { sel }]
const beatsTable = (zones) => `<div class="etab hbt">
    <div class="er h"><span></span><span>endpoint</span><span>zone</span><span>version</span><span>last message</span><span>messages in / out</span><span>status</span><span>heartbeat</span></div>
    ${zones.map(([st, name, txt, beat, eps]) => `<div class="zr">${dot(st, 'd7')}<span class="zn">${name}</span><span>${txt}</span><span class="grow"></span>${beatCell(beat)}</div>`
      + eps.map(([est, en, ez, ver, last, traffic, status, eb, o = {}]) => `<div class="er${o.sel ? ' sel' : ''}">${dot(est, 'd7')}<span><span class="strong">${en}</span></span><span class="muted">${ez}</span><span class="sec">${ver}</span><span class="sec">${last}</span><span class="muted">${traffic}</span><span class="${est === 'crit' ? 'c-crit' : 'muted'}">${status}</span>${beatCell(eb)}</div>`).join('')).join('')}
    <div class="zr" style="background:transparent;border-bottom:0;height:32px"><span class="faint">${icon('layers', 12)}</span><span>global zones: global-templates, director-global (config only, no endpoints)</span></div>
  </div>`;
// the heartbeat row becomes the summary of every beat: [st, label, subject, word, age]
// ('heartbeats', '6 of 6', 'on time'), or the worst ('heartbeat', 'fra', 'dead · last 02:11')
const hbSummary = ([st, label, subj, word, age = '']) => `<div class="hbrow hbsum"><span style="display:flex;align-items:center;gap:7px;flex:none">${dot(st, 'd9')}<span class="hl">${label}</span></span><span class="hsj">${subj}</span><span class="hs2 ${st === 'crit' ? 'c-crit' : st === 'warn' ? 'c-warn' : ''}">${word}</span><span class="ha">${age}</span><span class="grow"></span><span class="faint">${st === 'pend' ? '' : 'notify'}</span><span class="acc">settings</span></div>`;
