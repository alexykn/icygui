//! Key/value and performance data tables for the detail pane.

use std::fmt;

use gpui::{
    AnyElement, App, IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use ic_model::{Perfdata, PerfdataStatus, Threshold, format_number};

use crate::components::SectionLabel;
use crate::theme::ActiveTheme as _;

/// Width of the key column by default (the design's `check` section).
const DEFAULT_KEY_WIDTH: f32 = 140.;

/// A titled two-column table: `command  check_postgres`.
///
/// Values are elements, so they can be links or coloured text; plain strings
/// work too.
#[derive(IntoElement)]
#[must_use = "a table does nothing unless rendered"]
pub struct KvTable {
    title: Option<SharedString>,
    key_width: Pixels,
    rows: Vec<(SharedString, AnyElement)>,
}

impl KvTable {
    /// An empty table without a title.
    pub fn new() -> Self {
        Self {
            title: None,
            key_width: px(DEFAULT_KEY_WIDTH),
            rows: Vec::new(),
        }
    }

    /// Sets the section title above the rows.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets the width of the key column (default 140px).
    pub fn key_width(mut self, width: Pixels) -> Self {
        self.key_width = width;
        self
    }

    /// Adds a row.
    pub fn row(mut self, key: impl Into<SharedString>, value: impl IntoElement) -> Self {
        self.rows.push((key.into(), value.into_any_element()));
        self
    }

    /// The number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

impl Default for KvTable {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for KvTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let keys: Vec<_> = self.rows.iter().map(|(key, _)| key).collect();
        f.debug_struct("KvTable")
            .field("title", &self.title)
            .field("keys", &keys)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for KvTable {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let key_width = self.key_width;
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .when_some(self.title, |table, title| {
                table.child(div().pb(px(6.)).child(SectionLabel::new(title)))
            })
            .children(self.rows.into_iter().map(|(key, value)| {
                div()
                    .flex()
                    .gap(px(10.))
                    .py(px(5.))
                    .border_t_1()
                    .border_color(colors.border_row)
                    .text_size(theme.text.body)
                    .child(
                        div()
                            .flex_none()
                            .w(key_width)
                            .truncate()
                            .text_color(colors.text_muted)
                            .child(key),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_color(colors.text)
                            .child(value),
                    )
            }))
    }
}

/// One perfdata entry formatted for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PerfdataRow {
    /// The label.
    pub label: String,
    /// The value with its unit (`412s`), or `U` when unknown.
    pub value: String,
    /// The warning threshold, or empty.
    pub warn: String,
    /// The critical threshold, or empty.
    pub crit: String,
    /// The minimum, or empty.
    pub min: String,
    /// The maximum, or empty.
    pub max: String,
    /// Which threshold the value crosses; colours the value.
    pub status: PerfdataStatus,
}

impl PerfdataRow {
    /// Formats `perfdata` the way the design shows it.
    #[must_use]
    pub fn new(perfdata: &Perfdata) -> Self {
        let threshold =
            |threshold: Option<&Threshold>| threshold.map(Threshold::display).unwrap_or_default();
        let number = |value: Option<f64>| value.map(format_number).unwrap_or_default();
        Self {
            label: perfdata.label.clone(),
            value: perfdata.display_value(),
            warn: threshold(perfdata.warn.as_ref()),
            crit: threshold(perfdata.crit.as_ref()),
            min: number(perfdata.min),
            max: number(perfdata.max),
            status: perfdata.status(),
        }
    }
}

/// The detail pane's performance data table: label, value (coloured when a
/// threshold is crossed), warn and crit; min and max on request, where
/// there is room (a tab) and an entry has them.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a table does nothing unless rendered"]
pub struct PerfdataTable {
    title: SharedString,
    rows: Vec<PerfdataRow>,
    show_range: bool,
}

impl PerfdataTable {
    /// A table of `entries`, titled "performance data".
    pub fn new<'a>(entries: impl IntoIterator<Item = &'a Perfdata>) -> Self {
        Self {
            title: "performance data".into(),
            rows: entries.into_iter().map(PerfdataRow::new).collect(),
            show_range: false,
        }
    }

    /// Sets the title (the header of the label column).
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = title.into();
        self
    }

    /// Also shows the min and max columns, if any entry has a min or a max
    /// (empty columns would only take room).
    pub fn show_range(mut self, show: bool) -> Self {
        self.show_range = show;
        self
    }

    /// The formatted rows.
    #[must_use]
    pub fn rows(&self) -> &[PerfdataRow] {
        &self.rows
    }

    /// Whether the min and max columns are shown.
    #[must_use]
    pub fn shows_range(&self) -> bool {
        self.show_range
            && self
                .rows
                .iter()
                .any(|row| !row.min.is_empty() || !row.max.is_empty())
    }
}

impl RenderOnce for PerfdataTable {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let show_range = self.shows_range();
        let cell = |width: f32, text: SharedString| {
            div()
                .flex_none()
                .w(px(width))
                .text_right()
                .truncate()
                .child(text)
        };
        let header = div()
            .flex()
            .gap(px(10.))
            .pb(px(6.))
            .text_size(theme.text.label)
            .text_color(colors.text_faint)
            .child(div().flex_1().min_w_0().truncate().child(self.title))
            .child(cell(90., "value".into()))
            .child(cell(60., "warn".into()))
            .child(cell(60., "crit".into()))
            .when(show_range, |header| {
                header
                    .child(cell(60., "min".into()))
                    .child(cell(60., "max".into()))
            });
        div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(header)
            .children(self.rows.into_iter().map(|row| {
                div()
                    .flex()
                    .gap(px(10.))
                    .py(px(6.))
                    .border_t_1()
                    .border_color(colors.border_row)
                    .text_size(theme.text.body)
                    .text_color(colors.text_faint)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(colors.text_secondary)
                            .child(row.label),
                    )
                    .child(cell(90., row.value.into()).text_color(theme.perfdata_color(row.status)))
                    .child(cell(60., row.warn.into()))
                    .child(cell(60., row.crit.into()))
                    .when(show_range, |line| {
                        line.child(cell(60., row.min.into()))
                            .child(cell(60., row.max.into()))
                    })
            }))
    }
}

#[cfg(test)]
mod tests {
    use ic_model::parse_perfdata;

    use super::*;

    #[test]
    fn rows_format_values_and_thresholds() {
        let entries = parse_perfdata(
            "replication_lag=412s;60;300;0;3600 wal_retained=1.8GiB;4;8 active_connections=182;400;450;0",
        );
        let table = PerfdataTable::new(&entries);
        let rows = table.rows();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].label, "replication_lag");
        assert_eq!(rows[0].value, "412s");
        assert_eq!(rows[0].warn, "60");
        assert_eq!(rows[0].crit, "300");
        assert_eq!(rows[0].min, "0");
        assert_eq!(rows[0].max, "3600");
        assert_eq!(rows[0].status, PerfdataStatus::Critical);
        assert_eq!(rows[1].value, "1.8GiB");
        assert_eq!(rows[1].status, PerfdataStatus::Ok);
        assert_eq!(rows[2].max, "", "missing values stay empty");
    }

    #[test]
    fn unknown_values_and_missing_thresholds() {
        let entries = parse_perfdata("load=U time=0.2s");
        let rows: Vec<_> = entries.iter().map(PerfdataRow::new).collect();
        assert_eq!(rows[0].value, "U");
        assert_eq!(rows[0].warn, "");
        assert_eq!(rows[1].crit, "");
        assert_eq!(rows[1].status, PerfdataStatus::Ok);
    }

    #[test]
    fn the_range_columns_need_a_min_or_max() {
        let with_range = parse_perfdata("/=16.4GiB;32;36;0;40 /var=38.8GiB;32;36");
        assert!(
            !PerfdataTable::new(&with_range).shows_range(),
            "off by default"
        );
        assert!(
            PerfdataTable::new(&with_range)
                .show_range(true)
                .shows_range()
        );
        let without = parse_perfdata("load1=0.4;4;8 load5=0.3;4;8");
        assert!(
            !PerfdataTable::new(&without).show_range(true).shows_range(),
            "no entry has a min or max"
        );
    }

    #[test]
    fn kv_tables_keep_row_order() {
        let table = KvTable::new()
            .title("check")
            .row("command", "check_postgres")
            .row("interval", "60s · retry 15s");
        assert_eq!(table.len(), 2);
        assert!(!table.is_empty());
        assert!(format!("{table:?}").contains("interval"));
        assert!(KvTable::default().is_empty());
    }
}
