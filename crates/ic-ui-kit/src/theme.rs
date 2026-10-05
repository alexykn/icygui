//! Design tokens from `project/Icinga Client v2.dc.html`.
//!
//! Views never hard-code colours or sizes; they read them from the active
//! [`Theme`] (`cx.theme()`), so a light theme is another `Theme` value.

use gpui::{App, Global, Hsla, Pixels, SharedString, px, rgb};
use ic_model::{HostState, ServiceState};

/// The family name of the bundled UI font.
pub const FONT_FAMILY: &str = "IBM Plex Mono";

/// All design tokens: colours, type scale and layout metrics.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Font family used everywhere (the design is monospace throughout).
    pub font_family: SharedString,
    /// Surface, border, text and accent colours.
    pub colors: Colors,
    /// Host and service state colours.
    pub states: StateColors,
    /// Font sizes.
    pub text: TextSizes,
    /// Fixed sizes from the layout.
    pub metrics: Metrics,
}

impl Global for Theme {}

impl Theme {
    /// The dark theme from the v2 design.
    #[must_use]
    pub fn dark() -> Self {
        Self {
            font_family: FONT_FAMILY.into(),
            colors: Colors::dark(),
            states: StateColors::dark(),
            text: TextSizes::default(),
            metrics: Metrics::default(),
        }
    }
}

/// Surface, border, text and accent colours.
#[derive(Clone, Copy, Debug)]
pub struct Colors {
    /// Main window background (`#1d2125`).
    pub window_background: Hsla,
    /// Detail pane background (`#1a1d21`).
    pub pane_background: Hsla,
    /// Plugin output and other code blocks (`#16191c`).
    pub code_background: Hsla,
    /// Secondary buttons (`#262a2e`).
    pub element_background: Hsla,
    /// Selected list row (`#2a3036`).
    pub row_selected: Hsla,
    /// Active group header in the sidebar (`#30353a`).
    pub group_active: Hsla,
    /// Active dashboard in the sidebar (`#272c30`).
    pub item_active: Hsla,

    /// Window outline and the titlebar divider (`#33383c`).
    pub border_window: Hsla,
    /// Vertical split between sidebar, list and pane (`#2e3337`).
    pub border_split: Hsla,
    /// Rules under header bars (`#2a2f33`).
    pub border_header: Hsla,
    /// Rules between list rows (`#262a2e`).
    pub border_row: Hsla,

    /// Active sidebar labels (`#f2f3f4`).
    pub text_emphasis: Hsla,
    /// Titles and primary row text (`#eceeef`).
    pub text_strong: Hsla,
    /// Body text (`#d6d8da`).
    pub text: Hsla,
    /// Host names and inactive sidebar labels (`#b5b9bc`).
    pub text_secondary: Hsla,
    /// Plugin output and secondary labels (`#8b9094`).
    pub text_muted: Hsla,
    /// Section labels, tags and key hints (`#6c7175`).
    pub text_faint: Hsla,
    /// Text inside code blocks (`#c4c7ca`).
    pub text_code: Hsla,

    /// Links, focus and the primary button (`#74ade8`).
    pub accent: Hsla,
    /// Hovered links (`#a3c8f0`).
    pub accent_hover: Hsla,
    /// Text on the primary button (`#10161d`).
    pub on_accent: Hsla,
    /// Key hints on the primary button (`#2b4766`).
    pub on_accent_muted: Hsla,

    /// Window control: close (`#ff5f57`).
    pub traffic_close: Hsla,
    /// Window control: minimise (`#febc2e`).
    pub traffic_minimize: Hsla,
    /// Window control: zoom (`#28c840`).
    pub traffic_zoom: Hsla,
}

impl Colors {
    fn dark() -> Self {
        Self {
            window_background: hex(0x1d_2125),
            pane_background: hex(0x1a_1d21),
            code_background: hex(0x16_191c),
            element_background: hex(0x26_2a2e),
            row_selected: hex(0x2a_3036),
            group_active: hex(0x30_353a),
            item_active: hex(0x27_2c30),

            border_window: hex(0x33_383c),
            border_split: hex(0x2e_3337),
            border_header: hex(0x2a_2f33),
            border_row: hex(0x26_2a2e),

            text_emphasis: hex(0xf2_f3f4),
            text_strong: hex(0xec_eeef),
            text: hex(0xd6_d8da),
            text_secondary: hex(0xb5_b9bc),
            text_muted: hex(0x8b_9094),
            text_faint: hex(0x6c_7175),
            text_code: hex(0xc4_c7ca),

            accent: hex(0x74_ade8),
            accent_hover: hex(0xa3_c8f0),
            on_accent: hex(0x10_161d),
            on_accent_muted: hex(0x2b_4766),

            traffic_close: hex(0xff_5f57),
            traffic_minimize: hex(0xfe_bc2e),
            traffic_zoom: hex(0x28_c840),
        }
    }
}

/// State colours. Hosts reuse the service palette: up = ok, down = critical,
/// unreachable = unknown.
#[derive(Clone, Copy, Debug)]
pub struct StateColors {
    /// OK / up (`#56b870`).
    pub ok: Hsla,
    /// Warning (`#e5b04a`).
    pub warning: Hsla,
    /// Critical / down (`#e06c6c`).
    pub critical: Hsla,
    /// Unknown / unreachable (`#a97fdb`).
    pub unknown: Hsla,
    /// Pending, and dashboards without objects (`#3a3f43`).
    pub pending: Hsla,
}

impl StateColors {
    fn dark() -> Self {
        Self {
            ok: hex(0x56_b870),
            warning: hex(0xe5_b04a),
            critical: hex(0xe0_6c6c),
            unknown: hex(0xa9_7fdb),
            pending: hex(0x3a_3f43),
        }
    }

    /// The colour for a service state.
    #[must_use]
    pub fn service(&self, state: ServiceState) -> Hsla {
        match state {
            ServiceState::Ok => self.ok,
            ServiceState::Warning => self.warning,
            ServiceState::Critical => self.critical,
            ServiceState::Unknown => self.unknown,
            ServiceState::Pending => self.pending,
        }
    }

    /// The colour for a host state.
    #[must_use]
    pub fn host(&self, state: HostState) -> Hsla {
        match state {
            HostState::Up => self.ok,
            HostState::Down => self.critical,
            HostState::Unreachable => self.unknown,
            HostState::Pending => self.pending,
        }
    }
}

/// The type scale. Names describe where each size is used in the design.
#[derive(Clone, Copy, Debug)]
pub struct TextSizes {
    /// Time-in-state under list circles, state labels (10.5px).
    pub caption: Pixels,
    /// Key hints, footer status, group chevrons (11px).
    pub hint: Pixels,
    /// Counts, row tags, section labels (11.5px).
    pub label: Pixels,
    /// Plugin output in rows, summary bar, pane header (12px).
    pub small: Pixels,
    /// Pane body text and buttons (12.5px).
    pub body: Pixels,
    /// Row titles and sidebar items (13px).
    pub row: Pixels,
    /// Header titles and group names (13.5px).
    pub heading: Pixels,
    /// Object name in the detail pane (18px).
    pub title: Pixels,
}

impl Default for TextSizes {
    fn default() -> Self {
        Self {
            caption: px(10.5),
            hint: px(11.),
            label: px(11.5),
            small: px(12.),
            body: px(12.5),
            row: px(13.),
            heading: px(13.5),
            title: px(18.),
        }
    }
}

/// Fixed sizes from the layout.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    /// Sidebar width.
    pub sidebar_width: Pixels,
    /// Detail pane width when split beside the list.
    pub pane_width: Pixels,
    /// Header bars (titlebar row, list header, pane header).
    pub header_height: Pixels,
    /// State summary bar above the list.
    pub summary_bar_height: Pixels,
    /// Sidebar group row.
    pub group_row_height: Pixels,
    /// Sidebar dashboard row.
    pub item_row_height: Pixels,
    /// Sidebar footer bar.
    pub footer_height: Pixels,
    /// Action buttons.
    pub button_height: Pixels,
    /// Action button corner radius.
    pub button_radius: Pixels,
    /// State circle in list rows.
    pub row_circle: Pixels,
    /// State circle in the host pane's service rows.
    pub compact_circle: Pixels,
    /// State circle in the detail pane header.
    pub pane_circle: Pixels,
    /// Dashboard state dot in the sidebar.
    pub sidebar_dot: Pixels,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            sidebar_width: px(300.),
            pane_width: px(620.),
            header_height: px(40.),
            summary_bar_height: px(36.),
            group_row_height: px(36.),
            item_row_height: px(30.),
            footer_height: px(38.),
            button_height: px(28.),
            button_radius: px(5.),
            row_circle: px(22.),
            compact_circle: px(14.),
            pane_circle: px(34.),
            sidebar_dot: px(8.),
        }
    }
}

/// Access to the active theme from any GPUI context (`cx.theme()`).
pub trait ActiveTheme {
    /// The active theme.
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

fn hex(rgb_value: u32) -> Hsla {
    rgb(rgb_value).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_states_reuse_the_service_palette() {
        let states = StateColors::dark();
        assert_eq!(states.host(HostState::Up), states.service(ServiceState::Ok));
        assert_eq!(
            states.host(HostState::Down),
            states.service(ServiceState::Critical)
        );
        assert_eq!(
            states.host(HostState::Unreachable),
            states.service(ServiceState::Unknown)
        );
    }

    #[test]
    fn tokens_match_the_design() {
        let colors = Colors::dark();
        assert_eq!(colors.window_background, hex(0x1d_2125));
        assert_eq!(colors.accent, hex(0x74_ade8));
        assert_eq!(Metrics::default().sidebar_width, px(300.));
    }
}
