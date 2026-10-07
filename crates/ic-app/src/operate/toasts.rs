//! The toasts in the window's bottom-right corner (ACT-07): what each
//! action sent to Icinga became, and refusals. Built from
//! `AppState::toasts`; the × dismisses one, and toasts that need no
//! attention go by themselves (the workspace's clock).

use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, div, px,
};
use ic_ui_kit::{Link, Toast, ToastTone as KitTone};

use super::tracker::ToastTone;
use crate::app_state::AppState;

fn tone(tone: ToastTone) -> KitTone {
    match tone {
        ToastTone::Pending => KitTone::Pending,
        ToastTone::Success => KitTone::Success,
        ToastTone::Partial => KitTone::Warning,
        ToastTone::Failed => KitTone::Critical,
        ToastTone::Info => KitTone::Info,
    }
}

/// The toast stack, `bottom` above the window's lower edge (more while
/// the list's selection bar shows), or `None` without toasts.
pub(crate) fn render(state: &Entity<AppState>, bottom: f32, cx: &App) -> Option<AnyElement> {
    let current = state.read(cx);
    let toasts: Vec<AnyElement> = current
        .toasts()
        .map(|toast| {
            let id = toast.id;
            let dismiss = state.clone();
            let mut element = Toast::new(
                SharedString::from(format!("toast-{id}")),
                tone(toast.tone),
                toast.title.clone(),
            );
            for line in toast.shown_lines() {
                element = element.line(line.clone());
            }
            if toast.hidden() > 0 {
                element = element.footer(format!("+ {} more", toast.hidden()));
            }
            if matches!(toast.tone, ToastTone::Partial | ToastTone::Failed) {
                // Every failure, for a ticket or a colleague.
                let text = std::iter::once(toast.title.clone())
                    .chain(toast.lines.iter().cloned())
                    .collect::<Vec<_>>()
                    .join("\n");
                element = element.child(
                    Link::new(SharedString::from(format!("toast-{id}-copy")), "copy")
                        .quiet()
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        }),
                );
            }
            element
                .on_dismiss(move |_: &ClickEvent, _, cx| {
                    dismiss.update(cx, |state, cx| {
                        if state.dismiss_toast(id) {
                            cx.notify();
                        }
                    });
                })
                .into_any_element()
        })
        .collect();
    if toasts.is_empty() {
        return None;
    }
    Some(
        div()
            .id("toasts")
            .absolute()
            .right(px(16.))
            .bottom(px(bottom))
            .flex()
            .flex_col()
            .items_end()
            .gap(px(8.))
            .children(toasts)
            .into_any_element(),
    )
}
