// The settings window (topic 02), shared with the light theme (topic 03).
// settingsWindow({ page, search, scrolled, inactive }) returns the window's HTML.

const SETTINGS_PAGES = [
  ['general', 'settings', ['in the background']],
  ['appearance', 'sun-moon', ['theme and size', 'lists', 'preview']],
  ['notifications', 'bell', ['this environment', 'default rule', 'quiet hours', 'storm control', 'watched and muted', 'groups and dashboards']],
  ['icinga', 'server', ['reconcile', 'event log', 'environments']],
  ['keymap', 'keyboard', []],
  ['advanced', 'wrench', ['logs', 'files', 'about']],
];

function srow(name, desc, control, { sub, err, nameHtml } = {}) {
  return `<div class="srow${sub ? ' sub' : ''}"><div style="min-width:0"><div class="nm">${nameHtml || name}</div>${desc ? `<div class="ds">${desc}</div>` : ''}${err ? `<div class="err">${err}</div>` : ''}</div><div class="ctl">${control}</div></div>`;
}
const ssec = (label, d = '') => `<div class="ssec"><span>${label}</span>${d ? `<span class="d">· ${d}</span>` : ''}</div>`;
const inp = (value, w = 80, { focus, bad, suffix } = {}) => `<span class="input${focus ? ' focus' : ''}${bad ? ' bad' : ''}" style="width:${w}px;font-size:12.5px">${value}${focus ? '<span class="caret"></span>' : ''}</span>${suffix ? `<span class="muted" style="font-size:12px">${suffix}</span>` : ''}`;

function pageGeneral() {
  return ssec('in the background') +
    srow('keep running in the tray', 'When the window closes, icygui stays in the tray and keeps notifying.', sw(true)) +
    srow('start at login', 'Starts icygui in the tray, without its window, when you log in.', sw(false)) +
    srow('quiet mode when hidden', 'Out of sight for half a minute: follow Icinga without check results. Far less load on the master.', sw(true)) +
    `<div class="srow" style="border-top:1px solid var(--bd-row);padding:12px 0;grid-template-columns:16px minmax(0,1fr);gap:10px;align-items:start;color:var(--t-muted);font-size:12px;line-height:1.5">
      <span class="faint" style="padding-top:1px">${icon('info', 14)}</span><span>The tray icon shows the worst unhandled state; its menu opens the window, pauses notifications, switches environments and quits. Notifications are as prompt in quiet mode; outputs catch up when you look.</span></div>`;
}

function pageAppearance({ density = 1, times = 1, theme = 0 } = {}) {
  const prevRows = [
    ['crit', 'postgres-replication', 'db-prod-03', 'CRITICAL - standby lag 412s (> 300s)', times ? '13:58' : '14m'],
    ['warn', 'pg-connections', 'db-prod-03', 'WARNING - 182 of 200 per-db limit (orders)', times ? '13:50' : '22m'],
    ['unk', 'mysql-replication', 'db-mysql-03', 'UNKNOWN - connection refused', times ? '12:09' : '2h'],
  ];
  const compactRow = ([st, n, h, , t]) => `<div style="display:grid;grid-template-columns:44px minmax(0,1fr) auto;gap:14px;align-items:center;height:20px;padding:6px 18px;border-bottom:1px solid var(--bd-row)">
    <div style="display:flex;justify-content:center">${circle(st, 14)}</div><span class="trunc" style="font-size:13px"><span class="strong med">${n}</span><span class="faint"> on </span><span class="sec">${h}</span></span><span class="faint" style="font-size:11.5px">${t}</span></div>`;
  const preview = density === 1
    ? prevRows.map(compactRow).join('')
    : prevRows.map((r) => row(r)).join('');
  return ssec('theme and size') +
    srow('theme', 'Follow system switches with your desktop’s light or dark mode.', seg(['follow system', 'dark', 'light'], theme)) +
    srow('interface size', 'Scales text and spacing in every window: 90 %, 100 % or 115 %.', seg(['small', 'default', 'large'], 1)) +
    ssec('lists') +
    srow('row density', 'Compact drops the output line: one line per object, about twice the rows.', seg(['comfortable', 'compact'], density)) +
    srow('times in lists', `Under the state circle: how long in this state (14m), or since when (13:58).`, seg(['relative', 'clock'], times)) +
    ssec('preview', 'the databases dashboard as it would look') +
    `<div style="border:1px solid var(--bd-header);border-radius:6px;overflow:hidden;margin-top:2px">${preview}</div>`;
}

function pageNotifications(part = 0) {
  if (part === 0) {
    return ssec('this environment') +
      srow('environment', 'Notification rules belong to an environment; each one notifies on its own.', select('prod-cluster', 180)) +
      srow('notifications for prod-cluster', 'Desktop notifications from this environment’s rules.', sw(true)) +
      srow('pause all', 'Every environment, until it runs out; the tray and the centre can resume.', chip('30m') + chip('1h') + chip('until 08:00')) +
      srow('show plugin output', 'The output’s first line in desktop notifications. Off for shared screens and the lock screen.', sw(true)) +
      ssec('default rule', 'groups and dashboards notify with it unless they say otherwise') +
      srow('states', 'Recoveries follow problems that notified.', chip('critical', { sel: 1 }) + chip('warning') + chip('unknown', { sel: 1 }) + chip('down', { sel: 1 }) + chip('unreachable') + chip('recovery', { sel: 1 })) +
      srow('events', 'Also notify when these start or end.', chip('acknowledgements') + chip('downtimes') + chip('flapping')) +
      srow('hard states only', 'Soft states are retries in progress.', sw(true)) +
      srow('skip handled problems', 'Acknowledged, in downtime, or the host is down.', sw(true)) +
      srow('only after', 'A problem must last this long first (5m, 1h; 0 = at once).', inp('0', 80)) +
      srow('play a sound', 'The system’s alert sound, by state where the desktop plays sounds.', sw(true));
  }
  const scopeRow = (name, inherits, on = 0, sub = false) => srow(name, inherits, seg(['inherit', 'on', 'off', 'custom'], on), { sub });
  return ssec('quiet hours') +
    srow('record silently at night', 'Notifications in the window go to the centre without a desktop notification.', sw(true)) +
    srow('from and to', 'May cross midnight; the days are the ones it starts on.', inp('22:00', 72) + `<span class="faint">→</span>` + inp('07:00', 72), { sub: true }) +
    srow('days', '', ['mo', 'tu', 'we', 'th', 'fr', 'sa', 'su'].map((d, i) => chip(d, { sel: i < 5 })).join(''), { sub: true }) +
    srow('critical and down still notify out loud', '', sw(true), { sub: true }) +
    ssec('storm control') +
    srow('storm threshold', 'Beyond it, one summary notification.', `<span class="muted" style="font-size:12px">at most</span>${inp('5', 44)}<span class="muted" style="font-size:12px">in</span>${inp('10', 44)}<span class="muted" style="font-size:12px">seconds</span>`) +
    ssec('watched and muted', 'from a pane’s ··· menu or the palette') +
    srow('', '', `<span class="btn sm">remove</span>`, { nameHtml: `<span style="display:flex;align-items:center;gap:10px">${dot('crit', 'd7')}<span>postgres-replication<span class="faint"> on </span><span class="sec">db-prod-03</span></span><span class="muted" style="font-size:12px">watched: always notifies</span></span>` }) +
    srow('', '', `<span class="btn sm">remove</span>`, { nameHtml: `<span style="display:flex;align-items:center;gap:10px">${dot('unk', 'd7')}<span>backup-01</span><span class="muted" style="font-size:12px">muted until 08:00</span></span>` }) +
    ssec('groups and dashboards') +
    scopeRow('overview', 'inherits prod-cluster') + scopeRow('databases', 'on: notifies even if overview is off', 1, true) + scopeRow('host problems', 'inherits overview', 0, true);
}

function pageIcinga() {
  const envRow = (name, st, det, sel) => `<div class="srow" style="grid-template-columns:minmax(0,1fr) auto;${sel ? '' : ''}">
    <div style="display:flex;align-items:center;gap:10px;min-width:0">${dot(st, 'd7')}<span class="nm">${name}</span><span class="faint trunc" style="font-size:12px">${det}</span></div>
    <div class="ctl"><span class="ibtn" style="color:var(--t-muted)">${icon('settings', 13)}</span></div></div>`;
  return ssec('reconcile') +
    srow('reconcile with Icinga', 'A lean reload of every object catches what the event stream missed.', seg(['adaptive', 'fixed interval'], 0)) +
    `<div class="ds muted" style="font-size:12px;padding:0 0 12px;margin-top:-4px;line-height:1.5">Adaptive: every 5 minutes for a small Icinga, every 15 at 30 000 objects, up to an hour while the stream runs without a break.</div>` +
    ssec('event log') +
    srow('keep events for', 'The history tabs and the notification centre read the local log.', inp('48', 64, { suffix: 'hours' })) +
    ssec('environments', 'every environment runs and notifies, whichever is on screen') +
    envRow('prod-cluster', 'ok', 'master-01, master-02 · 2 URLs · connected to master-01') +
    envRow('staging', 'ok', 'stg-master-01 · 1 URL · connected') +
    envRow('lab', 'warn', 'lab-icinga · 1 URL · stale, last event 41s ago') +
    `<div style="padding:12px 0;border-top:1px solid var(--bd-row)">${btn('add environment', { icon: 'plus' })}</div>`;
}

function pageKeymap() {
  const k = (...keys) => keys.map((x) => `<span class="kbd">${x}</span>`).join('');
  const r = (name, keys, ctx) => `<div class="srow" style="grid-template-columns:minmax(0,1fr) 220px 150px;padding:9px 0;min-height:0"><span class="nm" style="font-size:12.5px">${name}</span><span style="display:flex;gap:4px">${keys}</span><span class="faint" style="font-size:12px">${ctx}</span></div>`;
  return `<div style="display:flex;gap:10px;align-items:center;padding:16px 0 10px"><span class="input" style="flex:1;gap:8px"><span class="muted">${icon('search', 13)}</span><span class="ph">filter shortcuts by name or key</span></span></div>` +
    `<div class="srow" style="grid-template-columns:minmax(0,1fr) 220px 150px;padding:6px 0;min-height:0;border-top:0"><span class="faint" style="font-size:11.5px">action</span><span class="faint" style="font-size:11.5px">keys</span><span class="faint" style="font-size:11.5px">where</span></div>` +
    r('command palette', k('ctrl-k'), 'anywhere') +
    r('settings', k('ctrl-,'), 'anywhere') +
    r('new dashboard', k('ctrl-n'), 'anywhere') +
    r('select dashboard 1 to 9', k('ctrl-1') + `<span class="faint">to</span>` + k('ctrl-9'), 'anywhere') +
    r('show or hide the sidebar', k('ctrl-b'), 'anywhere') +
    r('next row, previous row', k('j') + k('k') + k('↓') + k('↑'), 'list') +
    r('mark the row', k('x'), 'list') +
    r('mark every row', k('ctrl-a'), 'list') +
    r('open the pane, as a tab', k('enter') + k('ctrl-enter'), 'list') +
    r('acknowledge', k('a'), 'list, pane') +
    r('schedule downtime', k('d'), 'list, pane') +
    r('check now', k('r'), 'list, pane') +
    r('add comment', k('c'), 'list, pane') +
    r('run a verb on all matches', k('ctrl-enter'), 'palette') +
    r('save, discard', k('ctrl-s') + k('esc'), 'dashboard editor');
}

function pageAdvanced() {
  return ssec('logs') +
    srow('log level', 'What icygui writes to its log; debug adds each request’s path and timing.', select('info', 120)) +
    srow('log folder', '~/.local/state/icygui/logs · 3 files, 4.1 MB, kept 7 days', btn('open folder', { icon: 'folder-open' })) +
    ssec('files') +
    srow('config folder', '~/.config/icygui · settings.toml, keymap.toml, dashboards per environment', btn('open folder', { icon: 'folder-open' })) +
    ssec('about') +
    srow('icygui 0.1.0', 'IBM Plex Mono, Lucide icons, GPUI; licences in the about dialog.', btn('about'));
}

function pageSearch() {
  const hl = (s) => s.replace(/quiet/i, (m) => `<span class="hl">${m}</span>`);
  const grp = (label) => `<div class="ssec" style="padding-top:20px"><span style="color:var(--t-sec)">${label}</span></div>`;
  return grp('general') +
    srow('', 'Out of sight for half a minute: follow Icinga without check results. Far less load on the master.', sw(true), { nameHtml: hl('quiet mode when hidden') }) +
    grp('notifications · quiet hours') +
    srow('', 'Notifications in the window go to the centre without a desktop notification.', sw(true), { nameHtml: `record silently at night <span class="faint" style="font-size:12px">· ${hl('quiet hours')}</span>` }) +
    srow('from and to', 'May cross midnight; the days are the ones it starts on.', inp('22:00', 72) + `<span class="faint">→</span>` + inp('07:00', 72), { sub: true }) +
    srow('days', '', ['mo', 'tu', 'we', 'th', 'fr', 'sa', 'su'].map((d, i) => chip(d, { sel: i < 5 })).join(''), { sub: true }) +
    srow('critical and down still notify out loud', `During ${hl('quiet')} hours.`, sw(true), { sub: true }) +
    `<div class="ssec" style="padding-top:24px"><span>no matches in appearance, icinga, keymap, advanced</span></div>`;
}

function settingsNav({ page = 'general', search = '', section = 0 }) {
  let html = '';
  const counts = { general: 1, notifications: 4 };
  for (const [name, ic, subs] of SETTINGS_PAGES) {
    const active = !search && name === page;
    const dim = search && !counts[name];
    html += `<div class="sni${active ? ' active' : ''}${dim ? ' dim' : ''}">${icon(ic, 14)}<span>${name}</span>${search && counts[name] ? `<span class="count">${counts[name]}</span>` : ''}</div>`;
    if (active) subs.forEach((s, i) => { html += `<div class="snsub${i === section ? ' on' : ''}"><span class="guide"></span><span>${s}</span></div>`; });
  }
  return html;
}

function settingsWindow({ page = 'general', search = '', part = 0, section = 0, inactive = false, title, subtitle, appearance = {} } = {}) {
  const pages = { general: pageGeneral, appearance: () => pageAppearance(appearance), notifications: () => pageNotifications(part), icinga: pageIcinga, keymap: pageKeymap, advanced: pageAdvanced };
  const body = search ? pageSearch() : pages[page]();
  const subtitles = { general: 'for icygui on this computer', appearance: 'for icygui on this computer', notifications: 'for prod-cluster · on this computer only', icinga: 'for icygui on this computer', keymap: 'keymap.toml', advanced: 'for icygui on this computer' };
  const head = search
    ? `<span class="title">5 settings</span><span class="subtitle">match “quiet”</span>`
    : `<span class="title">${title || page}</span><span class="subtitle">${subtitle || subtitles[page]}</span>`;
  return `<div class="swin">
    <div class="snav">
      <div class="top">${trafficLights(inactive)}<span class="vdiv"></span><span class="muted">${icon('search', 13)}</span>
        ${search ? `<span class="q typed">${search}<span class="caret"></span></span><span class="grow"></span><span class="faint">${icon('x', 12)}</span>` : '<span class="q">Search settings</span>'}</div>
      <div class="list">${settingsNav({ page, search, section })}</div>
      <div class="foot"><span>focus navbar</span><span class="grow"></span><span class="kh">ctrl-shift-e</span></div>
    </div>
    <div class="spage">
      <div class="hbar" style="padding:0 20px 0 32px">${head}<span class="grow"></span>
        <span class="quiet" style="display:flex;align-items:center;gap:6px">${icon('check', 12)}saved</span>${btn(page === 'keymap' && !search ? 'edit keymap file' : 'edit in settings file', { icon: 'file-code' })}</div>
      <div class="scontent">${body}</div>
    </div></div>`;
}
