//! Icons: a curated subset of the Lucide set bundled with gpui-component,
//! embedded in the binary by [`crate::Assets`].
//!
//! Views can only name icons listed in [`IconName`], so every icon they use is
//! guaranteed to be embedded (a test loads each one).

use gpui::{App, Hsla, IntoElement, Pixels, RenderOnce, SharedString, Styled as _, Window, svg};

use crate::theme::ActiveTheme as _;

/// The icons the app uses. Each maps to a Lucide SVG.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum IconName {
    /// `arrow-down`: descending sort.
    ArrowDown,
    /// `arrow-up`: ascending sort.
    ArrowUp,
    /// `arrow-up-right`: "open as tab".
    ArrowUpRight,
    /// `bell`: notifications on.
    Bell,
    /// `bell-off`: notifications muted.
    BellOff,
    /// `check`: confirmations and checked menu items.
    Check,
    /// `chevron-down`: an expanded group.
    ChevronDown,
    /// `chevron-right`: a collapsed group.
    ChevronRight,
    /// `clock`: the notification centre.
    Clock,
    /// `x`: close a pane, the window close control.
    Close,
    /// `copy`: copy to the clipboard.
    Copy,
    /// `ellipsis`: a `···` menu.
    Ellipsis,
    /// `external-link`: notes and action URLs.
    ExternalLink,
    /// `maximize-2`: the window maximise control.
    Maximize,
    /// `minus`: the window minimise control.
    Minus,
    /// `panel-left`: show or hide the sidebar.
    PanelLeft,
    /// `plus`: add a dashboard or an environment.
    Plus,
    /// `refresh-cw`: check now.
    Refresh,
    /// `search`: search fields.
    Search,
}

impl IconName {
    /// Every icon, for tests and asset listings.
    pub const ALL: [Self; 19] = [
        Self::ArrowDown,
        Self::ArrowUp,
        Self::ArrowUpRight,
        Self::Bell,
        Self::BellOff,
        Self::Check,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::Clock,
        Self::Close,
        Self::Copy,
        Self::Ellipsis,
        Self::ExternalLink,
        Self::Maximize,
        Self::Minus,
        Self::PanelLeft,
        Self::Plus,
        Self::Refresh,
        Self::Search,
    ];

    /// The asset path [`crate::Assets`] serves the SVG under.
    #[must_use]
    pub fn path(self) -> SharedString {
        gpui_kit_assets::IconName::from(self).path()
    }
}

impl From<IconName> for gpui_kit_assets::IconName {
    fn from(name: IconName) -> Self {
        match name {
            IconName::ArrowDown => Self::ArrowDown,
            IconName::ArrowUp => Self::ArrowUp,
            IconName::ArrowUpRight => Self::ArrowUpRight,
            IconName::Bell => Self::Bell,
            IconName::BellOff => Self::BellOff,
            IconName::Check => Self::Check,
            IconName::ChevronDown => Self::ChevronDown,
            IconName::ChevronRight => Self::ChevronRight,
            IconName::Clock => Self::Clock,
            IconName::Close => Self::X,
            IconName::Copy => Self::Copy,
            IconName::Ellipsis => Self::Ellipsis,
            IconName::ExternalLink => Self::ExternalLink,
            IconName::Maximize => Self::Maximize2,
            IconName::Minus => Self::Minus,
            IconName::PanelLeft => Self::PanelLeft,
            IconName::Plus => Self::Plus,
            IconName::Refresh => Self::RefreshCw,
            IconName::Search => Self::Search,
        }
    }
}

/// An icon, drawn in a single colour.
///
/// Size defaults to [`crate::Metrics::icon`]; colour defaults to the
/// surrounding text colour.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "an icon does nothing unless rendered"]
pub struct Icon {
    name: IconName,
    size: Option<Pixels>,
    color: Option<Hsla>,
}

impl Icon {
    /// An icon at the default size, in the surrounding text colour.
    pub fn new(name: IconName) -> Self {
        Self {
            name,
            size: None,
            color: None,
        }
    }

    /// Sets the width and height.
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }

    /// Sets the colour.
    pub fn color(mut self, color: impl Into<Hsla>) -> Self {
        self.color = Some(color.into());
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let size = self.size.unwrap_or(cx.theme().metrics.icon);
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        svg()
            .path(self.name.path())
            .flex_none()
            .size(size)
            .text_color(color)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_icon_has_its_own_svg_path() {
        let paths: HashSet<_> = IconName::ALL.iter().map(|icon| icon.path()).collect();
        assert_eq!(paths.len(), IconName::ALL.len());
        for icon in IconName::ALL {
            let path = icon.path();
            assert!(
                path.starts_with("icons/") && path.ends_with(".svg"),
                "{icon:?}: {path}"
            );
        }
    }

    #[test]
    fn names_map_to_the_expected_lucide_icons() {
        assert_eq!(IconName::Close.path().as_ref(), "icons/x.svg");
        assert_eq!(IconName::Maximize.path().as_ref(), "icons/maximize-2.svg");
        assert_eq!(IconName::PanelLeft.path().as_ref(), "icons/panel-left.svg");
        assert_eq!(IconName::Refresh.path().as_ref(), "icons/refresh-cw.svg");
    }
}
