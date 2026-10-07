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
// The band is a flex row (.tb): the name and host never shrink, the faint
// output goes first, then the right slot; so a narrow list next to a pane
// keeps "service on host" whole as long as possible.
function tBand({ st, ring = false, name, host = '', out = '', slot = '', collapsed = false, sel = false, cls = '', after = '' }) {
  const label = host ? `${name}<span class="h"> on </span><span class="hh">${host}</span>` : name;
  return `<div class="ghb${after ? '' : ' tb'}${sel ? ' sel' : ''}${cls ? ' ' + cls : ''}"><span class="chev">${icon(collapsed ? 'chevron-right' : 'chevron-down', 12)}</span><div class="lead"><span class="dot ${st}${ring ? ' ring' : ''} d9"></span></div>
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

// A single downtime (or single entry) of an object without a fold: band and
// entry merged into one row of an entry's height; the chevron slot is empty,
// the mark slot has the object's state dot, the header line names the object.
function tRow({ st, ring = false, name, host = '', kind, au, at = '', meta = '', text, sel = false, a = '', b = '', bTone = '' }) {
  const k = T_KIND[kind];
  const slotA = typeof a === 'object' ? `<span class="mini"><span style="width:${a.progress}%"></span></span>` : a;
  const label = host ? `${name}<span class="h"> on </span><span class="hh">${host}</span>` : name;
  return `<div class="te tr1${sel ? ' sel' : ''}"><span class="mk"><span class="dot ${st}${ring ? ' ring' : ''} d9"></span></span>
    <div style="min-width:0"><div class="hd"><span class="on">${label}</span>${k ? `<span class="k${T_ACC[kind] ? ' acc' : ''}">${kind === 'cfg' ? `${icon('lock', 11)} ` : ''}${k}</span>` : ''}<span class="au">${au}</span><span>${at}</span>${meta ? `<span class="m">${meta}</span>` : ''}</div>
      <div class="tx">${text}</div></div>
    <span class="tg"><span class="tsa">${slotA}</span><span class="tsb${bTone ? ' t' + bTone : ''}">${b}</span></span></div>`;
}

// ---- the timeline: a shared axis, one row per single downtime, a group
// (band, one line per downtime, the services fold) for an object with several
// downtimes or a host downtime with services ----------------------------------
const TL = { a0: 12, a1: 24, w: 640, now: 14.2 };
const tlX = (t) => Math.round((Math.min(Math.max(t, TL.a0), TL.a1) - TL.a0) / (TL.a1 - TL.a0) * TL.w);
// bars: [from, to, kind, label] kind: on | up | flex; off: text at the right edge
function tlAxis(bars = [], off = '') {
  let h = '';
  bars.forEach(([a, b, k, lab]) => {
    const l = tlX(a), w = Math.max(6, tlX(b) - tlX(a));
    if (k === 'flex') h += `<span class="bar flex" style="left:${l}px;width:${w}px"></span><span class="lab" style="left:${l}px">${lab}</span>`;
    else if (k === 'on') h += `<span class="bar" style="left:${l}px;width:${w}px"><span style="width:${Math.round((TL.now - a) / (b - a) * 100)}%"></span></span>`;
    else h += `<span class="bar up" style="left:${l}px;width:${w}px"></span>`;
  });
  if (off) h += `<span class="off">${off}</span>`;
  return `<span class="ax">${h}</span>`;
}
const tlHead = (label) => `<div class="tlax"><span></span><span>${label}</span><span class="ax">${[12, 14, 16, 18, 20, 22, 24].map((t) => `<span style="left:${tlX(t)}px">${String(t % 24).padStart(2, '0')}:00</span>`).join('')}</span><span style="text-align:right">left</span></div>`;
// an object with a single downtime: one row
const tlRow = ({ st, ring = false, name, host = '', text, bars, off = '', right, acc = false, sel = false }) => `<div class="tml${sel ? ' sel' : ''}"><span class="c"><span class="dot ${st}${ring ? ' ring' : ''} d9"></span></span><span class="s"><span><span class="n">${name}</span>${host ? `<span class="o"> on </span>${host}` : ''}</span><span class="t2">${text}</span></span>${tlAxis(bars, off)}<span class="r${acc ? ' acc' : ''}">${right}</span></div>`;
// the group band of an object with several downtimes or a services fold
const tlBand = (o) => tBand({ ...o, cls: 'tlb', after: `<span class="ax"></span><span class="r faint">${o.slot || ''}</span>` });
// one downtime inside a group: the kind's icon in the mark slot
const tlLine = ({ kind, au, meta = '', text, bars, off = '', right, acc = false }) => `<div class="tml tline"><span class="c mk${T_ACC[kind] ? ' acc' : ''}">${T_ICON[kind]()}</span><span class="s"><span><span class="k${T_ACC[kind] ? ' acc' : ''}">${T_KIND[kind]}</span>  <span class="n">${au}</span>${meta ? `<span class="o">  ${meta}</span>` : ''}</span><span class="t2">${text}</span></span>${tlAxis(bars, off)}<span class="r${acc ? ' acc' : ''}">${right}</span></div>`;

const tSec = ({ ic, title, detail = '', right = '' }) => `<div class="tsec"><span class="mk">${icon(ic, 12)}</span><span><b>${title}</b>${detail ? `<span class="faint">  ·  ${detail}</span>` : ''}</span><span class="r">${right}</span></div>`;

// the filter chips in the summary bar: [key, label, count, mark]; sel: the key shown
// narrow: a stacked header with little room (the editor's preview, beside a
// pane) keeps each chip's mark and count and drops the word (its tooltip
// names it); the filter summary is cut before that
const tChipRow = (chips, sel, { narrow = false } = {}) => `<span class="fchips">${chips.map(([key, label, n, mark]) => `<span class="chip${key === sel ? ' sel' : ''}">${mark || ''}${n !== undefined ? (narrow ? `${n}` : `${n} `) : ''}${narrow && n !== undefined ? '' : label}</span>`).join('')}</span>`;
function tChips(chips, sel, end = '') {
  return `<div class="sum">${tChipRow(chips, sel)}<span class="end">${end}</span></div>`;
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
// Every view has ONE set of controls, always in its view header: the
// downtimes view's mode switch, the kind chips (they filter), the sort and
// "only mine". On a one-view dashboard the view header is the page header
// (with the summary bar as its second row): roomy. Stacked in a multi-view
// dashboard, the same controls sit compactly in the 36px view header, and
// "only mine" moves into the view's ··· menu.
const tModeSeg = (mode, { compact = false } = {}) => `<span class="seg${compact ? ' cmp' : ''}" style="${compact ? '' : 'height:26px;'}margin-right:6px">${['timeline', 'list'].map((m) => `<span class="${m === mode ? 'on' : ''}"${compact ? '' : ' style="padding:0 12px"'}>${m}</span>`).join('')}</span>`;
// kind ('handling' or 'downtimes', round 5): the one-view form of VIEW
// CONTROLS: mode switch, only mine, the row-density toggle (rows: as
// densityToggle's v; the timeline has rows too, so it is never dim), the
// sort in its fixed slot, ···
function tHeader({ title, subtitle, sort, mode = '', mine = false, kind = '', rows = null, eff = 'comfortable' }) {
  const dens = rows ? `<span style="margin-right:2px">${densityToggle({ v: rows, eff })}</span>` : '';
  return listHeader({ title, subtitle, sort, sortCh: kind ? SORT_CH[kind] : 0, extra: `${mode ? tModeSeg(mode) : ''}<span style="margin-right:6px">${sw(mine, 'only mine')}</span>${dens}` });
}
// the controls of a stacked view (in its view header): chips, then the mode switch
const tStackedControls = (chips, sel, mode = '', { narrow = false } = {}) => `<span class="vctl">${tChipRow(chips, sel, { narrow })}${mode ? tModeSeg(mode, { compact: true }) : ''}</span>`;
