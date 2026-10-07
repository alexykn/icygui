//! The history from the local event log (PANE-04): the host pane's
//! *history* tab (the host's and its services' state changes,
//! acknowledgements, comments, downtimes and flapping) and the service
//! pane's *history* section, newest first, styled like the turn-1 design's
//! event stream, under `recorded locally since … · kept 48 h`.
//!
//! The log is local (`SQLite` through the core), so reading it costs Icinga
//! nothing. It is read when shown and again, at most every two seconds,
//! while new snapshots arrive.

use std::time::{Duration, Instant};

use gpui::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, prelude::FluentBuilder as _,
};
use ic_core::LogEntry;
use ic_model::{ObjectKey, Timestamp};
use ic_ui_kit::{ActiveTheme as _, Density, SectionLabel, StateDot, Theme, px};

use super::{HostTab, ObjectPane};
use crate::notifications::history::{
    self, HOST_HISTORY_LIMIT, HistoryLine, HistoryTone, SERVICE_HISTORY_LIMIT,
};

/// The least time between two reads of the log for the same pane.
const RELOAD_SPACING: Duration = Duration::from_secs(2);

/// What the pane read from the log.
#[derive(Debug)]
pub(super) struct PaneHistory {
    /// Whose history.
    object: ObjectKey,
    /// Newest first.
    entries: Vec<LogEntry>,
    /// When the log's oldest entry happened.
    oldest: Option<Timestamp>,
    /// The snapshot revision it was read at.
    revision: u64,
    /// When it was read (or asked for).
    read_at: Instant,
    /// An answer is on its way.
    loading: bool,
}

impl ObjectPane {
    /// Whether the pane shows a history now: a host pane's history tab, or
    /// a service pane (its section).
    fn shows_history(&self) -> bool {
        match self.object {
            ObjectKey::Host { .. } => self.host_tab == HostTab::History,
            ObjectKey::Service { .. } => true,
        }
    }

    /// Reads the log for the shown object when its history shows and is
    /// missing or older than the snapshot (spaced by [`RELOAD_SPACING`]).
    pub(super) fn want_history(&mut self, cx: &mut Context<Self>) {
        if !self.shows_history() {
            return;
        }
        let revision = self.state.read(cx).snapshot().revision;
        let outdated = match &self.log {
            None => true,
            Some(history) if history.object != self.object => true,
            Some(history) => {
                !history.loading
                    && history.revision != revision
                    && history.read_at.elapsed() >= RELOAD_SPACING
            }
        };
        if !outdated {
            return;
        }
        let limit = match self.object {
            ObjectKey::Host { .. } => HOST_HISTORY_LIMIT,
            ObjectKey::Service { .. } => SERVICE_HISTORY_LIMIT,
        };
        let object = self.object.clone();
        let (entries, oldest) = {
            let state = self.state.read(cx);
            (
                state.load_history(object.clone(), limit),
                state.load_history_start(),
            )
        };
        let Some(entries) = entries else {
            // No core yet: nothing to read; try again with a later render.
            return;
        };
        let keep = self.log.take().filter(|history| history.object == object);
        self.log = Some(PaneHistory {
            object: object.clone(),
            entries: keep
                .as_ref()
                .map(|history| history.entries.clone())
                .unwrap_or_default(),
            oldest: keep.and_then(|history| history.oldest),
            revision,
            read_at: Instant::now(),
            loading: true,
        });
        self.log_task = Some(cx.spawn(async move |this, cx| {
            let entries = entries.await.unwrap_or_default();
            let oldest = match oldest {
                Some(oldest) => oldest.await.ok().flatten(),
                None => None,
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(history) = &mut this.log
                    && history.object == object
                {
                    history.entries = entries;
                    history.oldest = oldest;
                    history.loading = false;
                    cx.notify();
                }
            });
        }));
    }

    /// The history's lines and its heading, if it was read for the shown
    /// object.
    fn history_lines(
        &self,
        now: Timestamp,
        cx: &Context<Self>,
    ) -> (Vec<HistoryLine>, String, bool) {
        let state = self.state.read(cx);
        let retention = state.config().general.event_log_retention_hours;
        let started = state.started_at();
        match self
            .log
            .as_ref()
            .filter(|history| history.object == self.object)
        {
            Some(history) => (
                history
                    .entries
                    .iter()
                    .map(|entry| history::line(entry, &self.object, now))
                    .collect(),
                history::since_text(history.oldest, started, retention, now),
                history.loading && history.entries.is_empty(),
            ),
            None => (
                Vec::new(),
                history::since_text(None, started, retention, now),
                true,
            ),
        }
    }

    /// The entries the history shows, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn history_entries(&self) -> Option<&[LogEntry]> {
        self.log
            .as_ref()
            .filter(|history| history.object == self.object && !history.loading)
            .map(|history| history.entries.as_slice())
    }
}

/// The host pane's history tab.
pub(super) fn host_tab(
    pane: &ObjectPane,
    host_name: &str,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> AnyElement {
    let theme = cx.theme();
    let (lines, since, loading) = pane.history_lines(now, cx);
    let empty = if loading {
        "Reading the local event log".to_owned()
    } else {
        format!(
            "Nothing recorded yet: state changes, acknowledgements, downtimes and flapping of \
             {host_name} and its services appear here as they happen."
        )
    };
    div()
        .flex()
        .flex_col()
        .py(px(12.))
        .child(
            div()
                .px(theme.metrics.pane_inset)
                .pb(px(8.))
                .text_size(theme.text.small)
                .text_color(theme.colors.text_faint)
                .child(since),
        )
        .when(lines.is_empty(), |column| {
            column.child(
                div()
                    .px(theme.metrics.pane_inset)
                    .py(px(8.))
                    .text_size(theme.text.small)
                    .text_color(theme.colors.text_muted)
                    .child(empty),
            )
        })
        .children(render_lines(&lines, theme))
        .into_any_element()
}

/// The lines, with a time column as wide as the widest time needs
/// (`06:14` today, `Oct 3 06:14` before).
fn render_lines(lines: &[HistoryLine], theme: &Theme) -> Vec<AnyElement> {
    let time_width = if lines.iter().any(|line| line.time.chars().count() > 5) {
        84.
    } else {
        44.
    };
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| render_line(index, line, time_width, theme))
        .collect()
}

/// The service pane's history section (its last few entries).
pub(super) fn service_section(
    pane: &ObjectPane,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> AnyElement {
    let theme = cx.theme();
    let (lines, since, loading) = pane.history_lines(now, cx);
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(10.))
                .child(SectionLabel::new("history"))
                .child(
                    div()
                        .text_size(theme.text.hint)
                        .text_color(theme.colors.text_faint)
                        .child(since),
                ),
        )
        .when(lines.is_empty(), |column| {
            column.child(
                div()
                    .text_size(theme.text.small)
                    .text_color(theme.colors.text_muted)
                    .child(if loading {
                        "Reading the local event log"
                    } else {
                        "Nothing recorded yet."
                    }),
            )
        })
        .child(
            div()
                .mx(-theme.metrics.pane_inset)
                .children(render_lines(&lines, theme)),
        )
        .into_any_element()
}

/// One line: time, dot, `KIND object`, and what was said under it (not in
/// compact rows: one line per entry, as in the dashboard list).
fn render_line(index: usize, line: &HistoryLine, time_width: f32, theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    let compact = theme.density == Density::Compact;
    // The dot takes the fill shade, the kind word the text shade.
    let (dot, word) = match line.tone {
        HistoryTone::State(state) => (
            theme.states.fill.checkable(state),
            theme.states.text.checkable(state),
        ),
        HistoryTone::Accent => (colors.accent, colors.accent),
        HistoryTone::Downtime => (theme.states.fill.unknown, theme.states.text.unknown),
        HistoryTone::Flapping => (theme.states.fill.warning, theme.states.text.warning),
        HistoryTone::Quiet => (colors.text_faint, colors.text_faint),
    };
    div()
        .id(SharedString::from(format!("history-{index}")))
        .flex()
        .items_start()
        .gap(px(10.))
        .px(theme.metrics.pane_inset)
        .py(px(5.))
        .child(
            div()
                .w(px(time_width))
                .flex_none()
                .pt(px(1.))
                .text_size(theme.text.hint)
                .text_color(colors.text_faint)
                .child(line.time.clone()),
        )
        .child(
            div()
                .pt(px(5.))
                .flex_none()
                .child(StateDot::with_color(dot).size(px(7.))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(1.))
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .min_w_0()
                        .text_size(theme.text.body)
                        .child(
                            div()
                                .flex_none()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(word)
                                .child(line.kind),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(colors.text)
                                .child(line.object.clone()),
                        ),
                )
                .when(!compact && !line.note.is_empty(), |column| {
                    column.child(
                        div()
                            .truncate()
                            .text_size(theme.text.small)
                            .text_color(colors.text_faint)
                            .child(line.note.clone()),
                    )
                }),
        )
        .into_any_element()
}
