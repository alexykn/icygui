//! Header bars and sub-tabs.

use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use crate::px;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};

use crate::components::{GlyphButton, Tooltip};
use crate::theme::{ActiveTheme as _, Metrics};

type CloseHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// A 40px header bar with a rule underneath: the detail pane's header
/// (`service … × ↗ open as tab`) and the list header
/// (`production service problems … severity ↓ ···`).
///
/// The left side shows a title, a subtitle and/or a muted label; children
/// added with [`ParentElement`] go on the right. The `×` of
/// [`PaneHeader::on_close`] sits left of them, in a fixed place on every
/// platform, so it never sits next to a window's own close button (topic
/// 13); a header that keeps a child's place empty keeps the `×` where it
/// is.
#[derive(IntoElement)]
#[must_use = "a header does nothing unless rendered"]
pub struct PaneHeader {
    id: ElementId,
    title: Option<SharedString>,
    subtitle: Option<SharedString>,
    label: Option<SharedString>,
    padding: Option<Pixels>,
    leading: Vec<AnyElement>,
    status: Option<AnyElement>,
    trailing: Vec<AnyElement>,
    on_close: Option<CloseHandler>,
}

impl PaneHeader {
    /// An empty header; `id` scopes the close button's element id.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            title: None,
            subtitle: None,
            label: None,
            padding: None,
            leading: Vec::new(),
            status: None,
            trailing: Vec::new(),
            on_close: None,
        }
    }

    /// A strong title (13.5px, medium): the dashboard name.
    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Faint text after the title: the view name.
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// A muted label (12px): the pane's object kind (`service`, `host`).
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Sets the horizontal padding (default: [`crate::Metrics::pane_padding`];
    /// the list header uses [`crate::Metrics::list_padding`]).
    pub fn padding(mut self, padding: Pixels) -> Self {
        self.padding = Some(padding);
        self
    }

    /// Adds an element before the title (window controls, a sidebar toggle).
    pub fn leading(mut self, element: impl IntoElement) -> Self {
        self.leading.push(element.into_any_element());
        self
    }

    /// An element right after the title, subtitle and label, at the left
    /// (the pane's `updating` hint). Give it a fixed width when what it
    /// shows comes and goes, so nothing beside it moves.
    pub fn status(mut self, element: impl IntoElement) -> Self {
        self.status = Some(element.into_any_element());
        self
    }

    /// Adds a `×` button that runs `handler`, left of the children at the
    /// right end.
    pub fn on_close(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_close = Some(Box::new(handler));
        self
    }
}

impl ParentElement for PaneHeader {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.trailing.extend(elements);
    }
}

impl fmt::Debug for PaneHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PaneHeader")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("subtitle", &self.subtitle)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for PaneHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let text = theme.text;
        let metrics = theme.metrics;
        let close_id = ElementId::NamedChild(Arc::new(self.id), "close".into());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(Metrics::with_rule(metrics.header_height))
            .px(self.padding.unwrap_or(metrics.pane_padding))
            .border_b_1()
            .border_color(colors.border_header)
            .whitespace_nowrap()
            .overflow_hidden()
            .children(self.leading)
            .when_some(self.label, |header, label| {
                header.child(
                    div()
                        .text_size(text.small)
                        .text_color(colors.text_muted)
                        .child(label),
                )
            })
            .when_some(self.title, |header, title| {
                header.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(text.heading)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors.text_strong)
                        .child(title),
                )
            })
            .when_some(self.subtitle, |header, subtitle| {
                header.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(text.small)
                        .text_color(colors.text_faint)
                        .child(subtitle),
                )
            })
            .children(self.status)
            .child(div().flex_1())
            .when_some(self.on_close, |header, handler| {
                header.child(
                    GlyphButton::new(close_id, "×")
                        .text_size(px(15.))
                        .bleed()
                        .tooltip(Tooltip::new("Close"))
                        .on_click(handler),
                )
            })
            .children(self.trailing)
    }
}

type SelectHandler = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

#[derive(Clone, Debug)]
struct Tab {
    label: SharedString,
    count: Option<usize>,
    marked: bool,
}

/// Text tabs with an accent underline under the selected one: the host
/// pane's `services 23 · history · vars · config`.
#[derive(IntoElement)]
#[must_use = "tabs do nothing unless rendered"]
pub struct SubTabs {
    id: ElementId,
    tabs: Vec<Tab>,
    selected: usize,
    on_select: Option<SelectHandler>,
    trailing: Option<AnyElement>,
}

impl SubTabs {
    /// Tabs without entries; `id` scopes each tab's element id.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            tabs: Vec::new(),
            selected: 0,
            on_select: None,
            trailing: None,
        }
    }

    /// Adds a tab.
    pub fn tab(mut self, label: impl Into<SharedString>) -> Self {
        self.tabs.push(Tab {
            label: label.into(),
            count: None,
            marked: false,
        });
        self
    }

    /// Adds a tab with a count after its label (`services 23`).
    pub fn tab_with_count(mut self, label: impl Into<SharedString>, count: usize) -> Self {
        self.tabs.push(Tab {
            label: label.into(),
            count: Some(count),
            marked: false,
        });
        self
    }

    /// Adds a tab whose label, while `marked` and not selected, shows in
    /// the accent colour (something new behind it, such as unread
    /// notifications). Colour only: no dot or count, so nothing moves when
    /// the mark comes or goes.
    pub fn marked_tab(mut self, label: impl Into<SharedString>, marked: bool) -> Self {
        self.tabs.push(Tab {
            label: label.into(),
            count: None,
            marked,
        });
        self
    }

    /// Puts `element` after the last tab, on the tabs' baseline (an
    /// overflow `···` for tabs that don't fit: the row never wraps).
    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    /// The tab labels, in order (counts included).
    #[must_use]
    pub fn labels(&self) -> Vec<SharedString> {
        self.tabs.iter().map(Self::label).collect()
    }

    /// Selects the tab at `index` (out-of-range indices select nothing).
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Runs `handler` with the tab's index when a tab is clicked.
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    fn label(tab: &Tab) -> SharedString {
        match tab.count {
            Some(count) => format!("{} {count}", tab.label).into(),
            None => tab.label.clone(),
        }
    }
}

impl fmt::Debug for SubTabs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubTabs")
            .field("id", &self.id)
            .field("tabs", &self.tabs)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for SubTabs {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let parent_id = Arc::new(self.id);
        div()
            .flex()
            .flex_none()
            .gap(px(TAB_GAP))
            .text_size(theme.text.body)
            .whitespace_nowrap()
            .overflow_hidden()
            .border_b_1()
            .border_color(colors.border_header)
            .children(self.tabs.iter().enumerate().map(|(index, tab)| {
                let selected = index == self.selected;
                let marked = tab.marked && !selected;
                let on_select = self.on_select.clone();
                div()
                    .id(ElementId::NamedChild(
                        parent_id.clone(),
                        SharedString::from(format!("tab-{index}")),
                    ))
                    .pb(px(TAB_PADDING))
                    .border_b(px(2.))
                    .border_color(if selected {
                        colors.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .flex_none()
                    .text_color(if selected {
                        colors.text_strong
                    } else if marked {
                        colors.accent_text
                    } else {
                        colors.text_muted
                    })
                    .whitespace_nowrap()
                    .child(Self::label(tab))
                    .when(!selected, |tab| {
                        tab.cursor_pointer().hover(|style| {
                            style.text_color(if marked {
                                colors.accent_hover
                            } else {
                                colors.text
                            })
                        })
                    })
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .when_some(on_select, |tab, on_select| {
                        tab.on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            on_select(index, window, cx);
                        })
                    })
            }))
            .when_some(self.trailing, |tabs, trailing| {
                // On the labels' line: the same space under it as a tab.
                tabs.child(div().flex_none().pb(px(TAB_PADDING + 2.)).child(trailing))
            })
    }
}

/// Space between two tabs.
const TAB_GAP: f32 = 22.;
/// Space between a tab's label and its underline.
const TAB_PADDING: f32 = 9.;

/// How wide [`SubTabs`] draws a tab labelled `label` (with its count) at
/// `text_size`, gap excluded: the bundled font is monospaced, so callers
/// can tell which tabs fit before drawing them (an overflow menu for the
/// rest, so the row never wraps).
#[must_use]
pub fn sub_tab_width(label: &str, text_size: Pixels) -> Pixels {
    #[expect(
        clippy::cast_precision_loss,
        reason = "labels are far shorter than 2^23 characters"
    )]
    let chars = label.chars().count() as f32;
    text_size * (chars * crate::theme::CHAR_WIDTH)
}

/// The space [`SubTabs`] puts between two tabs, at the interface size.
#[must_use]
pub fn sub_tab_gap() -> Pixels {
    px(TAB_GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_labels_include_counts() {
        let tabs = SubTabs::new("host-tabs")
            .tab_with_count("services", 23)
            .tab("history")
            .selected(1);
        let labels: Vec<_> = tabs.tabs.iter().map(SubTabs::label).collect();
        assert_eq!(labels, ["services 23", "history"]);
        assert_eq!(tabs.selected, 1);
    }

    #[test]
    fn headers_collect_their_parts() {
        let header = PaneHeader::new("pane")
            .label("service")
            .title("postgres-replication")
            .subtitle("service problems")
            .status(div())
            .child(div())
            .on_close(|_, _, _| {});
        assert_eq!(header.label.as_deref(), Some("service"));
        assert!(header.status.is_some());
        assert_eq!(header.trailing.len(), 1);
        assert!(header.on_close.is_some());
        assert!(format!("{header:?}").contains("postgres-replication"));
    }
}
