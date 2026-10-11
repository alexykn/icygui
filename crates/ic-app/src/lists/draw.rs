//! The parts of topic 14's threads (`threads.js`, styles *threads* in
//! `v1.css`), drawn once for the handling and downtimes views and the
//! object's pane: the entry (the kind's icon in the mark slot, the header
//! line, the text, the two fixed tag slots), the light section label, the
//! fold of a host's services, the paging row, and the timeline's bars.
//! The band is the views' own ([`super::view`]): it opens the pane.
//!
//! Every slot that changes width is sized for its longest value, so a
//! changed time or expiry moves nothing.

use gpui::{
    AnyElement, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, SharedString, Styled as _, div, prelude::FluentBuilder as _, relative,
};
use ic_ui_kit::{CHAR_WIDTH, Density, Icon, IconName, Metrics, ObjectMark, StateDot, Theme, px};

use super::model::Mode;
use super::threads::{Line, Section};
use super::words::{Bar, EntryText, SlotA, Tone};

/// The mark column (state dot or kind icon), as wide as the list rows'.
pub(crate) const MARK_COLUMN: f32 = 44.;
/// The pane's mark column.
pub(crate) const PANE_MARK_COLUMN: f32 = 34.;
/// Space between the columns of a line.
pub(crate) const COLUMN_GAP: f32 = 14.;
/// Slot A: `sticky` or the progress line.
pub(crate) const SLOT_A: f32 = 56.;
/// Slot B, in characters: `expires 23:00, in 8h 48m`.
pub(crate) const SLOT_B_CHARS: f32 = 25.;
/// Between the tag's slots.
pub(crate) const SLOT_GAP: f32 = 10.;
/// The timeline's right column (`2h 03m left`).
pub(crate) const RIGHT_COLUMN: f32 = 92.;
/// The timeline's axis at most, and at least.
pub(crate) const AXIS_MAX: f32 = 640.;
pub(crate) const AXIS_MIN: f32 = 160.;
/// The name column beside the axis keeps at least this much.
const NAME_MIN: f32 = 260.;
/// The width of the accent bar on marked lines.
pub(crate) const MARK_WIDTH: f32 = 2.;

/// The heights the lines are drawn at (the theme's, at the interface size
/// and density in effect).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Sizes {
    /// A section label (30px) with its rule.
    pub(crate) section: Pixels,
    /// The timeline's axis (28px) with its rule.
    pub(crate) axis: Pixels,
    /// A band (36px) with its rule.
    pub(crate) band: Pixels,
    /// An entry with its rule: a list row's height (two lines; one when
    /// compact).
    pub(crate) entry: Pixels,
    /// A timeline line (44px; 32 compact) with its rule.
    pub(crate) timeline: Pixels,
    /// A fold, a service in it, a paging row (30px) with its rule.
    pub(crate) item: Pixels,
    /// Compact rows: one line per entry.
    pub(crate) compact: bool,
    /// The open comment field with its rule (topic 17): as tall as it was
    /// last drawn (it grows with what is typed), [`COMPOSER_HEIGHT`] until
    /// then.
    pub(crate) composer: Pixels,
}

/// The open comment field's height before it is first drawn: its header
/// line, a one-line field and its keys, with the rule.
pub(crate) const COMPOSER_HEIGHT: f32 = 100.;

impl Sizes {
    /// The sizes in `theme`.
    pub(crate) fn of(theme: &Theme) -> Self {
        let metrics = theme.metrics;
        let compact = theme.density == Density::Compact;
        Self {
            section: Metrics::with_rule(px(30.)),
            axis: Metrics::with_rule(px(28.)),
            band: Metrics::with_rule(metrics.group_row_height),
            entry: Metrics::with_rule(metrics.row_height),
            timeline: Metrics::with_rule(if compact { px(32.) } else { px(44.) }),
            item: Metrics::with_rule(metrics.item_row_height),
            compact,
            composer: px(COMPOSER_HEIGHT),
        }
    }

    /// These sizes with the open comment field `height` tall (as last
    /// drawn; `None`: not drawn yet).
    pub(crate) fn with_composer(mut self, height: Option<Pixels>) -> Self {
        if let Some(height) = height.filter(|height| *height > px(0.)) {
            self.composer = height;
        }
        self
    }

    /// The height of `line` in `mode`.
    pub(crate) fn height(&self, line: &Line, mode: Mode) -> Pixels {
        match line {
            Line::Section { .. } => self.section,
            Line::Axis => self.axis,
            Line::Band { .. } => self.band,
            Line::Entry { .. } => match mode {
                Mode::Timeline => self.timeline,
                Mode::List => self.entry,
            },
            Line::Fold { .. } | Line::Service { .. } | Line::More { .. } => self.item,
            // An entry, with its reason under it when refused.
            Line::Draft { refused, .. } => {
                self.entry + if *refused { px(NOTE_LINE) } else { px(0.) }
            }
            Line::Composer { .. } => self.composer,
        }
    }
}

/// The timeline's axis width in a list `width` wide: up to 640px, the name
/// column keeping its room.
pub(crate) fn axis_width(width: Pixels, theme: &Theme) -> Pixels {
    let fixed = theme.metrics.list_padding * 2.
        + px(MARK_COLUMN + RIGHT_COLUMN + 3. * COLUMN_GAP + NAME_MIN);
    (width - fixed).clamp(px(AXIS_MIN), px(AXIS_MAX))
}

/// Where the axis starts in a list `width` wide (its columns are right-
/// aligned: the name column takes what is left).
pub(crate) fn axis_left(width: Pixels, axis: Pixels, theme: &Theme) -> Pixels {
    width - theme.metrics.list_padding - px(RIGHT_COLUMN + COLUMN_GAP) - axis
}

/// The width of `chars` characters at `size`.
pub(crate) fn chars(size: Pixels, chars: f32) -> Pixels {
    (size * (chars * CHAR_WIDTH)).ceil()
}

/// The colour of a slot's words.
pub(crate) fn tone_color(tone: Tone, theme: &Theme) -> Hsla {
    match tone {
        Tone::Faint => theme.colors.text_faint,
        Tone::Accent => theme.colors.accent_text,
        Tone::Warning => theme.states.text.warning,
    }
}

/// `service on host` (or the host alone): the name in semibold, `on`
/// faint, the host medium (the bands' and single rows' label). It never
/// shrinks: what follows gives way first.
pub(crate) fn object_label(name: &str, host: Option<&str>, size: Pixels, theme: &Theme) -> Div {
    let colors = theme.colors;
    div()
        .flex()
        .flex_none()
        .items_baseline()
        .whitespace_nowrap()
        .text_size(size)
        .child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colors.text_emphasis)
                .child(name.to_owned()),
        )
        .when_some(host, |label, host| {
            label
                .child(div().text_color(colors.text_faint).child("\u{a0}on\u{a0}"))
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.text_secondary)
                        .child(host.to_owned()),
                )
        })
}

/// How an entry is drawn.
#[derive(Clone, Debug, Default)]
pub(crate) struct Look {
    /// In the object's pane: its narrower mark column, the text wrapping
    /// rather than cut.
    pub(crate) pane: bool,
    /// A later entry of its thread: the reply rule before its text.
    pub(crate) reply: bool,
    /// The object's only entry in one row (the downtimes list): its state
    /// dot in the mark slot and `service on host` first on the header
    /// line.
    pub(crate) object: Option<(ObjectMark, String, Option<String>)>,
    /// The text faint (the pane's downtime already shown in the banner).
    pub(crate) faint_text: bool,
    /// An action on its way, in slot B's place (`removing…`).
    pub(crate) pending: Option<&'static str>,
}

/// How an entry's first line gives way when it is narrow: its details
/// first, then the kind, the author last, each cut with an ellipsis, never
/// mid-letter; the time (a few characters) stays whole.
const SHRINK_META: f32 = 1_000_000.;
/// The kind's share of the shrinking (after the details).
const SHRINK_KIND: f32 = 100.;
/// The author's share (the last to give way).
const SHRINK_AUTHOR: f32 = 1.;

/// An entry: the kind's icon (or the object's dot), the header line (the
/// kind, the author, the time, the details), the text, and the tag's two
/// slots. The caller adds the background, the rule and the clicks.
pub(crate) fn entry(text: &EntryText, look: &Look, compact: bool, theme: &Theme) -> Div {
    entry_with(text, look, compact, theme, Extras::default())
}

/// What an entry of topic 17 adds to [`entry`]: a comment on its way or
/// refused, and the thread's last entry with *+ comment* in its time
/// slot.
#[derive(Default)]
pub(crate) struct Extras {
    /// Slot B holds this instead of its words (`retry · discard`).
    pub(crate) slot_b: Option<AnyElement>,
    /// Slot B swaps its words for this while the line (hover group
    /// `group`) is hovered: *+ comment* `c`.
    pub(crate) slot_b_hover: Option<(SharedString, AnyElement)>,
    /// A line under the text (a refusal's reason).
    pub(crate) note: Option<AnyElement>,
    /// The mark's colour (a refused comment's is critical).
    pub(crate) mark_color: Option<Hsla>,
    /// The header and the text dimmed (a comment on its way).
    pub(crate) dimmed: bool,
}

/// [`entry`] with topic 17's additions.
#[expect(
    clippy::too_many_lines,
    reason = "one entry as drawn: its mark, its header line in order of giving way, its text"
)]
pub(crate) fn entry_with(
    text: &EntryText,
    look: &Look,
    compact: bool,
    theme: &Theme,
    extras: Extras,
) -> Div {
    let colors = theme.colors;
    let mark_column = if look.pane {
        PANE_MARK_COLUMN
    } else {
        MARK_COLUMN
    };
    let mark: AnyElement = match &look.object {
        Some((mark, _, _)) => StateDot::mark(*mark).size(px(9.)).into_any_element(),
        None => Icon::new(text.icon)
            .size(px(13.))
            .color(extras.mark_color.unwrap_or(if text.accent {
                colors.accent_text
            } else {
                colors.text_faint
            }))
            .into_any_element(),
    };
    let kind = text.kind.map(|kind| {
        let config = text.icon == IconName::Lock && look.object.is_some();
        div()
            .flex()
            .flex_shrink(SHRINK_KIND)
            .min_w_0()
            .items_center()
            .gap(px(4.))
            .text_color(if text.accent {
                colors.accent_text
            } else {
                colors.text_muted
            })
            .when(config, |kind| {
                kind.child(
                    Icon::new(IconName::Lock)
                        .size(px(11.))
                        .color(colors.text_muted),
                )
            })
            .child(div().min_w_0().truncate().child(kind))
    });
    let header = div()
        .flex()
        .items_baseline()
        .gap(px(9.))
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(theme.text.small)
        .text_color(colors.text_faint)
        .when_some(look.object.as_ref(), |header, (_, name, host)| {
            header.child(object_label(name, host.as_deref(), theme.text.row, theme))
        })
        .children(kind)
        .when(!text.author.is_empty(), |header| {
            header.child(
                div()
                    .flex_shrink(SHRINK_AUTHOR)
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.body)
                    .text_color(colors.text_strong)
                    .child(text.author.clone()),
            )
        })
        .when(!text.at.is_empty(), |header| {
            header.child(div().flex_none().child(text.at.clone()))
        })
        .when(!text.meta.is_empty(), |header| {
            // Left out, not cut to a lone `…`, when only a few
            // characters fit.
            header.child(
                unless_narrow(
                    div().min_w_0().truncate().child(text.meta.clone()),
                    chars(theme.text.small, FEWEST_CHARS + 1.),
                    px(18.),
                )
                .flex_shrink(SHRINK_META),
            )
        });
    let words = div()
        .text_size(theme.text.body)
        .text_color(if look.faint_text {
            colors.text_faint
        } else {
            colors.text
        })
        .child(text.text.clone());
    let dimmed = extras.dimmed;
    let header = header.when(dimmed, |header| header.opacity(0.5));
    let words = words.when(dimmed, |words| words.opacity(0.5));
    let note = extras.note;
    let body = if compact && !look.pane {
        // One line: the header, then the text cut off (and the note under
        // it).
        let line = header.child(words.min_w_0().truncate().text_color(if look.faint_text {
            colors.text_faint
        } else {
            colors.text_muted
        }));
        match note {
            Some(note) => div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(line)
                .child(note_line(note, theme)),
            None => line,
        }
    } else {
        let words = if look.pane {
            words.line_height(relative(1.5))
        } else {
            words.h(px(20.)).line_height(px(20.)).min_w_0().truncate()
        };
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(header.h(px(18.)))
            .child(
                div()
                    .mt(px(3.))
                    .min_w_0()
                    .when(look.reply, |text| {
                        text.border_l(px(2.))
                            .border_color(colors.border_header)
                            .pl(px(12.))
                    })
                    .child(words),
            )
            .when_some(note, |body, note| body.child(note_line(note, theme)))
    };
    div()
        .flex()
        .items_start()
        .gap(px(if look.pane { 10. } else { COLUMN_GAP }))
        .child(
            div()
                .flex()
                .flex_none()
                .justify_center()
                .items_center()
                .w(px(mark_column))
                .h(px(18.))
                .child(mark),
        )
        .child(div().flex_1().min_w_0().child(body))
        .child(tag(
            text,
            look.pending,
            theme,
            extras.slot_b,
            extras.slot_b_hover,
        ))
}

/// The line under an entry's text: a refusal's reason, in the critical
/// colour.
fn note_line(note: AnyElement, theme: &Theme) -> Div {
    div()
        .mt(px(4.))
        .h(px(NOTE_LINE - 4.))
        .min_w_0()
        .truncate()
        .text_size(theme.text.small)
        .text_color(theme.states.text.critical)
        .child(note)
}

/// The height a note adds under an entry (its 4px gap included).
pub(crate) const NOTE_LINE: f32 = 22.;

/// The tag's two fixed slots: A (`sticky`, or the progress line) and B
/// (the expiry, the time left, when it starts), or slot B's own content
/// or its hover swap (see [`Extras`]).
fn tag(
    text: &EntryText,
    pending: Option<&'static str>,
    theme: &Theme,
    slot_b: Option<AnyElement>,
    slot_b_hover: Option<(SharedString, AnyElement)>,
) -> Div {
    let colors = theme.colors;
    let slot_a: AnyElement = match text.slot_a {
        SlotA::Empty => div().into_any_element(),
        SlotA::Sticky => div().child("sticky").into_any_element(),
        SlotA::Progress(fraction) => progress(fraction, colors.accent, theme).into_any_element(),
    };
    let (words, color) = match pending {
        Some(pending) => (pending.to_owned(), colors.text_muted),
        None => (text.slot_b.clone(), tone_color(text.tone, theme)),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(SLOT_GAP))
        .h(px(18.))
        .text_size(theme.text.label)
        .text_color(colors.text_faint)
        .whitespace_nowrap()
        .child(
            div()
                .flex()
                .flex_none()
                .justify_end()
                .w(px(SLOT_A))
                .child(slot_a),
        )
        .child({
            let slot = div()
                .relative()
                .flex()
                .flex_none()
                .justify_end()
                .w(chars(theme.text.label, SLOT_B_CHARS))
                .text_color(color);
            let own = match slot_b {
                Some(element) => element,
                None => div().child(words).into_any_element(),
            };
            match slot_b_hover {
                // The words give way to the hover's content in the same
                // slot: nothing moves.
                Some((group, hover)) => slot
                    .child(
                        div()
                            .group_hover(group.clone(), gpui::Styled::invisible)
                            .child(own),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .invisible()
                            .group_hover(group, gpui::Styled::visible)
                            .child(hover),
                    ),
                None => slot.child(own),
            }
        })
}

/// `child` (a truncating run) where at least `min` of it fits, else
/// nothing: never a lone `…`. The run wraps behind a zero-width first item
/// onto a second line, which the one-line box hides.
pub(crate) fn unless_narrow(child: Div, min: Pixels, line: Pixels) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .min_w_0()
        .h(line)
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .w(px(0.))
                .overflow_hidden()
                .child("\u{200b}"),
        )
        .child(child.min_w(min).max_w_full())
}

/// The fewest characters of output or details worth showing.
pub(crate) const FEWEST_CHARS: f32 = 4.;

/// A 56px progress line: the track, and the part passed in `color`.
pub(crate) fn progress(fraction: f32, color: Hsla, theme: &Theme) -> Div {
    div()
        .flex_none()
        .w(px(SLOT_A))
        .h(px(3.))
        .rounded(px(2.))
        .overflow_hidden()
        .bg(track_color(theme))
        .child(div().h_full().w(relative(fraction.clamp(0., 1.))).bg(color))
}

/// A progress track: the faint text colour at low alpha, which shows on
/// every row background (hovered, selected, marked) in both themes, where
/// a border colour sinks into the selection.
fn track_color(theme: &Theme) -> Hsla {
    theme.colors.text_faint.opacity(0.35)
}

/// A light section label: `in effect · 5 · ending soonest first`.
pub(crate) fn section(section: Section, count: usize, detail: &str, theme: &Theme) -> Div {
    let colors = theme.colors;
    div()
        .flex()
        .items_center()
        .gap(px(COLUMN_GAP))
        .size_full()
        .px(theme.metrics.list_padding)
        .border_b_1()
        .border_color(colors.border_header)
        .whitespace_nowrap()
        .overflow_hidden()
        .text_size(theme.text.label)
        .text_color(colors.text_faint)
        .child(
            div()
                .flex()
                .flex_none()
                .justify_center()
                .w(px(MARK_COLUMN))
                .child(
                    Icon::new(section.icon())
                        .size(px(12.))
                        .color(colors.text_faint),
                ),
        )
        .child(
            // The title and the detail as one run: one space each side of
            // the `·`, as between the title and its count.
            div()
                .flex()
                .min_w_0()
                .overflow_hidden()
                .child(
                    div()
                        .flex_none()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.text_secondary)
                        .child(section.title(count)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .child(format!("\u{a0}· {detail}")),
                ),
        )
}

/// The chevron at a band's or a fold's left (at the x of the view
/// headers'), which only folds and unfolds.
pub(crate) fn chevron(open: bool, theme: &Theme) -> Icon {
    Icon::new(if open {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .size(px(12.))
    .color(theme.colors.text_faint)
}

/// A fold's words: `18 services, same downtime · folded: they are
/// identical` (closed), `22 services, same downtime` (open).
pub(crate) fn fold_words(count: usize, open: bool, theme: &Theme) -> Div {
    let colors = theme.colors;
    let what = if count == 1 {
        "1 service, same downtime".to_owned()
    } else {
        format!("{count} services, same downtime")
    };
    div()
        .flex()
        .min_w_0()
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(theme.text.small)
        .text_color(colors.text_muted)
        .child(div().flex_none().child(what))
        .when(!open, |words| {
            words.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.text_faint)
                    .child("\u{a0}· folded: they are identical"),
            )
        })
}

/// A downtime's bar on an axis `width` wide.
pub(crate) fn bar(bar: &Bar, width: Pixels, theme: &Theme) -> Div {
    let colors = theme.colors;
    let x = |fraction: f32| width * fraction;
    let span = |from: f32, to: f32| (x(to) - x(from)).max(px(6.));
    let lane = div().relative().flex_none().w(width).h_full();
    match bar {
        Bar::InEffect {
            from,
            to,
            elapsed,
            since,
        } => lane
            .child(
                div()
                    .absolute()
                    .top(relative(0.5))
                    .mt(px(-1.))
                    .left(x(*from))
                    .w(span(*from, *to))
                    .h(px(3.))
                    .rounded(px(2.))
                    .overflow_hidden()
                    .bg(track_color(theme))
                    // The elapsed part ends at the now line.
                    .child(
                        div()
                            .h_full()
                            .w((x(*elapsed) - x(*from)).max(px(0.)))
                            .bg(colors.accent),
                    ),
            )
            .when_some(since.clone(), |lane, since| {
                // Began before the axis: say when, over its left end.
                lane.child(
                    div()
                        .absolute()
                        .top(px(3.))
                        .left(x(*from))
                        .whitespace_nowrap()
                        .text_size(px(10.5))
                        .text_color(colors.text_faint)
                        .child(since),
                )
            }),
        Bar::Upcoming { from, to } => lane.child(
            div()
                .absolute()
                .top(relative(0.5))
                .mt(px(-1.))
                .left(x(*from))
                .w(span(*from, *to))
                .h(px(3.))
                .rounded(px(2.))
                .bg(colors.text_faint)
                .opacity(0.55),
        ),
        Bar::Flexible { from, to, label } => lane
            .child(dashes(x(*from), span(*from, *to), colors.text_faint))
            .child(
                div()
                    .absolute()
                    .top(px(3.))
                    .left(x(*from))
                    .whitespace_nowrap()
                    .text_size(px(10.5))
                    .text_color(colors.text_faint)
                    .child(label.clone()),
            ),
        Bar::Later { text, lock } => lane.child(
            div()
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .flex()
                .items_center()
                .gap(px(5.))
                .whitespace_nowrap()
                .text_size(theme.text.hint)
                .text_color(colors.text_faint)
                .when(*lock, |later| {
                    later.child(
                        Icon::new(IconName::Lock)
                            .size(px(11.))
                            .color(colors.text_faint),
                    )
                })
                .child(text.clone()),
        ),
    }
}

/// A dashed line `width` wide at `left`, across the middle of its lane: a
/// flexible downtime's window (4px dashes, 3px apart).
fn dashes(left: Pixels, width: Pixels, color: Hsla) -> Div {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a few dozen dashes across an axis at most 640px wide"
    )]
    let count = (f32::from(width) / 7.).ceil().max(1.) as usize;
    div()
        .absolute()
        .top(relative(0.5))
        .left(left)
        .w(width)
        .h(Metrics::RULE)
        .flex()
        .gap(px(3.))
        .overflow_hidden()
        .opacity(0.8)
        .children((0..count).map(|_| div().flex_none().w(px(4.)).h_full().bg(color)))
}

/// The timeline's right column: `2h 03m left` in the accent, `in 7h 48m`,
/// `🔒 from config`.
pub(crate) fn right_column(words: SharedString, tone: Tone, lock: bool, theme: &Theme) -> Div {
    let colors = theme.colors;
    div()
        .flex()
        .flex_none()
        .justify_end()
        .items_center()
        .gap(px(5.))
        .w(px(RIGHT_COLUMN))
        .whitespace_nowrap()
        .text_size(theme.text.label)
        .text_color(tone_color(tone, theme))
        .when(lock, |column| {
            column.child(
                Icon::new(IconName::Lock)
                    .size(px(11.))
                    .color(colors.text_faint),
            )
        })
        .child(words)
}
