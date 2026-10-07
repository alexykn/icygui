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
    /// `arrow-left`: back to the previous object in a pane.
    ArrowLeft,
    /// `arrow-left-right`: switch to another environment.
    ArrowLeftRight,
    /// `arrow-up`: ascending sort.
    ArrowUp,
    /// `arrow-up-right`: "open as tab".
    ArrowUpRight,
    /// `bell`: notifications on.
    Bell,
    /// `bell-off`: notifications muted.
    BellOff,
    /// `calendar-clock`: a downtime (the pane's banner, the host line's
    /// marker).
    CalendarClock,
    /// `check`: confirmations and checked menu items.
    Check,
    /// `check-check`: mark notifications read.
    CheckCheck,
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
    /// `file-code`: open a settings file in the editor.
    FileCode,
    /// `file-input`: import from a file.
    FileInput,
    /// `file-output`: export to a file.
    FileOutput,
    /// `folder`: host group and service group headers in lists.
    Folder,
    /// `folder-open`: open a folder in the file manager.
    FolderOpen,
    /// `folder-plus`: a new group.
    FolderPlus,
    /// `info`: informational notes.
    Info,
    /// `key-round`: a login Icinga refused, a password that is missing.
    KeyRound,
    /// `keyboard`: the keymap.
    Keyboard,
    /// `layers`: several objects at once (the palette's *all N matches*).
    Layers,
    /// `loader-circle`: loading.
    Loader,
    /// `lock`: a server certificate that isn't trusted.
    Lock,
    /// `maximize-2`: the window maximise control.
    Maximize,
    /// A speech bubble: comments.
    MessageSquare,
    /// `minus`: the window minimise control.
    Minus,
    /// `panel-left`: show or hide the sidebar.
    PanelLeft,
    /// `pause`: pause notifications.
    Pause,
    /// `pencil`: edit a dashboard.
    Pencil,
    /// `plus`: add a dashboard or an environment.
    Plus,
    /// `power`: quit.
    Power,
    /// `refresh-cw`: check now.
    Refresh,
    /// `search`: search fields.
    Search,
    /// `server`: Icinga and its environments.
    Server,
    /// `settings`: settings (an environment's, the notifications', the app's).
    Settings,
    /// `sun-moon`: appearance (theme and size).
    SunMoon,
    /// `triangle-alert`: errors such as a dashboard filter that fails.
    TriangleAlert,
    /// `unplug`: the connection to Icinga is lost.
    Unplug,
    /// `wrench`: advanced settings.
    Wrench,
}

impl IconName {
    /// Every icon, for tests and asset listings.
    pub const ALL: [Self; 45] = [
        Self::ArrowDown,
        Self::ArrowLeft,
        Self::ArrowLeftRight,
        Self::ArrowUp,
        Self::ArrowUpRight,
        Self::Bell,
        Self::BellOff,
        Self::CalendarClock,
        Self::Check,
        Self::CheckCheck,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::Clock,
        Self::Close,
        Self::Copy,
        Self::Ellipsis,
        Self::ExternalLink,
        Self::FileCode,
        Self::FileInput,
        Self::FileOutput,
        Self::Folder,
        Self::FolderOpen,
        Self::FolderPlus,
        Self::Info,
        Self::KeyRound,
        Self::Keyboard,
        Self::Layers,
        Self::Loader,
        Self::Lock,
        Self::Maximize,
        Self::MessageSquare,
        Self::Minus,
        Self::PanelLeft,
        Self::Pause,
        Self::Pencil,
        Self::Plus,
        Self::Power,
        Self::Refresh,
        Self::Search,
        Self::Server,
        Self::Settings,
        Self::SunMoon,
        Self::TriangleAlert,
        Self::Unplug,
        Self::Wrench,
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
            IconName::ArrowLeft => Self::ArrowLeft,
            IconName::ArrowLeftRight => Self::ArrowLeftRight,
            IconName::ArrowUp => Self::ArrowUp,
            IconName::ArrowUpRight => Self::ArrowUpRight,
            IconName::Bell => Self::Bell,
            IconName::BellOff => Self::BellOff,
            IconName::CalendarClock => Self::CalendarClock,
            IconName::Check => Self::Check,
            IconName::CheckCheck => Self::CheckCheck,
            IconName::ChevronDown => Self::ChevronDown,
            IconName::ChevronRight => Self::ChevronRight,
            IconName::Clock => Self::Clock,
            IconName::Close => Self::X,
            IconName::Copy => Self::Copy,
            IconName::Ellipsis => Self::Ellipsis,
            IconName::ExternalLink => Self::ExternalLink,
            IconName::FileCode => Self::FileCode,
            IconName::FileInput => Self::FileInput,
            IconName::FileOutput => Self::FileOutput,
            IconName::Folder => Self::Folder,
            IconName::FolderOpen => Self::FolderOpen,
            IconName::FolderPlus => Self::FolderPlus,
            IconName::Info => Self::Info,
            IconName::KeyRound => Self::KeyRound,
            IconName::Keyboard => Self::Keyboard,
            IconName::Layers => Self::Layers,
            IconName::Loader => Self::LoaderCircle,
            IconName::Lock => Self::Lock,
            IconName::Maximize => Self::Maximize2,
            IconName::MessageSquare => Self::MessageSquare,
            IconName::Minus => Self::Minus,
            IconName::PanelLeft => Self::PanelLeft,
            IconName::Pause => Self::Pause,
            IconName::Pencil => Self::Pencil,
            IconName::Plus => Self::Plus,
            IconName::Power => Self::Power,
            IconName::Refresh => Self::RefreshCw,
            IconName::Search => Self::Search,
            IconName::Server => Self::Server,
            IconName::Settings => Self::Settings,
            IconName::SunMoon => Self::SunMoon,
            IconName::TriangleAlert => Self::TriangleAlert,
            IconName::Unplug => Self::Unplug,
            IconName::Wrench => Self::Wrench,
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
