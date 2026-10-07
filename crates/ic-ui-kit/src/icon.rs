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
    /// `list`: a list view (topic 04's view header).
    List,
    /// `rows-3`: a grouped-list view.
    Rows,
    /// `layout-grid`: a host-group grid view.
    LayoutGrid,
    /// `chart-bar`: a summary tiles view.
    ChartBar,
    /// `activity`: an event stream view.
    Activity,
    /// `grip-vertical`: a drag handle (the dashboard editor's views).
    GripVertical,
    /// `users`: the handling view (who is handling what, topic 14).
    Users,
    /// `rows-2`: the row-density toggle: comfortable rows (topic 14).
    Rows2,
    /// `rows-4`: the row-density toggle: compact rows (topic 14).
    Rows4,
    /// `trash`: a sidebar mark.
    Trash,
    /// `sun`: a sidebar mark.
    Sun,
    /// `moon`: a sidebar mark.
    Moon,
    /// `monitor`: a sidebar mark.
    Monitor,
    /// `layout-list`: a sidebar mark.
    LayoutList,
    /// `network`: a sidebar mark.
    Network,
    /// `user`: a sidebar mark.
    User,
    /// `mail`: a sidebar mark.
    Mail,
    /// `phone`: a sidebar mark.
    Phone,
    /// `terminal`: a sidebar mark.
    Terminal,
    /// `square-terminal`: a sidebar mark.
    SquareTerminal,
    /// `pin`: a sidebar mark.
    Pin,
    /// `save`: a sidebar mark.
    Save,
    /// `layout-dashboard`: a sidebar mark.
    LayoutDashboard,
    /// `eye`: a sidebar mark.
    Eye,
    /// `gauge`: a sidebar mark.
    Gauge,
    /// `file-text`: a sidebar mark.
    FileText,
    /// `scroll-text`: a sidebar mark.
    ScrollText,
    /// `list-filter`: a sidebar mark.
    ListFilter,
    /// `heart-pulse`: cluster health (topic 06); a sidebar mark.
    HeartPulse,
    /// `plug`: a sidebar mark.
    Plug,
    /// `timer`: a sidebar mark.
    Timer,
    /// `hourglass`: a sidebar mark.
    Hourglass,
    /// `calendar`: a sidebar mark.
    Calendar,
    /// `command`: a sidebar mark.
    Command,
    /// `sliders-horizontal`: a sidebar mark.
    SlidersHorizontal,
    /// `palette`: a sidebar mark.
    Palette,
    /// `bug`: a sidebar mark.
    Bug,
    /// `braces`: a sidebar mark.
    Braces,
    /// `brackets`: a sidebar mark.
    Brackets,
    /// `type`: a sidebar mark.
    Type,
    /// `hash`: a sidebar mark.
    Hash,
    /// `toggle-left`: a sidebar mark.
    ToggleLeft,
    /// `square-function`: a sidebar mark.
    SquareFunction,
    /// `variable`: a sidebar mark.
    Variable,
    /// `quote`: a sidebar mark.
    Quote,
    /// `parentheses`: a sidebar mark.
    Parentheses,
    /// `binary`: a sidebar mark.
    Binary,
    /// `box`: a sidebar mark.
    Box,
    /// `tag`: a sidebar mark.
    Tag,
    /// `sigma`: a sidebar mark.
    Sigma,
    /// `pin-off`: a sidebar mark.
    PinOff,
    /// `bookmark-plus`: a sidebar mark.
    BookmarkPlus,
    /// `users-round`: a sidebar mark.
    UsersRound,
    /// `circle-user`: a sidebar mark.
    CircleUser,
    /// `user-check`: a sidebar mark.
    UserCheck,
    /// `bell-ring`: a sidebar mark.
    BellRing,
    /// `at-sign`: a sidebar mark.
    AtSign,
    /// `database`: a sidebar mark.
    Database,
}

impl IconName {
    /// Every icon, for tests and asset listings.
    pub const ALL: [Self; 103] = [
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
        Self::List,
        Self::Rows,
        Self::LayoutGrid,
        Self::ChartBar,
        Self::Activity,
        Self::GripVertical,
        Self::Users,
        Self::Rows2,
        Self::Rows4,
        Self::Trash,
        Self::Sun,
        Self::Moon,
        Self::Monitor,
        Self::LayoutList,
        Self::Network,
        Self::User,
        Self::Mail,
        Self::Phone,
        Self::Terminal,
        Self::SquareTerminal,
        Self::Pin,
        Self::Save,
        Self::LayoutDashboard,
        Self::Eye,
        Self::Gauge,
        Self::FileText,
        Self::ScrollText,
        Self::ListFilter,
        Self::HeartPulse,
        Self::Plug,
        Self::Timer,
        Self::Hourglass,
        Self::Calendar,
        Self::Command,
        Self::SlidersHorizontal,
        Self::Palette,
        Self::Bug,
        Self::Braces,
        Self::Brackets,
        Self::Type,
        Self::Hash,
        Self::ToggleLeft,
        Self::SquareFunction,
        Self::Variable,
        Self::Quote,
        Self::Parentheses,
        Self::Binary,
        Self::Box,
        Self::Tag,
        Self::Sigma,
        Self::PinOff,
        Self::BookmarkPlus,
        Self::UsersRound,
        Self::CircleUser,
        Self::UserCheck,
        Self::BellRing,
        Self::AtSign,
        Self::Database,
    ];

    /// The asset path [`crate::Assets`] serves the SVG under.
    #[must_use]
    pub fn path(self) -> SharedString {
        gpui_kit_assets::IconName::from(self).path()
    }

    /// The icon's Lucide name (`calendar-clock`): how the settings file
    /// names a dashboard's chosen sidebar mark.
    #[must_use]
    #[expect(clippy::too_many_lines, reason = "one arm per icon")]
    pub fn lucide_name(self) -> &'static str {
        match self {
            Self::ArrowDown => "arrow-down",
            Self::ArrowLeft => "arrow-left",
            Self::ArrowLeftRight => "arrow-left-right",
            Self::ArrowUp => "arrow-up",
            Self::ArrowUpRight => "arrow-up-right",
            Self::Bell => "bell",
            Self::BellOff => "bell-off",
            Self::CalendarClock => "calendar-clock",
            Self::Check => "check",
            Self::CheckCheck => "check-check",
            Self::ChevronDown => "chevron-down",
            Self::ChevronRight => "chevron-right",
            Self::Clock => "clock",
            Self::Close => "x",
            Self::Copy => "copy",
            Self::Ellipsis => "ellipsis",
            Self::ExternalLink => "external-link",
            Self::FileCode => "file-code",
            Self::FileInput => "file-input",
            Self::FileOutput => "file-output",
            Self::Folder => "folder",
            Self::FolderOpen => "folder-open",
            Self::FolderPlus => "folder-plus",
            Self::Info => "info",
            Self::KeyRound => "key-round",
            Self::Keyboard => "keyboard",
            Self::Layers => "layers",
            Self::Loader => "loader-circle",
            Self::Lock => "lock",
            Self::Maximize => "maximize-2",
            Self::MessageSquare => "message-square",
            Self::Minus => "minus",
            Self::PanelLeft => "panel-left",
            Self::Pause => "pause",
            Self::Pencil => "pencil",
            Self::Plus => "plus",
            Self::Power => "power",
            Self::Refresh => "refresh-cw",
            Self::Search => "search",
            Self::Server => "server",
            Self::Settings => "settings",
            Self::SunMoon => "sun-moon",
            Self::TriangleAlert => "triangle-alert",
            Self::Unplug => "unplug",
            Self::Wrench => "wrench",
            Self::List => "list",
            Self::Rows => "rows-3",
            Self::LayoutGrid => "layout-grid",
            Self::ChartBar => "chart-bar",
            Self::Activity => "activity",
            Self::GripVertical => "grip-vertical",
            Self::Users => "users",
            Self::Rows2 => "rows-2",
            Self::Rows4 => "rows-4",
            Self::Trash => "trash",
            Self::Sun => "sun",
            Self::Moon => "moon",
            Self::Monitor => "monitor",
            Self::LayoutList => "layout-list",
            Self::Network => "network",
            Self::User => "user",
            Self::Mail => "mail",
            Self::Phone => "phone",
            Self::Terminal => "terminal",
            Self::SquareTerminal => "square-terminal",
            Self::Pin => "pin",
            Self::Save => "save",
            Self::LayoutDashboard => "layout-dashboard",
            Self::Eye => "eye",
            Self::Gauge => "gauge",
            Self::FileText => "file-text",
            Self::ScrollText => "scroll-text",
            Self::ListFilter => "list-filter",
            Self::HeartPulse => "heart-pulse",
            Self::Plug => "plug",
            Self::Timer => "timer",
            Self::Hourglass => "hourglass",
            Self::Calendar => "calendar",
            Self::Command => "command",
            Self::SlidersHorizontal => "sliders-horizontal",
            Self::Palette => "palette",
            Self::Bug => "bug",
            Self::Braces => "braces",
            Self::Brackets => "brackets",
            Self::Type => "type",
            Self::Hash => "hash",
            Self::ToggleLeft => "toggle-left",
            Self::SquareFunction => "square-function",
            Self::Variable => "variable",
            Self::Quote => "quote",
            Self::Parentheses => "parentheses",
            Self::Binary => "binary",
            Self::Box => "box",
            Self::Tag => "tag",
            Self::Sigma => "sigma",
            Self::PinOff => "pin-off",
            Self::BookmarkPlus => "bookmark-plus",
            Self::UsersRound => "users-round",
            Self::CircleUser => "circle-user",
            Self::UserCheck => "user-check",
            Self::BellRing => "bell-ring",
            Self::AtSign => "at-sign",
            Self::Database => "database",
        }
    }

    /// Whether the icon can be a dashboard's sidebar mark: content icons
    /// only, no chevrons, arrows, close, add, menus or handles (topic 14,
    /// round 5).
    #[must_use]
    pub fn is_pickable(self) -> bool {
        !matches!(
            self,
            Self::ArrowDown
                | Self::ArrowLeft
                | Self::ArrowLeftRight
                | Self::ArrowUp
                | Self::ArrowUpRight
                | Self::Check
                | Self::CheckCheck
                | Self::ChevronDown
                | Self::ChevronRight
                | Self::Close
                | Self::Copy
                | Self::Ellipsis
                | Self::ExternalLink
                | Self::FolderPlus
                | Self::Loader
                | Self::Maximize
                | Self::Minus
                | Self::PanelLeft
                | Self::Plus
                | Self::Refresh
                | Self::GripVertical
                | Self::Rows2
                | Self::Rows4
                | Self::Trash
                | Self::PinOff
                | Self::BookmarkPlus
                | Self::Pencil
                | Self::Save
                | Self::Search
        )
    }

    /// The icon with this Lucide name, if the app has it.
    #[must_use]
    pub fn from_lucide_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|icon| icon.lucide_name() == name)
    }
}

impl From<IconName> for gpui_kit_assets::IconName {
    #[expect(clippy::too_many_lines, reason = "one arm per icon")]
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
            IconName::List => Self::List,
            IconName::Rows => Self::Rows3,
            IconName::LayoutGrid => Self::LayoutGrid,
            IconName::ChartBar => Self::ChartBar,
            IconName::Activity => Self::Activity,
            IconName::GripVertical => Self::GripVertical,
            IconName::Users => Self::Users,
            IconName::Rows2 => Self::Rows2,
            IconName::Rows4 => Self::Rows4,
            IconName::Trash => Self::Trash,
            IconName::Sun => Self::Sun,
            IconName::Moon => Self::Moon,
            IconName::Monitor => Self::Monitor,
            IconName::LayoutList => Self::LayoutList,
            IconName::Network => Self::Network,
            IconName::User => Self::User,
            IconName::Mail => Self::Mail,
            IconName::Phone => Self::Phone,
            IconName::Terminal => Self::Terminal,
            IconName::SquareTerminal => Self::SquareTerminal,
            IconName::Pin => Self::Pin,
            IconName::Save => Self::Save,
            IconName::LayoutDashboard => Self::LayoutDashboard,
            IconName::Eye => Self::Eye,
            IconName::Gauge => Self::Gauge,
            IconName::FileText => Self::FileText,
            IconName::ScrollText => Self::ScrollText,
            IconName::ListFilter => Self::ListFilter,
            IconName::HeartPulse => Self::HeartPulse,
            IconName::Plug => Self::Plug,
            IconName::Timer => Self::Timer,
            IconName::Hourglass => Self::Hourglass,
            IconName::Calendar => Self::Calendar,
            IconName::Command => Self::Command,
            IconName::SlidersHorizontal => Self::SlidersHorizontal,
            IconName::Palette => Self::Palette,
            IconName::Bug => Self::Bug,
            IconName::Braces => Self::Braces,
            IconName::Brackets => Self::Brackets,
            IconName::Type => Self::Type,
            IconName::Hash => Self::Hash,
            IconName::ToggleLeft => Self::ToggleLeft,
            IconName::SquareFunction => Self::SquareFunction,
            IconName::Variable => Self::Variable,
            IconName::Quote => Self::Quote,
            IconName::Parentheses => Self::Parentheses,
            IconName::Binary => Self::Binary,
            IconName::Box => Self::Box,
            IconName::Tag => Self::Tag,
            IconName::Sigma => Self::Sigma,
            IconName::PinOff => Self::PinOff,
            IconName::BookmarkPlus => Self::BookmarkPlus,
            IconName::UsersRound => Self::UsersRound,
            IconName::CircleUser => Self::CircleUser,
            IconName::UserCheck => Self::UserCheck,
            IconName::BellRing => Self::BellRing,
            IconName::AtSign => Self::AtSign,
            IconName::Database => Self::Database,
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
    fn lucide_names_name_the_svgs() {
        for icon in IconName::ALL {
            let name = icon.lucide_name();
            assert_eq!(
                icon.path().as_ref(),
                format!("icons/{name}.svg"),
                "{icon:?}"
            );
            assert_eq!(IconName::from_lucide_name(name), Some(icon));
        }
        assert_eq!(IconName::from_lucide_name("no-such-icon"), None);
    }

    #[test]
    fn marks_are_content_icons() {
        assert!(IconName::Server.is_pickable());
        assert!(IconName::Phone.is_pickable());
        assert!(!IconName::ChevronDown.is_pickable());
        assert!(!IconName::Plus.is_pickable());
        assert!(!IconName::GripVertical.is_pickable());
        let pickable = IconName::ALL
            .iter()
            .filter(|icon| icon.is_pickable())
            .count();
        assert!(pickable > 60, "{pickable}");
    }

    #[test]
    fn names_map_to_the_expected_lucide_icons() {
        assert_eq!(IconName::Close.path().as_ref(), "icons/x.svg");
        assert_eq!(IconName::Maximize.path().as_ref(), "icons/maximize-2.svg");
        assert_eq!(IconName::PanelLeft.path().as_ref(), "icons/panel-left.svg");
        assert_eq!(IconName::Refresh.path().as_ref(), "icons/refresh-cw.svg");
    }
}
