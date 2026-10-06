//! The application's asset source: the icons in [`IconName`](crate::IconName)
//! plus the default icon bundle gpui-component's own controls use (the input's
//! clear button, spinners, menus).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

// Embeds exactly the Lucide SVGs behind `IconName`; keep in sync with it (a
// test checks every icon loads).
gpui_kit_assets::icon_assets!(
    AppIcons,
    [
        ArrowDown,
        ArrowLeft,
        ArrowUp,
        ArrowUpRight,
        Bell,
        BellOff,
        Check,
        ChevronDown,
        ChevronRight,
        Clock,
        X,
        Copy,
        Ellipsis,
        ExternalLink,
        Folder,
        Info,
        KeyRound,
        LoaderCircle,
        Lock,
        Maximize2,
        Minus,
        PanelLeft,
        Plus,
        RefreshCw,
        Search,
        TriangleAlert,
        Unplug,
    ]
);

/// Serves the app's icons and gpui-component's default icons. Pass it to
/// `Application::with_assets` before opening windows.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        match AppIcons.load(path)? {
            Some(data) => Ok(Some(data)),
            None => gpui_kit_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut paths = AppIcons.list(path)?;
        paths.extend(gpui_kit_assets::Assets.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IconName;

    #[test]
    fn every_app_icon_is_embedded() {
        for icon in IconName::ALL {
            let data = Assets
                .load(&icon.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{icon:?} is not embedded"));
            let svg = std::str::from_utf8(&data).unwrap();
            assert!(svg.contains("<svg"), "{icon:?} is not an SVG");
        }
    }

    #[test]
    fn component_icons_are_still_served() {
        // gpui-component's input draws its clear button with this icon.
        let data = Assets.load("icons/circle-x.svg").unwrap();
        assert!(data.is_some());
    }

    #[test]
    fn unknown_paths_are_errors_or_absent() {
        assert!(!matches!(
            Assets.load("icons/no-such-icon.svg"),
            Ok(Some(_))
        ));
    }

    #[test]
    fn listing_merges_both_sources_without_duplicates() {
        let paths = Assets.list("icons/").unwrap();
        assert!(paths.iter().any(|path| path.as_ref() == "icons/clock.svg"));
        assert!(
            paths
                .iter()
                .any(|path| path.as_ref() == "icons/circle-x.svg")
        );
        let mut deduped = paths.clone();
        deduped.dedup();
        assert_eq!(deduped.len(), paths.len());
    }
}
