//! The cluster health page as a built-in dashboard of every environment
//! (PLAN.md §4.2 H; mock-ups 16j–16l): its views are the health kinds
//! ([`ViewDisplay::HEALTH`]), edited in the same dashboard editor as every
//! dashboard, restricted to those kinds. The alert block and the heartbeat
//! row stay pinned at its top and are no views (trouble must stay
//! visible).
//!
//! Every environment has the approved layout of topic 06 without anyone
//! opening the editor: [`HealthPage::default`], which the settings file
//! leaves out until the page is edited, so files from before the page
//! existed load with it (and with the next version's defaults while it is
//! unedited). *Reset to default* in the editor brings it back.

use serde::{Deserialize, Serialize};

use crate::view::{View, ViewDisplay};

/// The cluster health page's views, top to bottom.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthPage {
    /// One view per health kind at most, each with a fixed id (its kind's,
    /// [`HealthPage::id_of`]); a switched-off view stays in the list.
    pub views: Vec<View>,
}

impl Default for HealthPage {
    /// Topic 06's layout: zones and endpoints, checks, queues and
    /// connections, Icinga's global switches, each with every tile and its
    /// trend lines (the `IcingaDB` tile shows only while Icinga reports
    /// the feature enabled).
    fn default() -> Self {
        Self {
            views: ViewDisplay::HEALTH.into_iter().map(Self::view_of).collect(),
        }
    }
}

impl HealthPage {
    /// The page's title (built in: it can't be renamed).
    pub const TITLE: &'static str = "cluster health";

    /// Whether the page is the default layout (the settings file leaves it
    /// out then).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The id of the page's view of `display`.
    #[must_use]
    pub fn id_of(display: ViewDisplay) -> &'static str {
        match display {
            ViewDisplay::ZonesAndEndpoints => "zones",
            ViewDisplay::Checks => "checks",
            ViewDisplay::QueuesAndConnections => "queues",
            ViewDisplay::GlobalSwitches => "switches",
            _ => "",
        }
    }

    /// A new view of `display`, as the default layout and *add view* make
    /// it.
    #[must_use]
    pub fn view_of(display: ViewDisplay) -> View {
        View {
            id: Self::id_of(display).to_owned(),
            display,
            filter: String::new(),
            problems_only: false,
            ..View::default()
        }
    }

    /// The views that show (not switched off), in order.
    pub fn shown(&self) -> impl Iterator<Item = &View> {
        self.views.iter().filter(|view| !view.health.off)
    }

    /// Makes a page read from a file (or edited by hand) one the app can
    /// show: views of other kinds are dropped (they belong to sidebar
    /// dashboards), a kind listed twice keeps its first view, and every
    /// view gets its kind's id. Returns how many things changed.
    pub fn repair(&mut self) -> usize {
        let mut changed = 0;
        let mut seen: Vec<ViewDisplay> = Vec::new();
        self.views.retain(|view| {
            let keep = view.display.is_health() && !seen.contains(&view.display);
            if keep {
                seen.push(view.display);
            } else {
                changed += 1;
            }
            keep
        });
        for view in &mut self.views {
            let id = Self::id_of(view.display);
            if view.id != id {
                id.clone_into(&mut view.id);
                changed += 1;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{HealthOptions, HealthTile};

    #[test]
    fn the_default_is_the_approved_layout() {
        let page = HealthPage::default();
        let kinds: Vec<ViewDisplay> = page.views.iter().map(|view| view.display).collect();
        assert_eq!(kinds, ViewDisplay::HEALTH);
        let ids: Vec<&str> = page.views.iter().map(|view| view.id.as_str()).collect();
        assert_eq!(ids, ["zones", "checks", "queues", "switches"]);
        for view in &page.views {
            assert_eq!(
                view.health,
                HealthOptions::default(),
                "every tile, with trends"
            );
            assert!(view.filter.is_empty());
        }
        assert_eq!(page.shown().count(), 4);
        assert!(page.is_default());
    }

    #[test]
    fn a_hand_edited_page_is_repaired() {
        let mut page = HealthPage {
            views: vec![
                View::default(),
                View {
                    id: "mine".to_owned(),
                    ..HealthPage::view_of(ViewDisplay::Checks)
                },
                HealthPage::view_of(ViewDisplay::GlobalSwitches),
                HealthPage::view_of(ViewDisplay::Checks),
            ],
        };
        assert_eq!(page.repair(), 3, "a list view, a second checks view, an id");
        let kinds: Vec<ViewDisplay> = page.views.iter().map(|view| view.display).collect();
        assert_eq!(kinds, [ViewDisplay::Checks, ViewDisplay::GlobalSwitches]);
        assert_eq!(page.views[0].id, "checks");
        assert_eq!(page.repair(), 0);
    }

    #[test]
    fn switched_off_views_stay_listed() {
        let mut page = HealthPage::default();
        page.views[3].health.off = true;
        page.views[1].health.hidden_tiles = vec![HealthTile::Pending];
        assert!(!page.is_default());
        assert_eq!(page.shown().count(), 3);
        assert_eq!(page.views.len(), 4);
        let text = toml::to_string(&page).unwrap();
        let back: HealthPage = toml::from_str(&text).unwrap();
        assert_eq!(back, page);
    }
}
