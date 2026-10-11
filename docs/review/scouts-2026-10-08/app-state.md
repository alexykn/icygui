Duplication scout report for crates/ic-app (scope: app_state/, background/, live/, notifications/, fixture/, top-level .rs). Read-only; nothing was edited.

Findings: 17. Top 3: (1) Fixture re-implements core's overall summary with a different worst-state tie-break; (2) AppState::apply and the parked-slot branch of apply_from duplicate per-event handling; (3) Three copies of the local-time conversion helper.

1. Fixture re-implements core's overall summary, with a different worst-state tie-break.
Copies: crates/ic-app/src/fixture/evaluate.rs:40-67 (Match), 69-80 (overall), 82-113 (summarize; severity-only tie-break at 106-108). crates/ic-core/src/summary.rs:10-86 (Tally: add_host 18, add_service 28, add 39, problem 58-72 with tie-break, finish 74, state_rank 82).
Duplicated: classifying each host/service into Summary counters and choosing worst_unhandled. Fixture keeps the first object at equal severity (hosts before services); core breaks ties by state_rank, so the worst-state dot can differ between fixture-backed tests and the core.
Suggestion: expose core's Tally (or a pub Summary::of(hosts, services)) and delete the fixture copy. Also fixture:53,63 uses counts_as_handled where summary.rs:20-23,30-35 uses is_handled; these agree for problem objects only (ic-model object.rs:201-203), so it is harmless.

2. AppState::apply and the parked-slot branch of apply_from repeat per-event handling.
Copies: crates/ic-app/src/app_state/mod.rs:1097-1118 (apply: Snapshot, Connection, Permissions, Notification, ActionFinished, NotificationsPaused; set_snapshot at 1120). crates/ic-app/src/app_state/engines.rs:270-315 (apply_from; parked arms 297-312).
Duplicated: the same core events update slot state (connection.on_snapshot, snapshot, connection.on_state, update_pending check, permissions, push_notification) in two places. Differences: active path calls tracker.settle and logs "connected"; parked path logs "connected in the background" and sends to a specific environment.
Suggestion: EngineSlot::apply(&mut self, event) -> bool (true when a pending environment update should go out), used by both; the active path adds tracker.settle and its log line.

3. Three copies of the local-time conversion helper.
Copies: crates/ic-app/src/format.rs:189-201 (date_time, pub(crate)); crates/ic-app/src/notifications/timing.rs:164-175 (local_time; body identical to format::date_time); crates/ic-app/src/notifications/entry.rs:474-485 (date_time, which rounds to milliseconds rather than flooring seconds) and 487-489 (local_date).
Duplicated: Timestamp to chrono DateTime in a zone. The timing.rs copy is verbatim.
Suggestion: delete timing.rs local_time and entry.rs date_time and use format::date_time; decide once whether ms rounding is wanted.

4. Four clock-time formatters with overlapping branches.
Copies: crates/ic-app/src/format.rs:142-155 (clock_in: HH:MM same day, else "%b %-d %H:%M"); format.rs:48-63 (list_clock_in: HH:MM, "%b %-d" same year, "%Y"); crates/ic-app/src/notifications/timing.rs:140-157 (when_in: HH:MM, "tomorrow HH:MM", "%b %-d %H:%M"); crates/ic-app/src/notifications/entry.rs:460-472 (time_label_in: HH:MM, "%b %-d" when Older). Out of scope, same idea: lists/model.rs:509,531,573 and downtimes.rs:482,538,568.
Duplicated: the same-day-or-date decision with the same chrono format calls, repeated per site with small variants.
Suggestion: one clock(at, now, zone, style) in format.rs where the style selects today/tomorrow/year variants.

5. Prod-cluster sample and scenario-builder helpers are written twice (fixture and ic-mock).
Copies: crates/ic-app/src/fixture/objects.rs:15-24 (MINUTE/HOUR/DAY, ago), 28-45 (check_info), 47 (check_result), 587 (service), with objects.rs 921 lines total. crates/ic-mock/src/scenario/build.rs:111-113 (ago), 134 (result), 230 (service). crates/ic-mock/src/scenario/prod_cluster.rs (1289 lines) defines the same prod-cluster scenario that fixture/objects.rs describes as "the design's prod-cluster sample".
Duplicated: sample cluster objects and time/check builders. The demo serves ic-mock's copy; tests use the fixture's.
Suggestion: build the fixture from ic-mock's scenario (ic-app already depends on ic-mock, Cargo.toml:30), or move the shared builders into a pub module.

6. ConnectionState wording is mapped three times; NoticeKind mirrors the failure variants.
Copies: crates/ic-app/src/app_state/connection.rs:406 (label_parts), 455 (short_state), 479 (describe), plus engine_word at 311. NoticeKind at 90-103 mirrors the failure variants that notice() (588) maps.
Duplicated: each ConnectionState variant gets footer, short and detail text in three exhaustive matches. The wording already disagrees: "not trusted" (footer) vs "certificate not trusted" (short and detail); "connecting (2)" vs "connecting (attempt 2)".
Suggestion: one match per variant returning (footer, short, detail), so each variant is listed once.

7. Permission literals and the ObjectAction mapping are hand-typed; ic-core already lists them.
Copies: crates/ic-app/src/app_state/permissions.rs:13-25 (action_permission) and 30-40 (action_verb), parallel exhaustive matches over ObjectAction; literals at 64, 65, 83, 101, 102, 117. crates/ic-core/src/probe.rs:20 (REQUIRED_PERMISSIONS) repeats the same names. crates/ic-app/src/pane/model.rs:176 and 963 repeat "objects/query/Notification" (out of scope).
Duplicated: permission names, and the action to permission and verb tables, are typed in two crates.
Suggestion: export permission names as pub consts from ic-core (build REQUIRED_PERMISSIONS from them), and fold action_permission and action_verb into one table keyed by ObjectAction.

8. Pause and mute choices duplicate their arms; later() reimplements Timestamp::plus.
Copies: crates/ic-app/src/notifications/timing.rs:17-58 (PauseChoice) and 61-101 (MuteChoice). "for 1 hour" at 41-48 and 93-100 (label). Until-morning arms at 31-38 and 82-90 (until). later() at 159-162 vs crates/ic-model/src/time.rs:58-60 (Timestamp::plus).
Duplicated: the one-hour and until-morning options, their labels, and hand-computed seconds.
Suggestion: one shared enum or table for the endings, and Timestamp::plus for the arithmetic.

9. Mark-all-read duplicates mark_read_in(None).
Copies: crates/ic-app/src/app_state/notifications.rs:129-151 (mark_read_in, None branch) and 169-181 (mark_all_notifications_read).
Duplicated: the same loop setting read to true, tracking changed, and sending MarkNotificationsRead.
Suggestion: mark_all_notifications_read calls mark_read_in for the active environment with None, or EngineSlot::mark_all_read is shared by both.

10. state.update-and-notify boilerplate, including a repeated select-and-notify.
Copies: crates/ic-app/src/workspace.rs:512-515 (select), 521-524 (open_tab), 709-712 (tick_actions), 1071-1074 (show_dashboard; identical to 512-515), 1089-1092 (cycle_tab), 1101-1104 (close_tab), 1275-1278, 1311-1314. Grep counts: 30 "state.update(cx, |state, cx| {" blocks in workspace.rs and 25 in live/mod.rs; not all have the same shape.
Duplicated: "if state.X() { cx.notify() }" inside a state.update closure.
Suggestion: a helper change(cx, f: impl FnOnce(&mut AppState) -> bool) that notifies when f returns true.

11. State-word labels live in four places.
Copies: crates/ic-app/src/notifications/history.rs:153-166 (state_kind, uppercase, all states). crates/ic-rules/src/text.rs:104-120 (Label::title, uppercase problem subset, pub(crate); impl Label at 55). crates/ic-app/src/format.rs:117-135 (state_word, lowercase). crates/ic-model/src/state.rs:59-67 and 104-111 (short_label, abbreviations).
Suggestion: add label() (upper) and word() (lower) on CheckableState in ic-model (event.rs:25 has its impl) and use them from history.rs, ic-rules and format.rs.

12. Endpoint and user labels re-derive ic-config helpers.
Copies: crates/ic-app/src/app_state/mod.rs:1241-1248 (endpoint_of, host only), 1251-1258 (user_of). crates/ic-config/src/environment.rs:148 (ApiUrl::label: host, port and path), 81 (primary_url), 49 (author_name, username fallback). crates/ic-app/src/app_state/environments.rs:240-242 (connection_differs: wraps environment.rs:88 and adds an id check).
Duplicated: endpoint and username derivation. endpoint_of drops the port that ApiUrl::label keeps, so reuse changes the displayed text.
Suggestion: use ApiUrl::label and primary_url; keep endpoint_of only if the port-less host is intended.

13. Hand-rolled epoch time in three places.
Copies: crates/ic-app/src/live/mod.rs:646-649 (start_demo_core seed time). crates/ic-app/src/live/demo.rs:129-131 (seed), 414-417 (demo_password). Existing helper: crates/ic-model/src/time.rs:25-29 (Timestamp::now).
Suggestion: Timestamp::now().as_unix_seconds() (cast to u64 for the seed).

14. Permission pass-through wrappers repeat the same lookup.
Copies: crates/ic-app/src/app_state/mod.rs:541-576 (action_denial 542, query_denial 547, list_denial 552, only_mine_denial 557, handling_gap 564, can_read_notifications 575), each passing self.engine.permissions.as_ref() to app_state/permissions.rs. crates/ic-app/src/app_state/engines.rs:381-388 (action_denial_in) repeats it for slot(id).
Suggestion: give EngineSlot the methods and have AppState delegate to the active slot once.

15. only_mine_denial ignores its kind argument.
Copies: crates/ic-app/src/app_state/mod.rs:557-561 (_kind: ListKind, unused). Callers: crates/ic-app/src/lists/view.rs:386, 1204, 1357, 1437, 1593.
Suggestion: drop the parameter and its five call-site arguments, or document why it is kept.

16. Three tone enums with overlapping meanings.
Copies: crates/ic-app/src/app_state/connection.rs:38-44 (Tone: Critical, Warning). crates/ic-app/src/notifications/history.rs:19-33 (HistoryTone: State, Accent, Downtime, Flapping, Quiet). ic-rules Tone, used in the mapping at crates/ic-app/src/live/desktop.rs:118-126 and returned by Label::tone at crates/ic-rules/src/text.rs:140.
Suggestion: consider reusing ic-rules' Tone for connection notices, keeping HistoryTone only for history-specific accents. Not verified against every use site.

17. Evaluator is a zero-sized marker with an unused_self expect.
Copies: crates/ic-app/src/fixture/mod.rs:57-60 (struct Evaluator), 63-75 (evaluate, with #[expect(clippy::unused_self)]), and preview at 113-121, a free function that only calls Evaluator.evaluate.
Suggestion: make evaluate a free function and drop the marker and the expect.

Dead code: no unused production functions found. Functions referenced only from tests were checked; the ones I looked at (live/mod.rs 504-1531, workspace.rs 574-2464, app_state/notifications.rs:548) are behind #[cfg(all(test, target_os = "linux"))]. The macOS delegate callbacks (live/macos.rs:217, 242) are invoked by the runtime, not dead.

Coverage: I did not read in depth app_state/hydration.rs, operations.rs and presence.rs (except start_hidden), background/tray.rs, menus.rs, window.rs and instance.rs, live/dbus.rs and macos.rs (in part), live/demo.rs (in part), and notifications/entry.rs (in part). Duplicates there may have been missed. ui_tests/ and cfg(test) code were excluded, except where cited to confirm gating.