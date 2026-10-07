//! Design tokens from `design/project/Icinga Client v2.dc.html` (dark) and
//! `design/v1/` topic 03 (light).
//!
//! Views never hard-code colours or sizes; they read them from the active
//! [`Theme`] (`cx.theme()`). The light theme is another `Theme` value with
//! the same fields; the interface size and the row density are built into
//! the theme's type scale and metrics ([`Theme::new`]).
#![expect(
    clippy::disallowed_methods,
    reason = "the design's sizes at 100 %, in real pixels: Theme::new scales them"
)]

use gpui::{
    App, DefiniteLength, Global, Hsla, Pixels, Rgba, SharedString, hsla, px, relative, rgb, rgba,
};
use ic_model::{CheckableState, HostState, PerfdataStatus, ServiceState};

use crate::scale;

/// The family name of the bundled UI font.
pub const FONT_FAMILY: &str = "IBM Plex Mono";

/// The bundled font's natural line height (its ascender plus descender),
/// which is what the design's CSS `line-height: normal` resolves to.
pub const LINE_HEIGHT: f32 = 1.3;

/// The bundled font's advance width relative to its size: IBM Plex Mono is
/// monospaced, 600 units to the em, so a line's width follows from its
/// length.
pub const CHAR_WIDTH: f32 = 0.6;

/// Light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ThemeMode {
    /// The v2 design's dark theme.
    #[default]
    Dark,
    /// The light theme of topic 03.
    Light,
}

/// How tall list rows are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Density {
    /// Two lines per row: the name, and the plugin output under it, with
    /// the time in state under the state circle (60px).
    #[default]
    Comfortable,
    /// One line per row: a smaller circle, the name, and the time at the
    /// right; no output line (32px).
    Compact,
}

/// All design tokens: colours, type scale and layout metrics.
#[derive(Clone, Debug)]
pub struct Theme {
    /// Light or dark.
    pub mode: ThemeMode,
    /// Font family used everywhere (the design is monospace throughout).
    pub font_family: SharedString,
    /// Line height of running text, relative to the font size. Set it on the
    /// window's root element; code blocks use their own, looser one.
    pub line_height: DefiniteLength,
    /// Surface, border, text and accent colours.
    pub colors: Colors,
    /// Host and service state colours, a fill and a text shade each.
    pub states: StateColors,
    /// Font sizes, at the interface size.
    pub text: TextSizes,
    /// Fixed sizes from the layout, at the interface size and row density.
    pub metrics: Metrics,
    /// The interface size: the factor text and spacing are scaled by (1.0 at
    /// 100 %). Lengths written in views follow it through [`crate::px`].
    pub scale: f32,
    /// How tall list rows are.
    pub density: Density,
}

impl Global for Theme {}

impl Theme {
    /// The dark theme from the v2 design, at 100 % with comfortable rows.
    #[must_use]
    pub fn dark() -> Self {
        Self::new(ThemeMode::Dark, 1., Density::Comfortable)
    }

    /// The light theme (topic 03), at 100 % with comfortable rows.
    #[must_use]
    pub fn light() -> Self {
        Self::new(ThemeMode::Light, 1., Density::Comfortable)
    }

    /// The `mode` theme with text and spacing scaled by `scale` (kept
    /// between 0.5 and 2) and list rows at `density`.
    #[must_use]
    pub fn new(mode: ThemeMode, scale: f32, density: Density) -> Self {
        let scale = scale::clamp(scale);
        let (colors, states) = match mode {
            ThemeMode::Dark => (Colors::dark(), StateColors::dark()),
            ThemeMode::Light => (Colors::light(), StateColors::light()),
        };
        Self {
            mode,
            font_family: FONT_FAMILY.into(),
            line_height: relative(LINE_HEIGHT),
            colors,
            states,
            text: TextSizes::default().scaled(scale),
            metrics: Metrics::default().scaled(scale, density),
            scale,
            density,
        }
    }

    /// The colour of a perfdata value: state text colours when a threshold
    /// is crossed, body text otherwise.
    #[must_use]
    pub fn perfdata_color(&self, status: PerfdataStatus) -> Hsla {
        match status {
            PerfdataStatus::Ok => self.colors.text,
            PerfdataStatus::Warning => self.states.text.warning,
            PerfdataStatus::Critical => self.states.text.critical,
        }
    }
}

/// Surface, border, text and accent colours. The values in the comments
/// are the dark theme's; [`Colors::light`] has the light ones.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    /// Main window background (`#1d2125`).
    pub window_background: Hsla,
    /// The sidebar and the settings panel's navigation: the window's
    /// surface in dark, slightly grey in light (`#f4f5f7`).
    pub sidebar_background: Hsla,
    /// Detail pane background (`#1a1d21`).
    pub pane_background: Hsla,
    /// Plugin output and other code blocks (`#16191c`).
    pub code_background: Hsla,
    /// Secondary buttons and tooltips (`#262a2e`).
    pub element_background: Hsla,
    /// Hovered secondary buttons and icon buttons (`#2e3337`).
    pub element_hover: Hsla,
    /// Pressed secondary buttons and icon buttons (`#33383c`).
    pub element_active: Hsla,
    /// Selected list row (`#2a3036`).
    pub row_selected: Hsla,
    /// Hovered list row (`#22262a`).
    pub row_hover: Hsla,
    /// Rows marked for a bulk action (`#25303a`, a dark accent tint).
    pub row_marked: Hsla,
    /// Group header rows in lists (`#16191c`, the code block surface: a
    /// band darker than the rows under it).
    pub row_header: Hsla,
    /// Active group header in the sidebar (`#30353a`).
    pub group_active: Hsla,
    /// Hovered, inactive group header in the sidebar (`#262a2e`).
    pub group_hover: Hsla,
    /// Active dashboard in the sidebar (`#272c30`).
    pub item_active: Hsla,
    /// Hovered, inactive dashboard in the sidebar (`#23272b`).
    pub item_hover: Hsla,

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
    /// Host names, inactive sidebar labels and footer icons (`#b5b9bc`).
    pub text_secondary: Hsla,
    /// Plugin output, placeholders and secondary labels (`#8b9094`).
    pub text_muted: Hsla,
    /// Section labels, tags and key hints (`#6c7175`).
    pub text_faint: Hsla,
    /// Text inside code blocks (`#c4c7ca`).
    pub text_code: Hsla,

    /// Links, focus and the primary button (`#74ade8`): the accent's fill
    /// shade, for bars, borders, tracks and icons.
    pub accent: Hsla,
    /// The accent's text shade, for words in the accent colour (links,
    /// highlights, `1h 48m left`): the accent in dark, a darker blue in
    /// light that keeps 4.5:1 on the selected and marked rows.
    pub accent_text: Hsla,
    /// Hovered links (`#a3c8f0`).
    pub accent_hover: Hsla,
    /// Hovered primary button (`#86b9ec`).
    pub accent_button_hover: Hsla,
    /// Text on the primary button (`#10161d`).
    pub on_accent: Hsla,
    /// Key hints on the primary button (`#2b4766`).
    pub on_accent_muted: Hsla,
    /// Selected text in inputs (accent at 30 % opacity).
    pub selection: Hsla,
    /// A faint accent wash: a banner for information, the selected view's
    /// header (accent at 8 %).
    pub accent_tint: Hsla,
    /// A stronger accent wash, for a hovered tinted surface (accent at
    /// 14 %).
    pub accent_tint_strong: Hsla,
    /// A faint warning wash: a warning banner or note (warning at 8 %).
    pub warning_tint: Hsla,
    /// A faint critical wash: a critical banner (critical at 8 %).
    pub critical_tint: Hsla,

    /// A switch's thumb while off (`#8b9094`, the muted text).
    pub switch_thumb: Hsla,
    /// A switch's thumb while on, over the accent track (`#f2f3f4`).
    pub switch_thumb_on: Hsla,
    /// The shadow that lifts a white thumb off a light track (none in
    /// dark).
    pub switch_thumb_shadow: Hsla,

    /// Window control: close (`#ff5f57`).
    pub traffic_close: Hsla,
    /// Window control: minimise (`#febc2e`).
    pub traffic_minimize: Hsla,
    /// Window control: zoom (`#28c840`).
    pub traffic_zoom: Hsla,
    /// Window controls while the window is inactive (`#3a3f43`).
    pub traffic_inactive: Hsla,
    /// Glyphs inside the window controls on hover (black at 55 %).
    pub traffic_glyph: Hsla,

    /// Drop shadow under tooltips (black at 35 %).
    pub shadow: Hsla,
    /// Drop shadow under menus and other popovers, which float higher
    /// (black at 45 %).
    pub shadow_strong: Hsla,
    /// Drop shadow under modal cards, which float highest (black at 60 %).
    pub shadow_modal: Hsla,
    /// The dimmed window behind a modal card (`#08090b` at 55 %, the
    /// command palette's in the design).
    pub backdrop: Hsla,
}

impl Colors {
    /// Every colour with its name. Lists every field (the destructuring
    /// fails to compile when a field is added and not listed), so checks
    /// over the whole palette cover new tokens too.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "one line per token, twice: the destructuring and the list"
    )]
    pub fn tokens(&self) -> [(&'static str, Hsla); 49] {
        let Self {
            window_background,
            sidebar_background,
            pane_background,
            code_background,
            element_background,
            element_hover,
            element_active,
            row_selected,
            row_hover,
            row_marked,
            row_header,
            group_active,
            group_hover,
            item_active,
            item_hover,
            border_window,
            border_split,
            border_header,
            border_row,
            text_emphasis,
            text_strong,
            text,
            text_secondary,
            text_muted,
            text_faint,
            text_code,
            accent,
            accent_text,
            accent_hover,
            accent_button_hover,
            on_accent,
            on_accent_muted,
            selection,
            accent_tint,
            accent_tint_strong,
            warning_tint,
            critical_tint,
            switch_thumb,
            switch_thumb_on,
            switch_thumb_shadow,
            traffic_close,
            traffic_minimize,
            traffic_zoom,
            traffic_inactive,
            traffic_glyph,
            shadow,
            shadow_strong,
            shadow_modal,
            backdrop,
        } = *self;
        [
            ("window_background", window_background),
            ("sidebar_background", sidebar_background),
            ("pane_background", pane_background),
            ("code_background", code_background),
            ("element_background", element_background),
            ("element_hover", element_hover),
            ("element_active", element_active),
            ("row_selected", row_selected),
            ("row_hover", row_hover),
            ("row_marked", row_marked),
            ("row_header", row_header),
            ("group_active", group_active),
            ("group_hover", group_hover),
            ("item_active", item_active),
            ("item_hover", item_hover),
            ("border_window", border_window),
            ("border_split", border_split),
            ("border_header", border_header),
            ("border_row", border_row),
            ("text_emphasis", text_emphasis),
            ("text_strong", text_strong),
            ("text", text),
            ("text_secondary", text_secondary),
            ("text_muted", text_muted),
            ("text_faint", text_faint),
            ("text_code", text_code),
            ("accent", accent),
            ("accent_text", accent_text),
            ("accent_hover", accent_hover),
            ("accent_button_hover", accent_button_hover),
            ("on_accent", on_accent),
            ("on_accent_muted", on_accent_muted),
            ("selection", selection),
            ("accent_tint", accent_tint),
            ("accent_tint_strong", accent_tint_strong),
            ("warning_tint", warning_tint),
            ("critical_tint", critical_tint),
            ("switch_thumb", switch_thumb),
            ("switch_thumb_on", switch_thumb_on),
            ("switch_thumb_shadow", switch_thumb_shadow),
            ("traffic_close", traffic_close),
            ("traffic_minimize", traffic_minimize),
            ("traffic_zoom", traffic_zoom),
            ("traffic_inactive", traffic_inactive),
            ("traffic_glyph", traffic_glyph),
            ("shadow", shadow),
            ("shadow_strong", shadow_strong),
            ("shadow_modal", shadow_modal),
            ("backdrop", backdrop),
        ]
    }

    /// The dark theme's colours (the v2 design).
    #[must_use]
    pub fn dark() -> Self {
        Self {
            window_background: hex(0x1d_2125),
            sidebar_background: hex(0x1d_2125),
            pane_background: hex(0x1a_1d21),
            code_background: hex(0x16_191c),
            element_background: hex(0x26_2a2e),
            element_hover: hex(0x2e_3337),
            element_active: hex(0x33_383c),
            row_selected: hex(0x2a_3036),
            row_hover: hex(0x22_262a),
            row_marked: hex(0x25_303a),
            row_header: hex(0x16_191c),
            group_active: hex(0x30_353a),
            group_hover: hex(0x26_2a2e),
            item_active: hex(0x27_2c30),
            item_hover: hex(0x23_272b),

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
            accent_text: hex(0x74_ade8),
            accent_hover: hex(0xa3_c8f0),
            accent_button_hover: hex(0x86_b9ec),
            on_accent: hex(0x10_161d),
            on_accent_muted: hex(0x2b_4766),
            selection: rgba(0x74ad_e84d).into(),
            accent_tint: hex(0x74_ade8).opacity(0.08),
            accent_tint_strong: hex(0x74_ade8).opacity(0.14),
            warning_tint: hex(0xe5_b04a).opacity(0.08),
            critical_tint: hex(0xe0_6c6c).opacity(0.08),

            switch_thumb: hex(0x8b_9094),
            switch_thumb_on: hex(0xf2_f3f4),
            switch_thumb_shadow: hsla(0., 0., 0., 0.),

            traffic_close: hex(0xff_5f57),
            traffic_minimize: hex(0xfe_bc2e),
            traffic_zoom: hex(0x28_c840),
            traffic_inactive: hex(0x3a_3f43),
            traffic_glyph: rgba(0x0000_008c).into(),

            shadow: hsla(0., 0., 0., 0.35),
            shadow_strong: hsla(0., 0., 0., 0.45),
            shadow_modal: hsla(0., 0., 0., 0.6),
            backdrop: rgba(0x0809_0b8c).into(),
        }
    }

    /// The light theme's colours (topic 03): the same structure as dark.
    /// The window is white, the pane a band off it, code blocks a band
    /// further; the sidebar is slightly grey; text keeps the dark theme's
    /// contrast steps; selection is tinted towards the accent.
    #[must_use]
    pub fn light() -> Self {
        Self {
            window_background: hex(0xff_ffff),
            sidebar_background: hex(0xf4_f5f7),
            pane_background: hex(0xf7_f8f9),
            code_background: hex(0xf2_f4f6),
            element_background: hex(0xed_f0f2),
            element_hover: hex(0xe4_e8eb),
            element_active: hex(0xda_dfe3),
            row_selected: hex(0xe3_ebf4),
            row_hover: hex(0xf4_f6f8),
            row_marked: hex(0xdb_e8f7),
            row_header: hex(0xf2_f4f6),
            group_active: hex(0xe1_e5e9),
            group_hover: hex(0xe9_ecef),
            item_active: hex(0xe6_e9ed),
            item_hover: hex(0xec_eef1),

            border_window: hex(0xd5_d9dd),
            border_split: hex(0xdf_e2e6),
            border_header: hex(0xe3_e6e9),
            border_row: hex(0xee_f0f2),

            text_emphasis: hex(0x0e_1012),
            text_strong: hex(0x16_191c),
            text: hex(0x2b_2f33),
            text_secondary: hex(0x47_4c51),
            text_muted: hex(0x68_7077),
            text_faint: hex(0x89_9097),
            text_code: hex(0x30_353a),

            accent: hex(0x2f_74c0),
            // 4.5:1 or more on the selected and marked rows (the fill
            // shade measures 3.99:1 on the selected row).
            accent_text: hex(0x27_67ad),
            accent_hover: hex(0x1f_5c9e),
            accent_button_hover: hex(0x3d_82cf),
            on_accent: hex(0xff_ffff),
            on_accent_muted: hex(0xc9_dcf1),
            selection: hex(0x2f_74c0).opacity(0.22),
            accent_tint: hex(0x2f_74c0).opacity(0.07),
            accent_tint_strong: hex(0x2f_74c0).opacity(0.12),
            warning_tint: hex(0xb0_7408).opacity(0.08),
            critical_tint: hex(0xcf_4646).opacity(0.07),

            switch_thumb: hex(0xff_ffff),
            switch_thumb_on: hex(0xff_ffff),
            switch_thumb_shadow: hsla(0., 0., 0., 0.2),

            traffic_close: hex(0xff_5f57),
            traffic_minimize: hex(0xfe_bc2e),
            traffic_zoom: hex(0x28_c840),
            traffic_inactive: hex(0xc5_cacf),
            traffic_glyph: rgba(0x0000_008c).into(),

            shadow: hex(0x10_161d).opacity(0.10),
            shadow_strong: hex(0x10_161d).opacity(0.14),
            shadow_modal: hex(0x10_161d).opacity(0.20),
            backdrop: hex(0x1e_242a).opacity(0.28),
        }
    }
}

/// State colours in two shades. Hosts reuse the service palette: up = ok,
/// down = critical, unreachable = unknown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StateColors {
    /// For shapes: circles, dots, bars and squares.
    pub fill: StateShades,
    /// For words and numbers in a state's colour (`CRIT`, `late 3m`, a
    /// perfdata value over its threshold, an event's kind in the history).
    /// The same as the fill in dark; darker in light, for contrast on the
    /// light surfaces.
    pub text: StateShades,
}

impl StateColors {
    /// The dark theme's: one shade serves shapes and words.
    #[must_use]
    pub fn dark() -> Self {
        let fill = StateShades {
            ok: hex(0x56_b870),
            warning: hex(0xe5_b04a),
            critical: hex(0xe0_6c6c),
            unknown: hex(0xa9_7fdb),
            pending: hex(0x3a_3f43),
        };
        Self {
            fill,
            text: StateShades {
                // Nothing pending is a word in a colour; were it one, it
                // reads as muted text.
                pending: hex(0x8b_9094),
                ..fill
            },
        }
    }

    /// The light theme's: friendly fills (2.3–4:1 on white, an amber
    /// warning circle as in Icinga Web) and text shades of 5:1 or more.
    #[must_use]
    pub fn light() -> Self {
        Self {
            fill: StateShades {
                ok: hex(0x34_a058),
                warning: hex(0xe0_a020),
                critical: hex(0xe0_4848),
                unknown: hex(0x9a_68dc),
                pending: hex(0xc5_cacf),
            },
            text: StateShades {
                ok: hex(0x23_7a3f),
                warning: hex(0x9a_6200),
                critical: hex(0xc0_3535),
                unknown: hex(0x7a_4cbc),
                pending: hex(0x68_7077),
            },
        }
    }
}

/// One shade of every state colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StateShades {
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

impl StateShades {
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

    /// The colour for a host or service state.
    #[must_use]
    pub fn checkable(&self, state: CheckableState) -> Hsla {
        match state {
            CheckableState::Host(state) => self.host(state),
            CheckableState::Service(state) => self.service(state),
        }
    }

    /// The shade named as in [`StateShades::all`].
    #[cfg(test)]
    fn checkable_named(&self, name: &str) -> Hsla {
        self.all()
            .into_iter()
            .find(|(shade, _)| *shade == name)
            .map(|(_, color)| color)
            .unwrap_or_default()
    }

    /// Every shade with its name, for checks over the whole palette.
    #[must_use]
    pub fn all(&self) -> [(&'static str, Hsla); 5] {
        [
            ("ok", self.ok),
            ("warning", self.warning),
            ("critical", self.critical),
            ("unknown", self.unknown),
            ("pending", self.pending),
        ]
    }
}

/// The type scale. Names describe where each size is used in the design.
#[derive(Clone, Copy, Debug, PartialEq)]
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

impl TextSizes {
    /// Every size times `scale`.
    #[must_use]
    pub fn scaled(self, scale: f32) -> Self {
        Self {
            caption: self.caption * scale,
            hint: self.hint * scale,
            label: self.label * scale,
            small: self.small * scale,
            body: self.body * scale,
            row: self.row * scale,
            heading: self.heading * scale,
            title: self.title * scale,
        }
    }
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
///
/// Bar heights are content heights, as the design's CSS states them; bars
/// with a rule add it on top (the design renders with `content-box` sizing,
/// so its 40px header with a rule is 41px tall). Use [`Metrics::with_rule`].
///
/// The default is the design's sizes at 100 %; a theme scales them to the
/// interface size and sets the list rows' height for the row density
/// ([`Metrics::scaled`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Sidebar width.
    pub sidebar_width: Pixels,
    /// Detail pane width when split beside the list (the design's, at
    /// 1440px).
    pub pane_width: Pixels,
    /// The narrowest the split pane gets before it covers the list instead.
    pub pane_min_width: Pixels,
    /// The narrowest the list gets beside the split pane.
    pub list_min_width: Pixels,
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
    /// Horizontal padding of the sidebar's header, rows and footer.
    pub sidebar_padding: Pixels,
    /// Horizontal padding of the list header, summary bar and list rows.
    pub list_padding: Pixels,
    /// Horizontal padding of the detail pane's header.
    pub pane_padding: Pixels,
    /// Horizontal padding of the detail pane's body and its service rows.
    pub pane_inset: Pixels,
    /// Dashboard list rows: a 22px circle with its caption beside two lines
    /// of text, 10px above and below (add [`Metrics::RULE`] for the row
    /// rule). Compact rows ([`Density::Compact`]) are one 20px line, 6px
    /// above and below.
    pub row_height: Pixels,
    /// Width of the list rows' state circle column.
    pub row_leading: Pixels,
    /// Extra indent of the rows under a group header.
    pub row_indent: Pixels,
    /// Items in popup menus.
    pub menu_item_height: Pixels,
    /// Action buttons.
    pub button_height: Pixels,
    /// Action button corner radius.
    pub button_radius: Pixels,
    /// Square hit area of icon buttons.
    pub icon_button: Pixels,
    /// Corner radius of icon buttons and tooltips.
    pub small_radius: Pixels,
    /// Corner radius of code blocks and text fields.
    pub code_radius: Pixels,
    /// Corner radius of modal cards (the command palette, dialogs).
    pub modal_radius: Pixels,
    /// Text fields and other form controls.
    pub field_height: Pixels,
    /// Small icons (sidebar header and footer).
    pub icon_small: Pixels,
    /// Default icon size.
    pub icon: Pixels,
    /// Large icons (the footer's `+`).
    pub icon_large: Pixels,
    /// State circle in list rows.
    pub row_circle: Pixels,
    /// Ring width of a handled state circle in list rows.
    pub row_ring: Pixels,
    /// State circle in the host pane's service rows.
    pub compact_circle: Pixels,
    /// Ring width of a handled compact state circle.
    pub compact_ring: Pixels,
    /// State circle in the detail pane header.
    pub pane_circle: Pixels,
    /// Ring width of a handled pane state circle.
    pub pane_ring: Pixels,
    /// Dashboard state dot in the sidebar.
    pub sidebar_dot: Pixels,
    /// State dots in the summary bar.
    pub summary_dot: Pixels,
    /// Connection status dot in the sidebar footer.
    pub status_dot: Pixels,
    /// Window control circles (traffic lights).
    pub window_control: Pixels,
    /// Space between window control circles.
    pub window_control_gap: Pixels,
}

impl Metrics {
    /// Width of the rules under header bars and between rows: a hairline
    /// at every interface size.
    pub const RULE: Pixels = px(1.);

    /// A compact list row's content height at 100 %.
    pub const COMPACT_ROW: f32 = 32.;

    /// The outer height of a bar with content height `height` and a rule.
    #[must_use]
    pub fn with_rule(height: Pixels) -> Pixels {
        height + Self::RULE
    }

    /// These sizes times `scale`, with list rows `density` tall. Ring widths
    /// round to whole pixels, so handled circles stay crisp.
    #[must_use]
    pub fn scaled(self, scale: f32, density: Density) -> Self {
        let length = |value: Pixels| value * scale;
        let ring = |value: Pixels| px((f32::from(value) * scale).round().max(1.));
        let row_height = match density {
            Density::Comfortable => self.row_height,
            Density::Compact => px(Self::COMPACT_ROW),
        };
        Self {
            sidebar_width: length(self.sidebar_width),
            pane_width: length(self.pane_width),
            pane_min_width: length(self.pane_min_width),
            list_min_width: length(self.list_min_width),
            header_height: length(self.header_height),
            summary_bar_height: length(self.summary_bar_height),
            group_row_height: length(self.group_row_height),
            item_row_height: length(self.item_row_height),
            footer_height: length(self.footer_height),
            sidebar_padding: length(self.sidebar_padding),
            list_padding: length(self.list_padding),
            pane_padding: length(self.pane_padding),
            pane_inset: length(self.pane_inset),
            row_height: length(row_height),
            row_leading: length(self.row_leading),
            row_indent: length(self.row_indent),
            menu_item_height: length(self.menu_item_height),
            button_height: length(self.button_height),
            button_radius: length(self.button_radius),
            icon_button: length(self.icon_button),
            small_radius: length(self.small_radius),
            code_radius: length(self.code_radius),
            modal_radius: length(self.modal_radius),
            field_height: length(self.field_height),
            icon_small: length(self.icon_small),
            icon: length(self.icon),
            icon_large: length(self.icon_large),
            row_circle: length(self.row_circle),
            row_ring: ring(self.row_ring),
            compact_circle: length(self.compact_circle),
            compact_ring: ring(self.compact_ring),
            pane_circle: length(self.pane_circle),
            pane_ring: ring(self.pane_ring),
            sidebar_dot: length(self.sidebar_dot),
            summary_dot: length(self.summary_dot),
            status_dot: length(self.status_dot),
            window_control: length(self.window_control),
            window_control_gap: length(self.window_control_gap),
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            sidebar_width: px(300.),
            pane_width: px(620.),
            pane_min_width: px(420.),
            list_min_width: px(440.),
            header_height: px(40.),
            summary_bar_height: px(36.),
            group_row_height: px(36.),
            item_row_height: px(30.),
            footer_height: px(38.),
            sidebar_padding: px(12.),
            list_padding: px(18.),
            pane_padding: px(20.),
            pane_inset: px(24.),
            row_height: px(60.),
            row_leading: px(44.),
            row_indent: px(12.),
            menu_item_height: px(28.),
            button_height: px(28.),
            button_radius: px(5.),
            icon_button: px(22.),
            small_radius: px(4.),
            code_radius: px(6.),
            modal_radius: px(10.),
            field_height: px(30.),
            icon_small: px(13.),
            icon: px(14.),
            icon_large: px(16.),
            row_circle: px(22.),
            row_ring: px(3.),
            compact_circle: px(14.),
            compact_ring: px(2.),
            pane_circle: px(34.),
            pane_ring: px(4.),
            sidebar_dot: px(8.),
            summary_dot: px(9.),
            status_dot: px(6.),
            window_control: px(12.),
            window_control_gap: px(8.),
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

/// The WCAG contrast ratio of `foreground` over `background` (1 to 21),
/// with a translucent foreground blended over the background first.
#[must_use]
pub fn contrast_ratio(foreground: Hsla, background: Hsla) -> f32 {
    let background = Rgba::from(background);
    let foreground = Rgba::from(foreground);
    let blend = |front: f32, back: f32| front * foreground.a + back * (1. - foreground.a);
    let foreground = Rgba {
        r: blend(foreground.r, background.r),
        g: blend(foreground.g, background.g),
        b: blend(foreground.b, background.b),
        a: 1.,
    };
    let (lighter, darker) = {
        let (a, b) = (luminance(foreground), luminance(background));
        if a >= b { (a, b) } else { (b, a) }
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// WCAG relative luminance of an (opaque) sRGB colour.
fn luminance(color: Rgba) -> f32 {
    let channel = |value: f32| {
        if value <= 0.040_45 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both() -> [Theme; 2] {
        [Theme::dark(), Theme::light()]
    }

    #[test]
    fn host_states_reuse_the_service_palette() {
        for theme in both() {
            for states in [theme.states.fill, theme.states.text] {
                assert_eq!(states.host(HostState::Up), states.service(ServiceState::Ok));
                assert_eq!(
                    states.host(HostState::Down),
                    states.service(ServiceState::Critical)
                );
                assert_eq!(
                    states.host(HostState::Unreachable),
                    states.service(ServiceState::Unknown)
                );
                assert_eq!(
                    states.host(HostState::Pending),
                    states.service(ServiceState::Pending)
                );
            }
        }
    }

    #[test]
    fn checkable_states_use_their_own_palette() {
        let states = StateColors::dark().fill;
        assert_eq!(
            states.checkable(CheckableState::Service(ServiceState::Warning)),
            states.warning
        );
        assert_eq!(
            states.checkable(CheckableState::Host(HostState::Unreachable)),
            states.unknown
        );
    }

    #[test]
    fn tokens_match_the_design() {
        let colors = Colors::dark();
        assert_eq!(colors.window_background, hex(0x1d_2125));
        assert_eq!(colors.sidebar_background, colors.window_background);
        assert_eq!(colors.accent, hex(0x74_ade8));
        assert_eq!(colors.group_active, hex(0x30_353a));
        assert_eq!(colors.item_active, hex(0x27_2c30));
        let metrics = Metrics::default();
        assert_eq!(metrics.sidebar_width, px(300.));
        assert_eq!(metrics.group_row_height, px(36.));
        assert_eq!(metrics.item_row_height, px(30.));
        assert_eq!(metrics.footer_height, px(38.));
        assert_eq!(Metrics::with_rule(metrics.header_height), px(41.));

        // Topic 03 (design/v1/v1.css, `.light`).
        let light = Colors::light();
        assert_eq!(light.window_background, hex(0xff_ffff));
        assert_eq!(light.sidebar_background, hex(0xf4_f5f7));
        assert_eq!(light.pane_background, hex(0xf7_f8f9));
        assert_eq!(light.row_selected, hex(0xe3_ebf4));
        assert_eq!(light.accent, hex(0x2f_74c0));
        assert_eq!(light.text_muted, hex(0x68_7077));
        let states = StateColors::light();
        assert_eq!(states.fill.critical, hex(0xe0_4848));
        assert_eq!(states.text.critical, hex(0xc0_3535));
        assert_eq!(states.text.warning, hex(0x9a_6200));
    }

    #[test]
    fn dark_keeps_one_shade_per_state() {
        let states = StateColors::dark();
        for ((name, fill), (_, text)) in states.fill.all().into_iter().zip(states.text.all()) {
            if name != "pending" {
                assert_eq!(fill, text, "{name}: dark draws words in the fill");
            }
        }
    }

    #[test]
    fn both_themes_set_every_token() {
        let (dark, light) = (Colors::dark(), Colors::light());
        // Translucent on purpose: washes, selection, shadows, the backdrop.
        let translucent = [
            "selection",
            "accent_tint",
            "accent_tint_strong",
            "warning_tint",
            "critical_tint",
            "switch_thumb_shadow",
            "traffic_glyph",
            "shadow",
            "shadow_strong",
            "shadow_modal",
            "backdrop",
        ];
        // The same in both: the window controls keep their colours.
        let shared = [
            "traffic_close",
            "traffic_minimize",
            "traffic_zoom",
            "traffic_glyph",
        ];
        for ((name, dark), (_, light)) in dark.tokens().into_iter().zip(light.tokens()) {
            for color in [dark, light] {
                if name == "switch_thumb_shadow" {
                    assert!(color.a < 1., "{name} is a shadow (none in dark)");
                } else if translucent.contains(&name) {
                    assert!(color.a > 0. && color.a < 1., "{name} is translucent");
                } else {
                    assert!((color.a - 1.).abs() < f32::EPSILON, "{name} is opaque");
                }
            }
            if shared.contains(&name) {
                assert_eq!(dark, light, "{name}");
            } else {
                assert_ne!(dark, light, "light sets its own {name}");
            }
        }
        for (name, light) in StateColors::light().fill.all() {
            assert_ne!(light, StateColors::dark().fill.checkable_named(name));
        }
    }

    #[test]
    fn text_keeps_its_contrast_steps_in_both_themes() {
        for theme in both() {
            let colors = theme.colors;
            for surface in [colors.window_background, colors.pane_background] {
                assert!(contrast_ratio(colors.text, surface) >= 7.);
                assert!(contrast_ratio(colors.text_secondary, surface) >= 4.5);
                assert!(contrast_ratio(colors.text_muted, surface) >= 4.5);
                // Faint is for labels and hints: readable, deliberately quiet.
                assert!(contrast_ratio(colors.text_faint, surface) >= 3.);
            }
            assert!(contrast_ratio(colors.on_accent, colors.accent) >= 4.5);
        }
    }

    #[test]
    fn accent_words_read_on_selected_and_marked_rows() {
        for theme in both() {
            let colors = theme.colors;
            for surface in [
                colors.window_background,
                colors.pane_background,
                colors.sidebar_background,
                colors.code_background,
                colors.element_background,
                colors.row_selected,
                colors.row_marked,
                colors.row_hover,
                colors.group_active,
                colors.item_active,
            ] {
                assert!(
                    contrast_ratio(colors.accent_text, surface) >= 4.5,
                    "{:?} on {surface:?}",
                    colors.accent_text
                );
            }
        }
        // Dark keeps one shade; light's text shade is the darker one.
        let dark = Colors::dark();
        assert_eq!(dark.accent_text, dark.accent);
        let light = Colors::light();
        assert!(
            contrast_ratio(light.accent, light.row_selected) < 4.5,
            "the fill shade alone is too light for words on the selected row"
        );
    }

    #[test]
    fn light_state_words_read_on_every_surface() {
        let theme = Theme::light();
        let colors = theme.colors;
        let text = theme.states.text;
        let words = [text.ok, text.warning, text.critical, text.unknown];
        // 5:1 or more on the window, as topic 03 asks; 4.5:1 (WCAG AA) on
        // every surface words in a state colour sit on.
        for word in words {
            assert!(contrast_ratio(word, colors.window_background) >= 5.);
            for surface in [
                colors.pane_background,
                colors.sidebar_background,
                colors.code_background,
                colors.row_header,
                colors.row_hover,
            ] {
                assert!(
                    contrast_ratio(word, surface) >= 4.5,
                    "{word:?} on {surface:?}"
                );
            }
            // The selected and marked rows are tinted: the approved shades
            // keep 4:1 there (`late 3m` on the row under the cursor).
            for surface in [colors.row_selected, colors.row_marked] {
                assert!(
                    contrast_ratio(word, surface) >= 4.,
                    "{word:?} on {surface:?}"
                );
            }
        }
        // The fills are shapes, friendlier than the words but visible.
        for (name, fill) in theme.states.fill.all() {
            if name != "pending" {
                assert!(contrast_ratio(fill, colors.window_background) >= 2.);
                assert!(
                    contrast_ratio(fill, colors.window_background)
                        < contrast_ratio(text.checkable_named(name), colors.window_background),
                    "{name}: the text shade is the darker one"
                );
            }
        }
    }

    #[test]
    fn contrast_ratios_follow_wcag() {
        let (black, white) = (hex(0x00_0000), hex(0xff_ffff));
        assert!((contrast_ratio(black, white) - 21.).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.).abs() < 0.01);
        assert!(
            (contrast_ratio(white, black) - 21.).abs() < 0.01,
            "symmetric"
        );
        // A translucent colour counts as blended over the surface.
        assert!(contrast_ratio(black.opacity(0.), white) < 1.01);
    }

    #[test]
    fn hover_colours_sit_between_rest_and_active() {
        let between = |rest: Hsla, hover: Hsla, active: Hsla| {
            (rest.l < hover.l && hover.l < active.l) || (rest.l > hover.l && hover.l > active.l)
        };
        for theme in both() {
            let colors = theme.colors;
            assert!(between(
                colors.sidebar_background,
                colors.item_hover,
                colors.item_active
            ));
            assert!(between(
                colors.sidebar_background,
                colors.group_hover,
                colors.group_active
            ));
            assert!(between(
                colors.element_background,
                colors.element_hover,
                colors.element_active
            ));
            assert!(between(
                colors.window_background,
                colors.row_hover,
                colors.row_selected
            ));
        }
    }

    #[test]
    fn popovers_cast_stronger_shadows_than_tooltips() {
        for theme in both() {
            let colors = theme.colors;
            assert!(colors.shadow_strong.a > colors.shadow.a);
            assert!(colors.shadow_modal.a > colors.shadow_strong.a);
            assert!(colors.shadow.l < 0.15, "shadows are near black");
        }
        // Light floats softer.
        assert!(Colors::light().shadow_modal.a < Colors::dark().shadow_modal.a);
        assert!(Colors::light().backdrop.a < Colors::dark().backdrop.a);
    }

    #[test]
    fn list_rows_follow_the_design() {
        for theme in both() {
            // 10px padding around a 22px circle, 4px gap and a 14px caption.
            assert_eq!(Metrics::with_rule(theme.metrics.row_height), px(61.));
            assert_eq!(theme.metrics.row_leading, px(44.));
            assert_eq!(theme.line_height, relative(LINE_HEIGHT));
            // Group headers are a band off the window and the pane.
            assert_ne!(theme.colors.row_header, theme.colors.window_background);
            // Marked rows are tinted towards the accent.
            assert!(theme.colors.row_marked.h > 0.5 && theme.colors.row_marked.h < 0.65);
        }
        let dark = Theme::dark();
        assert!(dark.colors.row_header.l < dark.colors.window_background.l);
        assert!(dark.colors.row_header.l < dark.colors.pane_background.l);
    }

    #[test]
    fn compact_rows_are_one_line() {
        let theme = Theme::new(ThemeMode::Light, 1., Density::Compact);
        assert_eq!(theme.density, Density::Compact);
        assert_eq!(Metrics::with_rule(theme.metrics.row_height), px(33.));
        assert_eq!(theme.metrics.compact_circle, px(14.));
        assert_eq!(theme.colors, Colors::light(), "density changes no colour");
    }

    #[test]
    fn the_interface_size_scales_text_and_spacing() {
        let large = Theme::new(ThemeMode::Dark, 1.15, Density::Comfortable);
        let default = Theme::dark();
        assert!((large.scale - 1.15).abs() < f32::EPSILON);
        assert_eq!(large.text.row, default.text.row * 1.15);
        assert_eq!(large.metrics.sidebar_width, px(345.));
        assert_eq!(large.metrics.row_height, px(69.));
        // Rules stay hairlines and rings whole pixels.
        assert_eq!(Metrics::with_rule(large.metrics.row_height), px(70.));
        assert_eq!(large.metrics.row_ring, px(3.));
        let small = Theme::new(ThemeMode::Dark, 0.9, Density::Compact);
        assert_eq!(small.text.title, px(18. * 0.9));
        assert_eq!(small.metrics.row_height, px(32. * 0.9));
        assert_eq!(small.metrics.compact_ring, px(2.));
        // Nonsense stays sensible.
        assert!((Theme::new(ThemeMode::Dark, f32::NAN, Density::Compact).scale - 1.).abs() < 1e-6);
    }

    #[test]
    fn perfdata_values_are_coloured_by_threshold() {
        for theme in both() {
            assert_eq!(theme.perfdata_color(PerfdataStatus::Ok), theme.colors.text);
            assert_eq!(
                theme.perfdata_color(PerfdataStatus::Warning),
                theme.states.text.warning
            );
            assert_eq!(
                theme.perfdata_color(PerfdataStatus::Critical),
                theme.states.text.critical
            );
        }
    }
}
