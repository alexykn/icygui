//! The selection bar under the list while rows are marked (ACT-07):
//! `3 selected · acknowledge a · downtime d · check now r · comment c ·
//! ··· · clear esc`. The keys do the same; the bar makes bulk actions
//! visible. It sits at the list's bottom edge, so marking rows never moves
//! the rows themselves (a shift-click lands where it was aimed).

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, Pixels, Styled as _, div, prelude::FluentBuilder as _, px,
};
use ic_model::ObjectKey;
use ic_rules::DashboardRef;
use ic_ui_kit::{
    ActiveTheme as _, Button, GlyphButton, KeyHint, Link, Menu, MenuItem, Popover, Tooltip,
};

use super::DashboardView;
use super::header::HeaderMenu;
use crate::actions::ObjectAction;
use crate::menu_state::down_position;
use crate::operate::expression;

/// The selection bar's height.
pub(crate) const SELECTION_BAR_HEIGHT: Pixels = px(40.);

/// Below this list width the bar drops its key hints and shortens labels.
const COMPACT_BELOW: f32 = 760.;

/// The bar's buttons: id, label, short label, key, action.
const BUTTONS: [(&str, &str, &str, &str, ObjectAction); 4] = [
    (
        "bulk-acknowledge",
        "acknowledge",
        "ack",
        "a",
        ObjectAction::Acknowledge,
    ),
    (
        "bulk-downtime",
        "downtime",
        "downtime",
        "d",
        ObjectAction::ScheduleDowntime,
    ),
    (
        "bulk-check",
        "check now",
        "check",
        "r",
        ObjectAction::CheckNow,
    ),
    (
        "bulk-comment",
        "comment",
        "comment",
        "c",
        ObjectAction::AddComment,
    ),
];

impl DashboardView {
    /// The bar for `reference`'s marked rows, if any are marked.
    pub(super) fn render_selection_bar(
        &self,
        reference: &DashboardRef,
        list_width: Pixels,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let marked = self.lists.get(reference)?.selection.marked_keys();
        if marked.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let colors = theme.colors;
        let compact = list_width < px(COMPACT_BELOW);
        let buttons = BUTTONS
            .iter()
            .enumerate()
            .map(|(index, (id, label, short, key, action))| {
                let button = self.bulk_button(id, if compact { short } else { label }, action, cx);
                let button = if compact {
                    button
                } else {
                    button.key_hint(*key)
                };
                if index == 0 { button.primary() } else { button }
            });
        Some(
            div()
                .id("selection-bar")
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .h(SELECTION_BAR_HEIGHT)
                .px(theme.metrics.list_padding)
                .border_t_1()
                .border_color(colors.border_header)
                .bg(colors.pane_background)
                .text_size(theme.text.small)
                .child(
                    div()
                        .flex_none()
                        .mr(px(4.))
                        .text_color(colors.accent)
                        .child(format!("{} selected", marked.len())),
                )
                .children(buttons)
                .child(self.selection_more(&marked, cx))
                .child(div().flex_1())
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .child(Link::new("bulk-clear", "clear").quiet().on_click(
                            cx.listener(|this, _: &ClickEvent, _, cx| this.clear_marks(cx)),
                        ))
                        .when(!compact, |clear| clear.child(KeyHint::new("esc"))),
                )
                .into_any_element(),
        )
    }

    /// A bulk action's button: disabled with the reason when the API user
    /// may not run it (ENV-09).
    fn bulk_button(
        &self,
        id: &'static str,
        label: &'static str,
        action: &ObjectAction,
        cx: &Context<Self>,
    ) -> Button {
        let button = Button::new(id, label);
        if let Some(denial) = self.state.read(cx).action_denial(action) {
            return button.disabled(true).tooltip(Tooltip::new(denial));
        }
        let action = action.clone();
        button.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.request(action.clone(), cx);
        }))
    }

    /// The bar's `···` and its menu.
    fn selection_more(&self, marked: &[ObjectKey], cx: &Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let open = self.menus.open() == Some(HeaderMenu::Selection);
        div()
            .relative()
            .flex_none()
            .child(
                GlyphButton::new("bulk-more", "···")
                    .text_size(px(13.))
                    .color(colors.text_muted)
                    .selected(open)
                    .tooltip(Tooltip::new("More for the selection"))
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.menus
                            .toggle(HeaderMenu::Selection, down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |trigger| {
                trigger.child(Popover::new(self.selection_menu(marked, cx)).above())
            })
            .into_any_element()
    }

    /// The bar's `···`: the actions without a key, and copying.
    fn selection_menu(&self, marked: &[ObjectKey], cx: &Context<Self>) -> Menu {
        let state = self.state.read(cx);
        let item = |id: &'static str, label: &'static str, action: ObjectAction| {
            let item = MenuItem::new(id, label);
            match state.action_denial(&action) {
                Some(denial) => item.disabled(true).tooltip(Tooltip::new(denial)),
                None => item.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.request(action.clone(), cx);
                })),
            }
        };
        let copy = |id: &'static str, label: &'static str, what: &'static str, text: String| {
            MenuItem::new(id, label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.menus.close();
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                this.state.update(cx, |state, cx| {
                    state.inform(format!("Copied {what}"), None);
                    cx.notify();
                });
            }))
        };
        Menu::new("selection-menu")
            .item(item(
                "bulk-result",
                "submit check result",
                ObjectAction::SubmitCheckResult,
            ))
            .item(item(
                "bulk-command",
                "run command",
                ObjectAction::RunCommand,
            ))
            .separator()
            .item(item(
                "bulk-remove-ack",
                "remove acknowledgements",
                ObjectAction::RemoveAcknowledgement,
            ))
            .item(item(
                "bulk-remove-downtimes",
                "remove downtimes",
                ObjectAction::RemoveDowntimes,
            ))
            .separator()
            .item(copy(
                "bulk-copy-names",
                "copy names",
                "the names",
                expression::names(marked),
            ))
            .item(copy(
                "bulk-copy-filter",
                "copy filter expression",
                "the filter expression",
                expression::filter(marked),
            ))
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// Unmarks every row.
    fn clear_marks(&mut self, cx: &mut Context<Self>) {
        if let Some(reference) = self.sync(cx)
            && let Some(list) = self.lists.get_mut(&reference)
            && list.selection.clear_marks()
        {
            cx.notify();
        }
    }
}
