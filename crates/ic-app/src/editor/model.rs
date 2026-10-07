//! The dashboard editor's pure parts: the draft it starts from, the views
//! it manages (add, duplicate, move, remove), what the inspector and the
//! preview say about each view, what keeps a draft from being saved, and
//! where a filter error points.

use chrono::Local;
use ic_config::{GroupBy, GroupSource, MAX_VIEWS, ObjectKind, STREAM_LINES, View, ViewDisplay};
use ic_core::snapshot::{DashboardResult, Summary, ViewBody, ViewResult};
use ic_model::Timestamp;
use ic_rules::{DashboardRef, ScopeSetting};

use crate::app_state::AppState;
use crate::app_state::editing::{DashboardDraft, NEW_DASHBOARD_NAME};

/// What the editor edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditorTarget {
    /// A dashboard that doesn't exist yet.
    New,
    /// An existing dashboard.
    Existing(DashboardRef),
}

/// The draft for `target`: the dashboard as saved, or a new one in
/// `group_id` with one list view of unhandled service problems.
pub(crate) fn initial_draft(
    state: &AppState,
    target: &EditorTarget,
    group_id: &str,
) -> Option<DashboardDraft> {
    match target {
        EditorTarget::New => Some(DashboardDraft {
            name: NEW_DASHBOARD_NAME.to_owned(),
            views: vec![View {
                id: ic_config::new_id(),
                ..View::default()
            }],
            notifications: ScopeSetting::Inherit,
            group_id: group_id.to_owned(),
        }),
        EditorTarget::Existing(reference) => {
            let (group, dashboard) = state.dashboard(reference)?;
            Some(DashboardDraft {
                name: dashboard.name.clone(),
                views: dashboard.views.clone(),
                notifications: dashboard.notifications.clone(),
                group_id: group.id.clone(),
            })
        }
    }
}

/// `view` with a new object kind: hosts have no service groups, so a
/// grouping by service group goes back to none (a plain list).
pub(crate) fn with_kind(mut view: View, kind: ObjectKind) -> View {
    view.object_kind = kind;
    if kind == ObjectKind::Hosts && view.group_by == GroupBy::ServiceGroup {
        view.set_grouping(GroupBy::None);
    }
    view
}

/// `view` shown as `display`: a list and a grouped list switch into each
/// other keeping the rest (a grouped list groups by host unless it chose
/// a grouping); the other options stay for when it switches back.
pub(crate) fn with_display(mut view: View, display: ViewDisplay) -> View {
    view.display = display;
    if display == ViewDisplay::GroupedList && view.group_by == GroupBy::None {
        view.group_by = GroupBy::Host;
    }
    view
}

/// A display's name, as the editor's menus and new views give it.
pub(crate) fn display_name(display: ViewDisplay) -> &'static str {
    match display {
        ViewDisplay::List => "list",
        ViewDisplay::GroupedList => "grouped list",
        ViewDisplay::HostGroupGrid => "host-group grid",
        ViewDisplay::SummaryTiles => "summary tiles",
        ViewDisplay::EventStream => "event stream",
    }
}

/// What a display shows, in one line (*add view*, 4d).
pub(crate) fn display_detail(display: ViewDisplay) -> &'static str {
    match display {
        ViewDisplay::List => "one line per object",
        ViewDisplay::GroupedList => "by host or group",
        ViewDisplay::HostGroupGrid => "hosts as squares, by group",
        ViewDisplay::SummaryTiles => "counts per group",
        ViewDisplay::EventStream => "changes, acks, downtimes",
    }
}

/// A new view of `display` (*add view*), named after its display and
/// starting from `filter` (the selected view's).
pub(crate) fn new_view(display: ViewDisplay, filter: &str) -> View {
    with_display(
        View {
            id: ic_config::new_id(),
            name: display_name(display).to_owned(),
            filter: filter.to_owned(),
            ..View::default()
        },
        display,
    )
}

/// Adds `view` under the view at `after` (at the end without one).
/// Returns its index; `None` when the dashboard has [`MAX_VIEWS`] already.
pub(crate) fn insert_view(
    views: &mut Vec<View>,
    after: Option<usize>,
    view: View,
) -> Option<usize> {
    if views.len() >= MAX_VIEWS {
        return None;
    }
    let index = after.map_or(views.len(), |after| (after + 1).min(views.len()));
    views.insert(index, view);
    Some(index)
}

/// Copies the view at `index` (`<name> copy`, a fresh id) right under
/// it. Returns the copy's index.
pub(crate) fn duplicate_view(views: &mut Vec<View>, index: usize) -> Option<usize> {
    let original = views.get(index)?;
    let copy = View {
        id: ic_config::new_id(),
        name: format!("{} copy", view_name(original)),
        ..original.clone()
    };
    insert_view(views, Some(index), copy)
}

/// Removes the view at `index`, unless it is the dashboard's only one.
/// Returns the index of the view to select afterwards: the one that took
/// its place, or the new last one.
pub(crate) fn remove_view(views: &mut Vec<View>, index: usize) -> Option<usize> {
    if views.len() < 2 || index >= views.len() {
        return None;
    }
    views.remove(index);
    Some(index.min(views.len() - 1))
}

/// Moves the view at `from` to `to` (clamped to the list). Returns where
/// it is now; `None` when it didn't move.
pub(crate) fn move_view(views: &mut Vec<View>, from: usize, to: usize) -> Option<usize> {
    if from >= views.len() {
        return None;
    }
    let to = to.min(views.len() - 1);
    if from == to {
        return None;
    }
    let view = views.remove(from);
    views.insert(to, view);
    Some(to)
}

/// A view's name, or what it shows when it has none (a dashboard from
/// rc1 has one unnamed list: `service problems`).
pub(crate) fn view_name(view: &View) -> String {
    let name = view.name.trim();
    if !name.is_empty() {
        return name.to_owned();
    }
    if view.is_list() {
        crate::dashboard::header::view_label(view).to_owned()
    } else {
        display_name(view.display).to_owned()
    }
}

/// How many objects a summary counts (every filter match, before
/// *problems only* and the handled switches).
pub(crate) fn matches(summary: &Summary) -> u32 {
    summary.ok
        + summary.critical
        + summary.warning
        + summary.unknown
        + summary.down
        + summary.unreachable
        + summary.pending
}

/// `1 match`, `6 matches`.
fn counted(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

/// What a view shows, at the right of its row in the views list (4c): a
/// list's rows (`6 matches`), a grid's hosts (`124 hosts`), the tiles
/// (`4 tiles`), a stream's events of today (`8 today`); `invalid` when
/// its filter doesn't work (`true`); nothing before it was evaluated.
pub(crate) fn shows_text(result: Option<&ViewResult>, now: Timestamp) -> (String, bool) {
    let Some(result) = result else {
        return (String::new(), false);
    };
    if result.error.is_some() {
        return ("invalid".to_owned(), true);
    }
    let text = match &result.body {
        ViewBody::List(_) => counted(matches(&result.shown) as usize, "match", "matches"),
        ViewBody::Grid(grid) => counted(grid.hosts as usize, "host", "hosts"),
        ViewBody::Tiles(tiles) => counted(tiles.len(), "tile", "tiles"),
        ViewBody::Stream(events) => {
            let today = events
                .iter()
                .filter(|event| same_day(event.at, now))
                .count();
            format!("{today} today")
        }
    };
    (text, false)
}

/// Whether `at` is on the same local day as `now`.
fn same_day(at: Timestamp, now: Timestamp) -> bool {
    match (
        crate::format::date_time(at, &Local),
        crate::format::date_time(now, &Local),
    ) {
        (Some(at), Some(now)) => at.date_naive() == now.date_naive(),
        _ => false,
    }
}

/// The filter field's status for an evaluated view (4c, 4e, 5e): `valid ·
/// 8 matches, 2 handled` (a list: what the filter matches, and how many
/// of them count as handled), `valid · 124 hosts` (a grid), `valid · 38
/// services` (tiles), `valid · 12 events` (a stream).
pub(crate) fn status_text(view: &View, result: &ViewResult) -> String {
    let matched = matches(&result.summary) as usize;
    match &result.body {
        ViewBody::List(_) => {
            let text = format!("valid · {}", counted(matched, "match", "matches"));
            if result.handled > 0 {
                format!("{text}, {} handled", result.handled)
            } else {
                text
            }
        }
        ViewBody::Grid(grid) => {
            format!("valid · {}", counted(grid.hosts as usize, "host", "hosts"))
        }
        ViewBody::Tiles(_) => match view.object_kind {
            ObjectKind::Hosts => format!("valid · {}", counted(matched, "host", "hosts")),
            ObjectKind::Services => format!("valid · {}", counted(matched, "service", "services")),
        },
        ViewBody::Stream(_) if view.filter.trim().is_empty() => {
            "valid · every host and service".to_owned()
        }
        ViewBody::Stream(events) => format!("valid · {}", counted(events.len(), "event", "events")),
    }
}

/// The error of view `view_id` in a preview, if its filter doesn't work.
pub(crate) fn view_error<'a>(result: &'a DashboardResult, view_id: &str) -> Option<&'a str> {
    result.view(view_id)?.error.as_deref()
}

/// What keeps a view from being saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    /// Its filter doesn't work (why, with its line and column).
    Filter(String),
    /// A grid or tiles grouped by a custom variable without a name.
    NoCustomVar,
    /// Hosts grouped by service group.
    HostsByServiceGroup,
    /// A stream's lines out of range.
    Lines,
}

impl Problem {
    /// The problem as the editor says it; `view` names the view when the
    /// dashboard has several.
    pub(crate) fn message(&self, view: Option<&str>) -> String {
        match (self, view) {
            (Self::Filter(error), None) => format!("Fix the filter first: {error}"),
            (Self::Filter(error), Some(view)) => {
                format!("Fix the filter of {view} first: {error}")
            }
            (Self::NoCustomVar, None) => "Name the custom variable to group by.".to_owned(),
            (Self::NoCustomVar, Some(view)) => {
                format!("Name the custom variable {view} groups by.")
            }
            (Self::HostsByServiceGroup, view) => {
                with_view("Hosts can't be grouped by service group", view)
            }
            (Self::Lines, view) => with_view(
                &format!(
                    "Show between {} and {} lines",
                    STREAM_LINES.start(),
                    STREAM_LINES.end()
                ),
                view,
            ),
        }
    }
}

/// `text.`, or `text (view).`
fn with_view(text: &str, view: Option<&str>) -> String {
    match view {
        Some(view) => format!("{text} ({view})."),
        None => format!("{text}."),
    }
}

/// Why `views` can't be saved: the first view with a problem (its index)
/// and what it is. Filters that don't parse, a grid or tiles grouped by a
/// custom variable without a name, hosts grouped by service group, a
/// stream's lines out of range. The core's verdict on filters that parse
/// but don't evaluate comes with the preview.
pub(crate) fn check_views(views: &[View]) -> Result<(), (usize, Problem)> {
    for (index, view) in views.iter().enumerate() {
        check_filter(&view.filter).map_err(|error| (index, Problem::Filter(error)))?;
        let grouped = matches!(
            view.display,
            ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles
        );
        if grouped
            && view.groups.by == GroupSource::CustomVar
            && view.groups.custom_var_name().is_empty()
        {
            return Err((index, Problem::NoCustomVar));
        }
        if view.object_kind == ObjectKind::Hosts && view.list_grouping() == GroupBy::ServiceGroup {
            return Err((index, Problem::HostsByServiceGroup));
        }
        if view.display == ViewDisplay::EventStream && !STREAM_LINES.contains(&view.stream.lines) {
            return Err((index, Problem::Lines));
        }
    }
    Ok(())
}

/// Checks that `filter` parses, as the core does before evaluating it:
/// the error names the line and column like the core's (`… (line 1,
/// column 50)`).
///
/// # Errors
///
/// The filter doesn't parse.
pub(crate) fn check_filter(filter: &str) -> Result<(), String> {
    ic_filter::Filter::parse(filter).map(drop).map_err(|error| {
        let (line, column) = error.line_column(filter);
        format!("{} (line {line}, column {column})", error.message)
    })
}

/// Where a filter error points: the message's trailing `(line L, column
/// C)`, as `ic-core` writes it, as 1-based line and column.
pub(crate) fn error_position(message: &str) -> Option<(usize, usize)> {
    let start = message.rfind("(line ")?;
    let rest = message[start + "(line ".len()..].strip_suffix(')')?;
    let (line, column) = rest.split_once(", column ")?;
    let line = line.trim().parse().ok()?;
    let column = column.trim().parse().ok()?;
    (line > 0 && column > 0).then_some((line, column))
}

/// How many characters of the offending line the error marker shows: what
/// fits the inspector's filter field in the monospace font.
pub(crate) const MARKER_CHARS: usize = 40;

/// The line of `source` an error points at, and a caret under its column
/// (`^`), for the error marker under the filter field. At most `width`
/// characters show: a longer line is cut around the column (with `…` where
/// it was cut), so the caret is always in view.
pub(crate) fn error_marker(
    source: &str,
    line: usize,
    column: usize,
    width: usize,
) -> Option<(String, String)> {
    let text: Vec<char> = source.lines().nth(line.checked_sub(1)?)?.chars().collect();
    // Past the end of the line (an unexpected end): the caret follows the
    // last character.
    let caret_at = column.saturating_sub(1).min(text.len());
    let width = width.max(12);
    // The columns the line and its caret need.
    let span = text.len().max(caret_at + 1);
    if span <= width {
        return Some((text.iter().collect(), format!("{}^", " ".repeat(caret_at))));
    }
    // A window ending a little after the caret (or at the line's end),
    // with `…` where the line was cut.
    let after = 8.min(width / 3);
    let window_end = (caret_at + 1 + after).min(span);
    let cut_after = window_end < text.len();
    let keep = width - 1 - usize::from(cut_after);
    let (shown, caret_column) = match window_end.checked_sub(keep) {
        Some(start) if start > 0 => {
            let end = window_end.min(text.len());
            let mut shown = String::from("…");
            shown.extend(&text[start..end]);
            if cut_after {
                shown.push('…');
            }
            (shown, 1 + caret_at - start)
        }
        // The caret is near the start: cut the end only.
        _ => {
            let mut shown: String = text[..width - 1].iter().collect();
            shown.push('…');
            (shown, caret_at)
        }
    };
    Some((shown, format!("{}^", " ".repeat(caret_column))))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ic_core::snapshot::{DashboardRow, ViewBody, ViewResult};
    use ic_model::{ObjectKey, Timestamp};

    use super::*;

    #[test]
    fn filters_are_checked_like_the_core_checks_them() {
        assert_eq!(check_filter(""), Ok(()));
        assert_eq!(check_filter("host.vars.role == \"db\""), Ok(()));
        let error =
            check_filter("host.vars.role == \"postgres\" && service.state != ").unwrap_err();
        assert_eq!(error_position(&error), Some((1, 50)), "{error}");
    }

    #[test]
    fn errors_point_at_their_line_and_column() {
        assert_eq!(
            error_position("unexpected token `&&` (line 2, column 14)"),
            Some((2, 14))
        );
        assert_eq!(error_position("no position"), None);
        assert_eq!(error_position("(line 0, column 1)"), None);
        assert_eq!(error_position("bad (line x, column 1)"), None);
        let (text, caret) = error_marker("a == 1 &&\nb ==", 2, 5, MARKER_CHARS).unwrap();
        assert_eq!(text, "b ==");
        assert_eq!(caret, "    ^");
        let (_, caret) = error_marker("ab", 1, 9, MARKER_CHARS).unwrap();
        assert_eq!(caret, "  ^", "the caret stops after the line");
        assert!(error_marker("one line", 3, 1, MARKER_CHARS).is_none());
    }

    /// The character the caret points at in a marker (`None` past the
    /// end).
    fn under_caret(text: &str, caret: &str) -> Option<char> {
        text.chars().nth(caret.chars().count() - 1)
    }

    #[test]
    fn long_lines_are_cut_around_the_error() {
        let filter = "host.vars.role == \"postgres\" && service.state != ";
        // The end of the filter, column 50: shown with what leads to it.
        let (text, caret) = error_marker(filter, 1, 50, 30).unwrap();
        assert!(text.chars().count() <= 30, "{text}");
        assert!(caret.chars().count() <= 30, "{caret}");
        assert!(text.starts_with('…'), "{text}");
        assert!(text.ends_with("service.state != "), "{text}");
        assert_eq!(under_caret(&text, &caret), None, "after the last character");

        // An error in the middle of a long line: context on both sides.
        let long = format!(
            "{} && oops( && {}",
            "a == 1 && ".repeat(5),
            "b == 2 && ".repeat(5)
        );
        let column = long.find("oops").unwrap() + 1;
        let (text, caret) = error_marker(&long, 1, column, 30).unwrap();
        assert_eq!(text.chars().count(), 30, "{text}");
        assert!(text.starts_with('…') && text.ends_with('…'), "{text}");
        assert_eq!(under_caret(&text, &caret), Some('o'));
        assert!(text.contains("oops("));

        // Near the start: cut at the end only.
        let (text, caret) = error_marker(&long, 1, 3, 30).unwrap();
        assert!(!text.starts_with('…') && text.ends_with('…'), "{text}");
        assert_eq!(under_caret(&text, &caret), Some('='));
    }

    #[test]
    fn the_status_says_what_the_filter_matches() {
        let summary = Summary {
            ok: 9,
            critical: 2,
            ..Summary::default()
        };
        let rows = vec![
            DashboardRow::Object(ObjectKey::host("a")),
            DashboardRow::Object(ObjectKey::host("b")),
        ];
        let list = ViewResult {
            id: "v".to_owned(),
            body: ViewBody::List(Arc::new(rows)),
            summary,
            shown: Summary {
                critical: 2,
                ..Summary::default()
            },
            handled: 2,
            ..ViewResult::default()
        };
        let view = View::default();
        assert_eq!(status_text(&view, &list), "valid · 11 matches, 2 handled");
        let now = Timestamp::from_unix_seconds(1_790_000_000.);
        assert_eq!(
            shows_text(Some(&list), now),
            ("2 matches".to_owned(), false)
        );
        let one = ViewResult {
            summary: Summary {
                ok: 1,
                ..Summary::default()
            },
            handled: 0,
            ..list.clone()
        };
        assert_eq!(status_text(&view, &one), "valid · 1 match");
        assert_eq!(shows_text(None, now), (String::new(), false));
        // A view whose filter fails.
        let failed = ViewResult {
            error: Some("bad".to_owned()),
            ..list
        };
        assert_eq!(shows_text(Some(&failed), now), ("invalid".to_owned(), true));
        let dashboard = DashboardResult {
            summary: Summary::default(),
            views: vec![failed],
        };
        assert_eq!(view_error(&dashboard, "v"), Some("bad"));
        assert_eq!(view_error(&dashboard, "gone"), None);
    }

    #[test]
    fn grids_tiles_and_streams_say_what_they_show() {
        let now = Timestamp::from_unix_seconds(1_790_000_000.);
        let grid = ViewResult {
            body: ViewBody::Grid(Arc::new(ic_core::snapshot::Grid {
                groups: Vec::new(),
                hosts: 124,
            })),
            ..ViewResult::default()
        };
        let grid_view = new_view(ViewDisplay::HostGroupGrid, "");
        assert_eq!(status_text(&grid_view, &grid), "valid · 124 hosts");
        assert_eq!(shows_text(Some(&grid), now).0, "124 hosts");
        let tiles = ViewResult {
            body: ViewBody::Tiles(Arc::new(vec![ic_core::snapshot::Tile::default(); 4])),
            summary: Summary {
                ok: 38,
                ..Summary::default()
            },
            ..ViewResult::default()
        };
        let tiles_view = new_view(ViewDisplay::SummaryTiles, "");
        assert_eq!(status_text(&tiles_view, &tiles), "valid · 38 services");
        assert_eq!(shows_text(Some(&tiles), now).0, "4 tiles");
        let event = |seconds_ago: f64| ic_core::LogEntry {
            at: Timestamp::from_unix_seconds(now.as_unix_seconds() - seconds_ago),
            object: ObjectKey::host("a"),
            kind: ic_core::LogKind::CommentAdded,
            text: String::new(),
            author: None,
        };
        let stream = ViewResult {
            body: ViewBody::Stream(Arc::new(vec![event(1.), event(2.), event(3. * 86_400.)])),
            ..ViewResult::default()
        };
        let mut stream_view = new_view(ViewDisplay::EventStream, "");
        assert_eq!(shows_text(Some(&stream), now).0, "2 today");
        assert_eq!(
            status_text(&stream_view, &stream),
            "valid · every host and service"
        );
        stream_view.filter = "host.name == \"a\"".to_owned();
        assert_eq!(status_text(&stream_view, &stream), "valid · 3 events");
    }

    #[test]
    fn views_are_added_duplicated_moved_and_removed() {
        let mut views = vec![View {
            id: "a".to_owned(),
            ..View::default()
        }];
        // A new view goes under the selected one, named after its display,
        // starting from the selected view's filter.
        let added = new_view(ViewDisplay::EventStream, "host.vars.env == \"prod\"");
        assert_eq!(added.name, "event stream");
        assert_eq!(added.filter, "host.vars.env == \"prod\"");
        assert!(!added.id.is_empty());
        let index = insert_view(&mut views, Some(0), added).unwrap();
        assert_eq!(index, 1);
        let grouped = new_view(ViewDisplay::GroupedList, "");
        assert_eq!(grouped.list_grouping(), GroupBy::Host);
        assert_eq!(insert_view(&mut views, Some(0), grouped), Some(1));
        assert_eq!(views[2].display, ViewDisplay::EventStream);
        // A copy follows its original.
        assert_eq!(duplicate_view(&mut views, 2), Some(3));
        assert_eq!(views[3].name, "event stream copy");
        assert_ne!(views[3].id, views[2].id);
        // An unnamed rc1 list is copied under what it shows.
        assert_eq!(duplicate_view(&mut views, 0), Some(1));
        assert_eq!(views[1].name, "service problems copy");
        // Moves are clamped and report where the view went.
        assert_eq!(move_view(&mut views, 0, 9), Some(4));
        assert_eq!(views[4].id, "a");
        assert_eq!(move_view(&mut views, 4, 4), None);
        assert_eq!(move_view(&mut views, 9, 0), None);
        // Removing selects the view that took its place, or the new last.
        assert_eq!(remove_view(&mut views, 4), Some(3));
        assert_eq!(remove_view(&mut views, 0), Some(0));
        while views.len() > 1 {
            remove_view(&mut views, 0).unwrap();
        }
        assert_eq!(remove_view(&mut views, 0), None, "the last view stays");
        // At most MAX_VIEWS.
        while insert_view(&mut views, None, View::default()).is_some() {}
        assert_eq!(views.len(), MAX_VIEWS);
        assert_eq!(duplicate_view(&mut views, 0), None);
    }

    #[test]
    fn displays_switch_keeping_the_rest() {
        let list = View {
            filter: "x".to_owned(),
            ..View::default()
        };
        let grouped = with_display(list.clone(), ViewDisplay::GroupedList);
        assert_eq!(grouped.list_grouping(), GroupBy::Host);
        assert_eq!(grouped.filter, "x");
        let back = with_display(grouped, ViewDisplay::List);
        assert_eq!(back.list_grouping(), GroupBy::None);
        let grid = with_display(back, ViewDisplay::HostGroupGrid);
        assert!(!grid.is_list());
        for display in ViewDisplay::ALL {
            assert!(!display_name(display).is_empty());
            assert!(!display_detail(display).is_empty());
        }
        assert_eq!(
            view_name(&new_view(ViewDisplay::SummaryTiles, "")),
            "summary tiles"
        );
        assert_eq!(
            view_name(&View {
                display: ViewDisplay::HostGroupGrid,
                ..View::default()
            }),
            "host-group grid"
        );
    }

    #[test]
    fn drafts_are_checked_before_saving() {
        let mut views = vec![View::default(), new_view(ViewDisplay::SummaryTiles, "")];
        assert_eq!(check_views(&views), Ok(()));
        views[1].groups.by = GroupSource::CustomVar;
        assert_eq!(check_views(&views), Err((1, Problem::NoCustomVar)));
        assert_eq!(
            Problem::NoCustomVar.message(Some("sites")),
            "Name the custom variable sites groups by."
        );
        views[1].groups.custom_var = "host.vars.site".to_owned();
        assert_eq!(check_views(&views), Ok(()));
        views[0].filter = "service.state != ".to_owned();
        let (index, problem) = check_views(&views).unwrap_err();
        assert_eq!(index, 0);
        let message = problem.message(None);
        assert!(message.starts_with("Fix the filter first: "), "{message}");
        assert!(message.contains("(line 1, column 18)"), "{message}");
        assert!(
            problem
                .message(Some("failing"))
                .starts_with("Fix the filter of failing first: ")
        );
        views[0].filter.clear();
        views.push(new_view(ViewDisplay::EventStream, ""));
        views[2].stream.lines = 0;
        assert_eq!(check_views(&views), Err((2, Problem::Lines)));
        assert_eq!(
            Problem::Lines.message(None),
            "Show between 1 and 200 lines."
        );
        views[2].stream.lines = 8;
        views[0].object_kind = ObjectKind::Hosts;
        views[0].set_grouping(GroupBy::ServiceGroup);
        assert_eq!(check_views(&views), Err((0, Problem::HostsByServiceGroup)));
        assert_eq!(
            Problem::HostsByServiceGroup.message(Some("x")),
            "Hosts can't be grouped by service group (x)."
        );
    }

    #[test]
    fn hosts_have_no_service_groups() {
        let mut view = View::default();
        view.set_grouping(GroupBy::ServiceGroup);
        assert_eq!(
            with_kind(view.clone(), ObjectKind::Hosts).list_grouping(),
            GroupBy::None
        );
        assert_eq!(
            with_kind(view, ObjectKind::Services).list_grouping(),
            GroupBy::ServiceGroup
        );
    }

    #[test]
    fn drafts_start_from_the_dashboard_or_the_defaults() {
        let state = AppState::fixture(Timestamp::from_unix_seconds(1_790_000_000.));
        let selected = state.selected().unwrap().clone();
        let draft = initial_draft(&state, &EditorTarget::Existing(selected.clone()), "x").unwrap();
        assert_eq!(draft.name, "production");
        assert_eq!(draft.group_id, selected.group_id);
        let new = initial_draft(&state, &EditorTarget::New, "lab").unwrap();
        assert_eq!(new.name, NEW_DASHBOARD_NAME);
        assert_eq!(new.group_id, "lab");
        assert_eq!(
            new.views.len(),
            1,
            "a new dashboard starts with one list view"
        );
        assert!(new.views[0].is_list());
        assert!(new.views[0].problems_only);
        assert_eq!(new.views[0].handled, ic_config::HandledSetting::SETTINGS);
        assert!(!new.views[0].id.is_empty());
        let gone = DashboardRef {
            group_id: "x".to_owned(),
            dashboard_id: "y".to_owned(),
        };
        assert!(initial_draft(&state, &EditorTarget::Existing(gone), "x").is_none());
    }
}
