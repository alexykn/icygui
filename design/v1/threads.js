// Threads (topic 14, round 2): the one group pattern shared by the handling
// view, the downtimes view (list and timeline) and the object's pane, so the
// parts are identical everywhere. Styles: "threads" in v1.css.
//
//   tBand(o)      a group: the slim band of 10h (chevron at the left, which
//                 only collapses; a click elsewhere opens the object's pane)
//   tEntry(e)     an entry: acknowledgement, downtime or free-standing comment
//   tFold(f)      the services a host downtime covers, folded by default
//   tSec(s)       a light section label (expires within 2 hours, in effect…)
//   tChips(c)     the summary bar's filter chips
//   tHeader(h)    the view header: title, subtitle, mode, only mine, sort

const T_ICON = {
  ack: () => icon('check', 13),
  on: () => icon('calendar-clock', 13),
  up: () => icon('calendar-clock', 13),
  cfg: () => icon('lock', 13),
  comment: () => icon('message-square', 13),
};
const T_KIND = { ack: 'acknowledged', on: 'in downtime', up: 'downtime, upcoming', cfg: 'downtime, from config', comment: '' };
const T_ACC = { ack: true, on: true };

// a group band. o: { st, ring, name, host, out, slot, collapsed, sel, tl }
function tBand({ st, ring = false, name, host = '', out = '', slot = '', collapsed = false, sel = false, cls = '', after = '' }) {
  const label = host ? `${name}<span class="h"> on </span><span class="hh">${host}</span>` : name;
  return `<div class="ghb${sel ? ' sel' : ''}${cls ? ' ' + cls : ''}"><span class="chev">${icon(collapsed ? 'chevron-right' : 'chevron-down', 12)}</span><div class="lead"><span class="dot ${st}${ring ? ' ring' : ''} d9"></span></div>
    <div class="t"><span class="n">${label}</span><span class="o">${out}</span></div>${after || `<span class="cs"><span class="bslot">${slot}</span></span>`}</div>`;
}

// an entry. e: { kind, au, at, meta, text, re, sel, a, b, bTone }
//   a: slot A (sticky, or { progress }), b: slot B (expiry, time left, start)
function tEntry({ kind, au, at = '', meta = '', text, re = false, sel = false, a = '', b = '', bTone = '' }) {
  const k = T_KIND[kind];
  const slotA = typeof a === 'object' ? `<span class="mini"><span style="width:${a.progress}%"></span></span>` : a;
  return `<div class="te${sel ? ' sel' : ''}"><span class="mk${T_ACC[kind] ? ' acc' : ''}">${T_ICON[kind]()}</span>
    <div style="min-width:0"><div class="hd">${k ? `<span class="k${T_ACC[kind] ? ' acc' : ''}">${k}</span>` : ''}<span class="au">${au}</span><span>${at}</span>${meta ? `<span class="m">${meta}</span>` : ''}</div>
      <div class="tx${re ? ' re' : ''}">${text}</div></div>
    <span class="tg"><span class="tsa">${slotA}</span><span class="tsb${bTone ? ' t' + bTone : ''}">${b}</span></span></div>`;
}

// the services a host downtime covers: one fold row, collapsed by default;
// open, the services page by count (7, then "+ N more")
function tFold({ n, host, open = false, services = [], total = 0, tl = '' }) {
  let h = `<div class="tfold"><span class="chev">${icon(open ? 'chevron-down' : 'chevron-right', 12)}</span><span></span><span>${n} services, same downtime${open ? '' : ' <span class="faint">· folded: they are identical</span>'}</span></div>`;
  if (open) {
    h += services.slice(0, 7).map((s) => `<div class="tsvc${tl ? ' tlb' : ''}"><span class="c"><span class="dot ${s[1] || 'ok'} ring d7"></span></span><span><span class="n">${s[0]}</span><span class="o"> on ${host}</span></span>${tl || ''}</div>`).join('');
    if (total > 7) h += `<div class="morerow"><span></span><span>+ ${total - 7} more</span></div>`;
  }
  return h;
}

const tSec = ({ ic, title, detail = '', right = '' }) => `<div class="tsec"><span class="mk">${icon(ic, 12)}</span><span><b>${title}</b>${detail ? `<span class="faint">  ·  ${detail}</span>` : ''}</span><span class="r">${right}</span></div>`;

// the filter chips in the summary bar: [key, label, count, mark]; sel: the key shown
function tChips(chips, sel, end = '') {
  return `<div class="sum"><span class="fchips">${chips.map(([key, label, n, mark]) => `<span class="chip${key === sel ? ' sel' : ''}">${mark || ''}${n !== undefined ? `${n} ` : ''}${label}</span>`).join('')}</span><span class="end">${end}</span></div>`;
}
const HANDLING_CHIPS = [
  ['all', 'all', undefined],
  ['ack', 'acknowledged', 7, `<span class="acc" style="display:flex">${icon('check', 11)}</span>`],
  ['on', 'in downtime', 5, `<span class="dot accb d7"></span>`],
  ['up', 'upcoming', 4, `<span class="dot pend d7"></span>`],
  ['comment', 'comments', 6, `<span style="display:flex">${icon('message-square', 11)}</span>`],
];
const DOWNTIME_CHIPS = [
  ['all', 'all', undefined],
  ['on', 'in effect', 5, `<span class="dot accb d7"></span>`],
  ['up', 'upcoming', 4, `<span class="dot pend d7"></span>`],
  ['cfg', 'from config', 2, `<span style="display:flex">${icon('lock', 11)}</span>`],
];

// the view header; mode: the downtimes view's display (timeline or list)
function tHeader({ title, subtitle, sort, mode = '', mine = false }) {
  const seg = mode ? `<span class="seg" style="height:26px;margin-right:6px">${['timeline', 'list'].map((m) => `<span class="${m === mode ? 'on' : ''}" style="padding:0 12px">${m}</span>`).join('')}</span>` : '';
  return listHeader({ title, subtitle, sort, extra: `${seg}<span style="margin-right:6px">${sw(mine, 'only mine')}</span>` });
}
