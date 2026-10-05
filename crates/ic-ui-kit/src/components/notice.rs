//! Text blocks for the panes and empty lists: comment-style notes, empty and
//! error states, and an indented key/value tree.

use std::fmt;

use gpui::{
    AnyElement, App, IntoElement, ParentElement, Pixels, RenderOnce, SharedString, Styled as _,
    Window, div, prelude::FluentBuilder as _, px, relative,
};

use crate::components::SectionLabel;
use crate::theme::ActiveTheme as _;

/// A comment, acknowledgement or downtime in the detail pane:
///
/// ```text
/// #  j.berg 13:58
///    Failover drill on db-prod-01 at 15:00. Expect lag on 03.
/// ```
///
/// Children added with [`ParentElement`] go at the right end (a remove
/// button).
#[derive(IntoElement)]
#[must_use = "a note does nothing unless rendered"]
pub struct NoteEntry {
    marker: SharedString,
    author: SharedString,
    meta: Vec<SharedString>,
    body: SharedString,
    trailing: Vec<AnyElement>,
}

impl NoteEntry {
    /// A note by `author` saying `body`, marked with `#`.
    pub fn new(author: impl Into<SharedString>, body: impl Into<SharedString>) -> Self {
        Self {
            marker: "#".into(),
            author: author.into(),
            meta: Vec::new(),
            body: body.into(),
            trailing: Vec::new(),
        }
    }

    /// Replaces the `#` marker (`✓` for acknowledgements, `↓` for downtimes).
    pub fn marker(mut self, marker: impl Into<SharedString>) -> Self {
        self.marker = marker.into();
        self
    }

    /// Adds faint text after the author (the time, an expiry).
    pub fn meta(mut self, meta: impl Into<SharedString>) -> Self {
        self.meta.push(meta.into());
        self
    }

    /// The header line: author and meta texts.
    #[must_use]
    pub fn header_text(&self) -> String {
        std::iter::once(self.author.as_ref())
            .chain(self.meta.iter().map(AsRef::as_ref))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl ParentElement for NoteEntry {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.trailing.extend(elements);
    }
}

impl fmt::Debug for NoteEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoteEntry")
            .field("marker", &self.marker)
            .field("header", &self.header_text())
            .field("body", &self.body)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for NoteEntry {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        div()
            .flex()
            .gap(px(12.))
            .text_size(theme.text.body)
            .line_height(relative(1.6))
            .text_color(colors.text_secondary)
            .child(
                div()
                    .flex_none()
                    .text_color(colors.text_faint)
                    .child(self.marker),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_x(px(8.))
                            .child(div().text_color(colors.text).child(self.author))
                            .children(
                                self.meta
                                    .into_iter()
                                    .map(|meta| div().text_color(colors.text_faint).child(meta)),
                            ),
                    )
                    .child(self.body),
            )
            .when(!self.trailing.is_empty(), |note| {
                note.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_start()
                        .gap(px(4.))
                        .children(self.trailing),
                )
            })
    }
}

/// The message shown instead of an empty or failed list: an optional icon or
/// state circle, a title, a muted explanation and an optional extra element
/// (a link, the failing filter).
#[derive(IntoElement)]
#[must_use = "an empty state does nothing unless rendered"]
pub struct EmptyState {
    leading: Option<AnyElement>,
    title: SharedString,
    detail: Option<SharedString>,
    extra: Vec<AnyElement>,
    max_width: Pixels,
}

impl EmptyState {
    /// A message titled `title`.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            leading: None,
            title: title.into(),
            detail: None,
            extra: Vec::new(),
            max_width: px(460.),
        }
    }

    /// Shows an element above the title.
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading = Some(element.into_any_element());
        self
    }

    /// Adds a muted explanation under the title.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Limits the width of the text (default 460px).
    pub fn max_width(mut self, width: Pixels) -> Self {
        self.max_width = width;
        self
    }

    /// The title.
    #[must_use]
    pub fn title(&self) -> &SharedString {
        &self.title
    }
}

impl ParentElement for EmptyState {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.extra.extend(elements);
    }
}

impl fmt::Debug for EmptyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmptyState")
            .field("title", &self.title)
            .field("detail", &self.detail)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .p(px(24.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(10.))
                    .max_w(self.max_width)
                    .text_center()
                    .children(self.leading)
                    .child(
                        div()
                            .text_size(theme.text.row)
                            .text_color(colors.text)
                            .child(self.title),
                    )
                    .when_some(self.detail, |column, detail| {
                        column.child(
                            div()
                                .text_size(theme.text.small)
                                .text_color(colors.text_muted)
                                .child(detail),
                        )
                    })
                    .children(self.extra),
            )
    }
}

/// One line of a [`TreeTable`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeLine {
    /// Nesting level; 0 for top-level keys.
    pub depth: usize,
    /// The key or array index.
    pub key: SharedString,
    /// The value, or a summary (`{3}`, `[2]`) for nested containers.
    pub value: SharedString,
}

impl TreeLine {
    /// A line at `depth`.
    pub fn new(depth: usize, key: impl Into<SharedString>, value: impl Into<SharedString>) -> Self {
        Self {
            depth,
            key: key.into(),
            value: value.into(),
        }
    }
}

/// Nested key/value data (custom variables) as an indented two-column tree.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a tree does nothing unless rendered"]
pub struct TreeTable {
    title: Option<SharedString>,
    lines: Vec<TreeLine>,
    key_width: Pixels,
}

impl TreeTable {
    /// A tree of `lines`, in display order.
    pub fn new(lines: impl IntoIterator<Item = TreeLine>) -> Self {
        Self {
            title: None,
            lines: lines.into_iter().collect(),
            key_width: px(200.),
        }
    }

    /// Sets the section title above the tree.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Sets the width of the key column, indentation included (default 200px).
    pub fn key_width(mut self, width: Pixels) -> Self {
        self.key_width = width;
        self
    }

    /// The lines.
    #[must_use]
    pub fn lines(&self) -> &[TreeLine] {
        &self.lines
    }
}

/// Indentation per nesting level.
const TREE_INDENT: f32 = 16.;

impl RenderOnce for TreeTable {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let key_width = self.key_width;
        div()
            .flex()
            .flex_col()
            .when_some(self.title, |tree, title| {
                tree.child(div().pb(px(6.)).child(SectionLabel::new(title)))
            })
            .children(self.lines.into_iter().map(|line| {
                #[expect(clippy::cast_precision_loss, reason = "nesting depths are tiny")]
                let indent = px(TREE_INDENT * line.depth as f32);
                div()
                    .flex()
                    .gap(px(10.))
                    .py(px(4.))
                    .border_t_1()
                    .border_color(colors.border_row)
                    .text_size(theme.text.body)
                    .child(
                        div()
                            .flex_none()
                            .w(key_width)
                            .pl(indent)
                            .truncate()
                            .text_color(colors.text_muted)
                            .child(line.key),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(colors.text)
                            .child(line.value),
                    )
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_join_author_and_meta() {
        let note = NoteEntry::new("j.berg", "Failover drill")
            .meta("13:58")
            .meta("")
            .meta("expires 16:00");
        assert_eq!(note.header_text(), "j.berg 13:58 expires 16:00");
        assert_eq!(note.marker, "#");
        assert_eq!(NoteEntry::new("a", "b").marker("✓").marker, "✓");
    }

    #[test]
    fn empty_states_keep_their_texts() {
        let empty = EmptyState::new("No service problems").detail("Everything is OK");
        assert_eq!(empty.title(), "No service problems");
        assert!(format!("{empty:?}").contains("Everything is OK"));
    }

    #[test]
    fn trees_keep_line_order() {
        let tree = TreeTable::new([
            TreeLine::new(0, "disks", "{2}"),
            TreeLine::new(1, "/", "{1}"),
            TreeLine::new(2, "warn", "80%"),
        ])
        .title("custom vars");
        let keys: Vec<_> = tree.lines().iter().map(|line| line.key.clone()).collect();
        assert_eq!(keys, ["disks", "/", "warn"]);
        assert_eq!(tree.lines()[2].depth, 2);
    }
}
