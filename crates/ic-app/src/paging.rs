//! A host's services paged by count (PLAN.md §4.2, the rule of rc1's host
//! pane): up to [`HOST_SERVICES_PREVIEW`] rows, the host's problems first
//! (worst first, never hidden, even when there are more than seven), then
//! OK services in name order to fill the seven; the rest wait behind
//! `+ N more`, which shows the whole host in place (and becomes `− show
//! fewer` in the same slot). Every host-with-services view (a dashboard
//! grouped by host, a grouped-list view) and the host pane page this way.
//!
//! Paging only changes what shows: counts, summaries and *mark all* cover
//! every service. Pure, so it's tested without a window.

use std::cmp::Ordering;

/// How many service rows a host shows before `+ N more`.
pub(crate) const HOST_SERVICES_PREVIEW: usize = 7;

/// The order of a host's services: worst first (Icinga's severity, so
/// unhandled problems before handled ones, and anything not OK before OK),
/// then by name. `a` and `b` are `(severity, name)`.
pub(crate) fn service_order(a: (u32, &str), b: (u32, &str)) -> Ordering {
    b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1))
}

/// How many of a host's `total` services show, `problems` of them not OK:
/// all when `expanded`, else up to [`HOST_SERVICES_PREVIEW`] and every
/// problem.
pub(crate) fn shown_count(total: usize, problems: usize, expanded: bool) -> usize {
    if expanded {
        total
    } else {
        HOST_SERVICES_PREVIEW.max(problems).min(total)
    }
}

/// Whether a host of `total` services, `problems` of them not OK, pages at
/// all: some of its services wait behind `+ N more` unless it is expanded.
/// Only such a host has the `+ N more` / `− show fewer` row.
pub(crate) fn pages(total: usize, problems: usize) -> bool {
    shown_count(total, problems, false) < total
}

/// The paging row's text: `+ 12 more` while `hidden` services wait, `−
/// show fewer` once the host shows them all.
pub(crate) fn more_label(hidden: usize) -> String {
    if hidden > 0 {
        format!("+ {hidden} more")
    } else {
        "− show fewer".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problems_are_never_hidden_and_ok_ones_fill_seven() {
        assert_eq!(shown_count(23, 2, false), 7, "2 problems, 5 OK");
        assert_eq!(shown_count(19, 0, false), 7, "an all-OK host: its first 7");
        assert_eq!(shown_count(12, 9, false), 9, "every problem, more than 7");
        assert_eq!(shown_count(5, 1, false), 5, "fewer than 7: all");
        assert_eq!(shown_count(23, 2, true), 23, "expanded: all");
        assert_eq!(shown_count(0, 0, false), 0);
    }

    #[test]
    fn only_hosts_with_more_than_they_show_page() {
        assert!(pages(23, 2));
        assert!(pages(8, 0));
        assert!(!pages(7, 0));
        assert!(!pages(9, 9), "all problems: nothing waits");
        assert!(!pages(0, 0));
    }

    #[test]
    fn worst_first_then_by_name() {
        let mut services = [
            (0, "swap"),
            (2176, "replication"),
            (0, "apt"),
            (2080, "load"),
        ];
        services.sort_by(|a, b| service_order(*a, *b));
        let names: Vec<_> = services.iter().map(|(_, name)| *name).collect();
        assert_eq!(names, ["replication", "load", "apt", "swap"]);
    }

    #[test]
    fn the_paging_row_reads_more_or_fewer() {
        assert_eq!(more_label(12), "+ 12 more");
        assert_eq!(more_label(0), "− show fewer");
    }
}
