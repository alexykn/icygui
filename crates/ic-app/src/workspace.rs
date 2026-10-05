//! The root view: sidebar and main area.
//!
//! M0 shows the empty state (no environment yet) to prove out the theme,
//! fonts and window chrome; M1 replaces it with the dashboard list.

use gpui::{
    Context, Div, FontWeight, IntoElement, ParentElement as _, Render, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};
use ic_ui_kit::{ActiveTheme as _, Theme};

/// Space left in the sidebar header for the native macOS traffic lights.
const TRAFFIC_LIGHT_INSET: f32 = 76.;

pub(crate) struct Workspace;

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex()
            .size_full()
            .bg(theme.colors.window_background)
            .font_family(theme.font_family.clone())
            .text_color(theme.colors.text)
            .child(sidebar(theme))
            .child(main_area(theme))
    }
}

fn sidebar(theme: &Theme) -> Div {
    let colors = &theme.colors;
    let metrics = &theme.metrics;
    let text = &theme.text;

    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .h(metrics.header_height)
        .px_3()
        .border_b_1()
        .border_color(colors.border_header)
        .when(cfg!(target_os = "macos"), |header| {
            header.pl(px(TRAFFIC_LIGHT_INSET))
        })
        .text_size(text.row)
        .text_color(colors.text_muted)
        .child("Search dashboards…");

    let empty = div()
        .flex()
        .items_center()
        .gap_3()
        .h(metrics.item_row_height)
        .px_3()
        .text_size(text.row)
        .text_color(colors.text_muted)
        .child(state_dot(metrics.sidebar_dot, theme))
        .child("No dashboards yet");

    let footer = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .h(metrics.footer_height)
        .px_3()
        .border_t_1()
        .border_color(colors.border_header)
        .text_size(text.hint)
        .text_color(colors.text_faint)
        .child(state_dot(px(6.), theme))
        .child("no environment");

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(metrics.sidebar_width)
        .h_full()
        .border_r_1()
        .border_color(colors.border_split)
        .child(header)
        .child(empty)
        .child(div().flex_1())
        .child(footer)
}

fn main_area(theme: &Theme) -> Div {
    let colors = &theme.colors;
    let text = &theme.text;

    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .h(theme.metrics.header_height)
        .px_4()
        .border_b_1()
        .border_color(colors.border_header)
        .text_size(text.heading)
        .font_weight(FontWeight::MEDIUM)
        .text_color(colors.text_strong)
        .child("Icinga Client");

    let empty = div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_size(text.body)
        .text_color(colors.text_muted)
        .child("Add an environment to start monitoring.");

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(header)
        .child(empty)
}

fn state_dot(size: gpui::Pixels, theme: &Theme) -> Div {
    div().size(size).rounded_full().bg(theme.states.pending)
}
