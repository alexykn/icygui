Duplication scout report: crates/ic-core/src (tests, *tests.rs, tests/ and #[cfg(test)] modules skipped). 20 findings, most valuable first.

1. Host-group glob matcher diverges from ic-filter's match()
- CORE/dashboards/groups.rs:31-80 (Pattern, glob_matches, case-sensitive, no escapes); test at CORE/dashboards/groups.rs:529-541 asserts "case-sensitive, as Icinga" (line 540).
- FILTER/src/pattern.rs:38-60 (Glob: case_insensitive(true), `\*` escapes), glob_to_regex at FILTER/src/pattern.rs:68-96.
- What: the same Icinga match() semantics are implemented twice, and the two disagree on case. FILTER's module doc says ASCII letters match case-insensitively and was checked against Icinga's C code. The groups test asserts the opposite. Exact host-group names also compare with == (groups.rs:47-49), which may be case-sensitive where Icinga's would not be.
- Suggestion: expose one pub glob function from ic-filter and use it in groups.rs. Resolve the case question against Icinga first; this is a behaviour bug, not only duplication.

2. The "handled" predicate is written four times, with the bits of Host/Service::counts_as_handled re-derived by hand
- CORE/dashboards/board.rs:113-125 (handled_reasons, doc says it splits Host/Service::counts_as_handled), used by board.rs:82-108 (Facts::of_host/of_service). board.rs:52-57: Facts.handled is documented as "the same as reasons != 0", so it is a redundant field.
- MODEL/src/object.rs:289-291 and 299-301 (Host::is_handled, counts_as_handled); object.rs:361-363 and 373-375 (Service).
- CORE/engine/notify.rs:1171-1177 (handled(view, host_problem); adds `!view.reachable`).
- FILTER/src/scope.rs:189-192 (ServiceScope::handled; deliberately differs from Service::is_handled, documented at scope.rs:127-131).
- MODEL/src/severity.rs:45-55 (handling_weight, same ack/downtime/unreachable tiers).
- What: four definitions of "handled" with slightly different inputs, and none is shared.
- Suggestion: one model-level function returning a bit set (acknowledged, in downtime, host down, unreachable); board.rs bits, severity weights and the filter variant become callers. Drop Facts.handled and derive it from reasons.

3. The services-of-host range scan is copied six times, with two different helpers
- CORE/snapshot.rs:115-125 (Snapshot::services_of, inline ServiceKey range).
- CORE/store/mod.rs:348-357 (problem_services_of), CORE/store/mod.rs:1109-1116 (remove_objects).
- CORE/dashboards/mod.rs:572-577 (Dirty::new) and helper CORE/dashboards/mod.rs:588-594 (service_range).
- CORE/dashboards/board.rs:430-434 (for_each_counted, uses service_range).
- CORE/dashboards/groups.rs:215-219 (grid).
- What: each copy is a BTreeMap range from a sentinel ServiceKey plus take_while on the host. The sentinel is built inline in some places and via service_range in others.
- Suggestion: one helper (e.g. on ServiceKey or as a free fn in ic-model) used by snapshot, store and dashboards.

4. The "Ok or Up" predicate is written three times in ic-core/ic-rules, and twice more in ic-app
- CORE/dashboards/groups.rs:348-354 (is_ok(cell)); CORE/dashboards/stream.rs:53-59 (is_recovery); RULES/src/engine.rs:1386-1393 (is_up).
- Same predicate also inline at CORE/summary.rs:41-42 (ok bucket). Outside scope, same copy at ic-app/src/dashboard/page.rs:1629 and ic-app/src/dashboard/draw.rs:1532.
- What: identical match on Service(Ok) | Host(Up).
- Suggestion: CheckableState::is_ok() in MODEL/src/event.rs next to is_problem (lines 25-33), used by all of them.

5. Store::put_host and Store::put_service are near-duplicates
- CORE/store/mod.rs:1000-1035 and CORE/store/mod.rs:1037-1091.
- What: both do superseded check, removed.remove, merge with stored (evented_after → keep_event_fields, otherwise push Discovered), equality short-circuit with mark_fetched, insert, mark_fetched, changes.any/objects. Only the lean-detail merge differs.
- Suggestion: one put_object generic over Host/Service with a closure for the detail-specific merge.

6. Effective check interval (retry while soft problem, else check interval) is computed three times, with different preconditions
- CORE/engine/quiet.rs:302-318 (result_due; uses features.active_checks; non-finite → INFINITY).
- CORE/engine/watchdog.rs:224-229 (next_update; requires active && checked && soft problem).
- CORE/store/apply.rs:447-458 (record_result; requires result.active && soft).
- What: the same retry-or-check choice, but the three conditions are not the same, so the copies can drift.
- Suggestion: CheckInfo::interval_for(state) (or similar) in MODEL/src/object.rs; callers keep their own preconditions explicitly.

7. ViewResult literal repeated six times; BuiltGrid and BuiltTiles have identical fields
- CORE/dashboards/board.rs:725-736 (grid), 740-751 (tiles), 757-763 (stream), 767-773 (members), 796-801 (empty_result), 822-833 (list_result).
- CORE/dashboards/groups.rs:181-190 (BuiltGrid) and 419-428 (BuiltTiles): both have summary, counts, handled.
- What: the grid and tiles arms are the same 12-field struct literal with the same id/shown/hidden/hosts/services/error defaults.
- Suggestion: a single From/constructor taking (summary, counts, handled, body), and one shared Built struct.

8. grid() and tiles() duplicate the group build/rank/sort pipeline
- CORE/dashboards/groups.rs:193-282 (grid) and 432-516 (tiles).
- Repeated pieces: summary/counts/handled tallies (205-207 vs 442-444); "if !handled { counts.add }" (234-238 vs 483-487); label lookup (240-242 vs 460-463); group_rank → by_colour → sort_groups (261-272 vs 500-509); worst = max(rank).
- Also a third ordering rule for list group-by at CORE/dashboards/board.rs:953-959 (sort by worst severity, label, id).
- Suggestion: shared accumulate-and-finish helper keyed by group, returning (name, label, Tally, Rank) for both views.

9. Display-name fallback for group labels is written three times
- CORE/dashboards/groups.rs:164-175 (labels(): filters empty display names) and the lookups at groups.rs:240-242 and 460-463 (map_or_else to the group name).
- CORE/dashboards/board.rs:996-1027 (Labels::new / label: filter empty, fallback to name) and board.rs:1030-1042 (GroupBy::Host fallback to host name, same filter-empty shape); board.rs:1057-1064 (group_labels).
- Suggestion: one display_label(map, name) helper and one builder of group display maps from host_groups/service_groups.

10. list_counts re-encodes the visible() gate
- CORE/dashboards/board.rs:463-467 (visible: problems_only, mask, in_chip) and 471-475 (in_chip).
- CORE/dashboards/board.rs:886-922 (list_counts repeats problems_only at 895, in_chip at 903, mask at 892 and 908).
- Also reorder at board.rs:647-652 and finish at 694-697 use visible().
- Suggestion: have list_counts classify each member once through a single gate function returning hidden/shown/excluded, so the counts and rows cannot disagree.

11. Comment and downtime handling duplicates the same upsert/sort/remove code in three places
- CORE/store/mod.rs:1218-1232: upsert_comment and upsert_downtime have identical bodies; 1239-1246 sort_comments and sort_downtimes differ only by field; 1269-1282 remove_named is already generic.
- CORE/store/apply.rs:252-277 (on_comment) and 283-341 (on_downtime): same seq/annotations_fetched check, log_annotation, upsert-or-remove, sort, changes.any/objects, view_of.
- CORE/store/mod.rs:624-658 (apply_overview replays annotations with the same upsert/remove arms at 646-657).
- CORE/store/apply.rs:292-296 repeats CORE/store/mod.rs:378-383 (downtime_of) inline.
- Suggestion: a small trait (name(), object()) with one generic upsert, sort and remove, and one annotation apply path shared by live events and overview replay.

12. replace_hosts and replace_services have the same skeleton
- CORE/store/mod.rs:685-699 and 704-724: compute gone = stored keys not in fetched, remove_objects, put each, set changes.all and changes.any.
- Suggestion: one generic replace over keys with a put closure.

13. ObjectView and CheckableState are rebuilt from host/service fields in many places
- CORE/store/mod.rs:1011-1012, 1028, 1051-1052, 1081 (ObjectView::of(CheckableState::Host/Service(x.state), &x.check)); 361 (problem_services_of).
- CORE/store/mod.rs:226-237 (checkable) and CORE/store/apply.rs:234-244 (view_of, which repeats checkable's logic); CORE/store/apply.rs:82-96 (Target::state and view).
- Similar CheckableState::Host(host.state) constructions at CORE/dashboards/board.rs:67, 86, 100; groups.rs:368, 379, 393; summary.rs:20, 31; engine/notify.rs:323, 329.
- Suggestion: Host::checkable_state() and Service::checkable_state() in MODEL; view_of becomes checkable(key).map(ObjectView::of).

14. State ranking exists in three places with the same order
- CORE/summary.rs:80-93 (state_rank: down, critical, unreachable, unknown, warning, pending, ok).
- CORE/dashboards/groups.rs:290-304 (GroupRank colour tiers 3/2/1 in group_rank) and 306-335 (by_colour: down, critical, unreachable, unknown, warning, the same order as state_rank).
- Outside scope, same idea at ic-app/src/dashboard/page.rs:1677 (state_rank) and ic-app/src/background/tray.rs:43 (rank).
- Suggestion: CheckableState::severity_rank() and colour tier in MODEL; by_colour becomes a max over the same rank.

15. An f64 total-order wrapper is defined twice, and Timestamp is PartialOrd only
- CORE/dashboards/board.rs:127-150 (OrdF64).
- CORE/engine/watchdog.rs:285-300 (Seconds, same impls).
- Also total_cmp ordering at CORE/store/mod.rs:1234-1236 (cmp_time), CORE/store/mod.rs:288 (latest_check max_by), CORE/engine/mod.rs:1880-1883 (apply_lines max_by).
- MODEL/src/time.rs:9 derives PartialOrd only on Timestamp(f64).
- Suggestion: implement Ord/Eq for Timestamp via total_cmp in MODEL and delete both wrappers.

16. The dashboard Data is built three ways
- CORE/engine/publish.rs:145-152 (publish, built from the snapshot plus evaluation_time()).
- CORE/engine/publish.rs:232-242 (Engine::data, same fields from store accessors).
- CORE/dashboards/mod.rs:131-142 (Data::of_snapshot, uses snapshot.taken_at as now).
- Suggestion: one constructor taking an explicit now, used by all three; the difference is only the `now` value.

17. Notification seeding repeats state_change, and handled/host_problem pairs are repeated
- CORE/engine/notify.rs:342-368 (seed loop: push Change::State with previous None, then entry_of(LogKind::State)) and CORE/engine/notify.rs:751-795 (state_change: the same push and log entry).
- host_problem_of(store,&object) followed by handled(...) at notify.rs:344-355, 397, 445-447, 699-701, 762-773, 865-866.
- Suggestion: seeding calls a shared helper with previous: None; add handled_for(store, object, view) to remove the repeated pair.

18. Newest-first capped lists are merged in the same way twice, and stream selection is a near-copy
- CORE/engine/recent.rs:22-35 (record_log: reversed log then existing entries, truncate to RECENT_EVENTS) and CORE/engine/recent.rs:62-69 (on_recent_events: existing then older entries, truncate).
- CORE/dashboards/stream.rs:13-21 (select: filter, take(STREAM_EVENTS), cloned) and CORE/dashboards/stream.rs:28-35 (stream_events: the same pipeline without the member filter).
- Suggestion: one cap_newest helper; select can be stream_events with a member predicate.

19. The Board::reconfigure keyed-reuse pattern appears at two levels
- CORE/dashboards/mod.rs:210-237 (Dashboard::reconfigure: map views by id, drain, seen set, reuse or Board::new).
- CORE/dashboards/mod.rs:346-382 (Dashboards::configure: map dashboards by reference, drain, reuse or Dashboard::new).
- Suggestion: a small generic "take old by key or build" helper. Low value, but it removes two near-identical blocks.

20. The cancel check loop is repeated four times in Board evaluation
- CORE/dashboards/board.rs:491-496 (evaluate_all hosts), 499-506 (evaluate_all services), 516-521 (evaluate_some hosts), 525-530 (evaluate_some services). Each has `if index % CANCEL_EVERY == 0 && cancel.load(...) { return; }`.
- Suggestion: a helper (for_each_until_cancelled(iter, cancel, f) -> bool) used by all four.

Dead code: none confirmed. I checked these non-test items for callers and found all of them used: Snapshot::host_of, list_rows, services_of, notified, is_late, is_updating; ViewResult rows, is_empty, members; DashboardResult::first; Store downtime, comment, changes, node, cluster, set_hiding, set_dependencies, set_cluster, latest_check, object_count, hidden_count, track_appeared, take_appeared, release_hidden, note_deleted, note_reflected, shows_change, service_states, display_names. Dashboards::results and Store::icinga_notifications are already cfg(test). This was not an exhaustive reference check across every crate, so treat it as "no obvious dead code", not "verified none".

Count: 20 findings. Top 3 titles: (1) Host-group glob matcher diverges from ic-filter's match(); (2) The "handled" predicate is written four times; (3) The services-of-host range scan is copied six times.