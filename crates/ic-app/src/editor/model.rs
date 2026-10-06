//! The dashboard editor's pure parts: the draft it starts from, what the
//! preview says about the filter, and where a filter error points.

use ic_config::{GroupBy, ObjectKind, View};
use ic_core::snapshot::{DashboardResult, Summary};
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
/// `group_id` listing unhandled service problems.
pub(crate) fn initial_draft(
    state: &AppState,
    target: &EditorTarget,
    group_id: &str,
) -> Option<DashboardDraft> {
    match target {
        EditorTarget::New => Some(DashboardDraft {
            name: NEW_DASHBOARD_NAME.to_owned(),
            view: View::default(),
            notifications: ScopeSetting::Inherit,
            group_id: group_id.to_owned(),
        }),
        EditorTarget::Existing(reference) => {
            let (group, dashboard) = state.dashboard(reference)?;
            Some(DashboardDraft {
                name: dashboard.name.clone(),
                view: dashboard.view.clone(),
                notifications: dashboard.notifications.clone(),
                group_id: group.id.clone(),
            })
        }
    }
}

/// `view` with a new object kind: hosts have no service groups, so a
/// grouping by service group goes back to none.
pub(crate) fn with_kind(mut view: View, kind: ObjectKind) -> View {
    view.object_kind = kind;
    if kind == ObjectKind::Hosts && view.group_by == GroupBy::ServiceGroup {
        view.group_by = GroupBy::None;
    }
    view
}

/// How many objects a summary counts (every filter match, before
/// `problems_only` and `hide_handled`).
pub(crate) fn matches(summary: &Summary) -> u32 {
    summary.ok
        + summary.critical
        + summary.warning
        + summary.unknown
        + summary.down
        + summary.unreachable
        + summary.pending
}

/// The filter's status line for a preview: `valid · 12 matches · 3
/// shown`.
pub(crate) fn status_text(result: &DashboardResult) -> String {
    let matched = matches(&result.summary);
    let shown = result
        .rows
        .iter()
        .filter(|row| matches!(row, ic_core::snapshot::DashboardRow::Object(_)))
        .count();
    let noun = if matched == 1 { "match" } else { "matches" };
    if usize::try_from(matched).is_ok_and(|matched| matched == shown) {
        format!("valid · {matched} {noun}")
    } else {
        format!("valid · {matched} {noun} · {shown} shown")
    }
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

/// The line of `source` an error points at, and a caret under its column
/// (`^`), for the error marker under the filter field.
pub(crate) fn error_marker(source: &str, line: usize, column: usize) -> Option<(String, String)> {
    let text = source.lines().nth(line.checked_sub(1)?)?;
    // Past the end of the line (an unexpected end): the caret follows the
    // last character.
    let width = text.chars().count();
    let caret = format!("{}^", " ".repeat(column.saturating_sub(1).min(width)));
    Some((text.to_owned(), caret))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ic_core::snapshot::DashboardRow;
    use ic_model::{ObjectKey, Timestamp};

    use super::*;

    #[test]
    fn errors_point_at_their_line_and_column() {
        assert_eq!(
            error_position("unexpected token `&&` (line 2, column 14)"),
            Some((2, 14))
        );
        assert_eq!(error_position("no position"), None);
        assert_eq!(error_position("(line 0, column 1)"), None);
        assert_eq!(error_position("bad (line x, column 1)"), None);
        let (text, caret) = error_marker("a == 1 &&\nb ==", 2, 5).unwrap();
        assert_eq!(text, "b ==");
        assert_eq!(caret, "    ^");
        let (_, caret) = error_marker("ab", 1, 9).unwrap();
        assert_eq!(caret, "  ^", "the caret stops after the line");
        assert!(error_marker("one line", 3, 1).is_none());
    }

    #[test]
    fn the_status_counts_matches_and_rows_shown() {
        let summary = Summary {
            ok: 9,
            critical: 2,
            handled: 1,
            ..Summary::default()
        };
        let rows = vec![
            DashboardRow::Group {
                label: "db".to_owned(),
                count: 2,
            },
            DashboardRow::Object(ObjectKey::host("a")),
            DashboardRow::Object(ObjectKey::host("b")),
        ];
        let result = DashboardResult {
            rows: Arc::new(rows),
            summary,
            error: None,
        };
        assert_eq!(status_text(&result), "valid · 11 matches · 2 shown");
        let all = DashboardResult {
            rows: Arc::new(vec![DashboardRow::Object(ObjectKey::host("a"))]),
            summary: Summary {
                ok: 1,
                ..Summary::default()
            },
            error: None,
        };
        assert_eq!(status_text(&all), "valid · 1 match");
    }

    #[test]
    fn hosts_have_no_service_groups() {
        let view = View {
            group_by: GroupBy::ServiceGroup,
            ..View::default()
        };
        assert_eq!(
            with_kind(view.clone(), ObjectKind::Hosts).group_by,
            GroupBy::None
        );
        assert_eq!(
            with_kind(view, ObjectKind::Services).group_by,
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
        assert!(new.view.problems_only && new.view.hide_handled);
        let gone = DashboardRef {
            group_id: "x".to_owned(),
            dashboard_id: "y".to_owned(),
        };
        assert!(initial_draft(&state, &EditorTarget::Existing(gone), "x").is_none());
    }
}
