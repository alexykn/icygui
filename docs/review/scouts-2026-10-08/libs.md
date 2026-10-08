Duplication scout report for /home/claude/icygui (read-only). Scope: ic-api, ic-config, ic-filter, ic-rules, ic-model, ic-platform (src, non-test). Cross-checked against ic-core and ic-app, and ic-mock where noted.

FINDINGS: 25 total. Ranked most valuable first within each group. Overall top 3: #1 (summary tie-break divergence), #6 (Icinga attribute lists), #7 (downtime in-effect rule). Line ranges are from grep and reads; a few end lines are approximate (~3 lines).

== Cross-crate (copies in ic-core / ic-app; in-scope home noted)

1. Summary tally and worst-state rank diverge between ic-core and ic-app (bug risk)
 Copies: ic-core/src/summary.rs:39-56 (add), 82-93 (state_rank: Down=6 > Critical=5); ic-app/src/dashboard/page.rs:1639-1675 (tally, same state-to-bucket match), 1677-1688 (state_rank: Critical=6 > Down=5); ic-app/src/fixture/evaluate.rs:82-~100 (summarize, third bucket match).
 What: the state-to-counter match is written three times. The two state_rank tables order Down and Critical oppositely, so the band dot can disagree with the core summary when a down host and a critical service tie on severity.
 Suggest: reuse ic-core's Summary and state_rank from the app's tally, and add a test that pins the tie-break.

2. Relative clock/day formatters repeated about eight times in ic-app
 Copies: ic-app/src/format.rs:48-63 (list_clock_in), 142-155 (clock_in); ic-app/src/lists/model.rs:495-512 (day_clock), 517-534 (short_when), 538-547 (same_day); ic-app/src/downtimes.rs:524-545 (at); ic-app/src/notifications/timing.rs:140-157 (when_in); ic-app/src/notifications/entry.rs:460-470 (time_label_in); ic-app/src/editor/model.rs:404 (same_day).
 What: each matches the day difference (today / tomorrow / within six days / other) and picks %H:%M, %a %H:%M or %b %-d %H:%M, with slightly different thresholds.
 Suggest: one day_relative_clock(at, now, zone, style) in format.rs; callers keep only their labels.

3. Timestamp-to-DateTime conversion repeated in ic-app, and one copy differs
 Copies: ic-app/src/format.rs:189-202 (date_time); ic-app/src/notifications/timing.rs:164-175 (local_time, same body); ic-app/src/notifications/entry.rs:474-485 (date_time, rounds to milliseconds, not seconds).
 What: format.rs and timing.rs are identical. entry.rs rounds to ms, so boundary timestamps can land in a different second or day.
 Suggest: keep one helper in format.rs; confirm whether entry.rs's ms rounding is intended before unifying.

4. Ellipsis truncation implemented in at least seven places, with verbatim copies
 Copies: ic-config/src/error.rs:147-153 (excerpt), 222-229 (floor_char_boundary); ic-api/src/error.rs:161-180 (error_message, cut at 200 chars, 173-176); ic-filter/src/value.rs:401-408 (floor_char_boundary, identical to config), 410-414 (finish_preview), 419 (preview_str); ic-filter/src/eval.rs:72-93 (quoted message cut); ic-platform/src/tray/menu.rs:320-335 (clean_label); ic-rules/src/text.rs:309-349 (clean, cut at MAX_TEXT_CHARS); ic-app/src/operate/forms.rs:743-744; ic-app/src/editor/model.rs:628-650.
 What: the same cut-at-N-characters-on-a-boundary-then-append-… logic. ic-api's error_message and ic-config's excerpt are the same function in two crates, and floor_char_boundary is copied verbatim.
 Suggest: one truncate_chars(text, max) -> Cow<str> and one floor_char_boundary in ic-model; ic-api, ic-config, ic-filter and ic-platform call them.

5. Control-character sanitizing in several places with overlapping rules
 Copies: ic-rules/src/text.rs:309-349 (clean: strips ESC sequences and bidi controls, control to space, trim, cut); ic-platform/src/tray/menu.rs:320-326 (clean_label: control to space); ic-platform/src/tray/mod.rs:317-321 (tooltip_text: control except newline to space); ic-platform/src/autostart/mod.rs:335-341 (validate_text: rejects control characters).
 What: three variants that turn controls into spaces, and one that rejects them. The tooltip and label versions differ only in newline handling.
 Suggest: one sanitize_line in ic-model with a newline flag; platform labels and tooltips call it; keep validate_text for file-format checks.

== ic-api

6. Icinga check-attribute name lists copied between ic-api and ic-filter (ic-mock also has a copy, out of scope)
 Copies: ic-api/src/detail.rs:111-143 (HOST_LEAN), 145-185 (HOST_FULL), 186-219 (SERVICE_LEAN), 220-261 (SERVICE_FULL), 262 (FULL_ONLY, cfg(test)); ic-filter/src/scope.rs:379-394 (HOST_ATTRIBUTES), 395-409 (SERVICE_ATTRIBUTES), 410-436 (CHECK_ATTRIBUTES, 24 names), 437 (LINK_ATTRIBUTES), 565 (CHECK_RESULT_ATTRIBUTES); ic-mock/src/model/attrs.rs:73 (copy).
 What: about 25 attribute names repeated. Detail's lists must match what the filter exposes, but only detail's own test checks subset relations, and nothing ties the two crates together.
 Suggest: define the check and link attribute lists once in ic-model and compose HOST_FULL and SERVICE_LEAN from them.

7. Downtime "in effect" rule encoded in ic-api and ic-model
 Copies: ic-api/src/wire.rs:508-530 (builds a Window, then destructures it again), 546-554 (struct Window), 556-566 (downtime_in_effect), 587-611 (downtime_in_effect_until, effect_end at ~594-606); ic-model/src/object.rs:483-535 (Downtime::phase, effective_start, effective_end).
 What: both encode fixed = [start, end) and flexible = [trigger, trigger + duration). ic-api applies it when Icinga omits is_in_effect; ic-model's phase() re-derives the same window states from the stored fields.
 Suggest: add Downtime::in_effect_at(now) in ic-model; wire.rs calls it on the built Downtime and the Window shim goes away.

8. TLS verifier delegation repeated three times in one file
 Copies: ic-api/src/tls.rs:197-218 (PinnedVerifier), 279-299 (ChainVerifier), 420-442 (CapturingVerifier).
 What: each impl forwards verify_tls12_signature, verify_tls13_signature and supported_verify_schemes to the same WebPkiSupportedAlgorithms with identical bodies.
 Suggest: a small Signatures(WebPkiSupportedAlgorithms) newtype implementing the three methods, held by each verifier.

9. send_action reimplements send's timeout and error mapping, without its logging
 Copies: ic-api/src/client.rs:839-865 (send_action; timeout and ApiError::from_reqwest at 847-858); ic-api/src/client.rs:1047-1068 (send; timeout, mapping, and the debug log).
 What: the same timeout and error mapping in two places. Action requests skip the debug log that normal requests get.
 Suggest: let send take the timeout as a parameter and return the reqwest error, so classification and logging live in one place.

10. URL and server-name validation split between ic-config and ic-api, with overlapping rules
 Copies: ic-config/src/environment.rs:290-325 (parse_api_url: https, host, credentials, query and fragment, /v1 path, trailing slash); ic-api/src/client.rs:1444-1465 (normalize_base: https, host, credentials, query and fragment cleared, /v1 stripped, trailing slash); ic-config/src/validate.rs:340-366 (server_name_problem) against ic-api/src/tls.rs:151-154 (parse_server_name) and 540-546 (host_server_name, bracket stripping also at 507-509).
 What: the same URL rules are checked twice, with different messages. ic-api cannot call ic-config because it has no dependency on it.
 Suggest: move URL normalization into ic-model (which can depend on url) and have both validate_api_url in ic-config and normalize_base in ic-api call it.

11. Dead or test-only public API: RequestBudget::counts and Client::unknown_attributes
 Copies: ic-api/src/budget.rs:110-116 (counts; callers only in tests at budget.rs:139, 190, 201); ic-api/src/client.rs:256-263 (unknown_attributes; no callers in the workspace; doc links only at client.rs:1092 and detail.rs:19).
 What: neither has a production caller.
 Suggest: #[cfg(test)] on counts; delete unknown_attributes unless it is kept as external diagnostic API.

12. Duplicated helpers and clamp trio inside ic-api
 Copies: ic-api/src/client.rs:1522-1524 (non_empty) and ic-api/src/wire.rs:965-967 (non_empty, identical); ic-api/src/wire.rs:978-999 (clamp_u8, clamp_u32, clamp_i32: same round, clamp and cast); ic-api/src/client.rs:1507-1513 (status_code, uses clamp_u32 then try_from).
 What: identical helpers and three copies of one clamp pattern.
 Suggest: a single non_empty and one generic clamp helper in wire.rs, imported by client.rs.

== ic-config

13. Fingerprint formatting duplicated in ic-config and ic-api
 Copies: ic-config/src/fingerprint.rs:58-70 (format_fingerprint, DIGITS table; re-exported at ic-config/src/lib.rs:42); ic-api/src/tls.rs:33-43 (format_fingerprint via format! and join; re-exported at ic-api/src/lib.rs:49).
 What: identical output ("AB:CD:..."). Both are public and ic-api does not depend on ic-config.
 Suggest: one implementation in ic-model next to a fingerprint type, re-exported by both crates.

14. Atomic write and permission helpers duplicated between ic-config and ic-platform
 Copies: ic-config/src/files.rs:103 (write_atomic), 269-286 (write_temp: tempfile in dir, sync_all), 288-300 (restrict_permissions, 0600), 309-313 (restrict_file), 322-330 (sync_dir); ic-platform/src/autostart/mod.rs:240-262 (write_atomically: same tempfile, write, sync_all, persist sequence), 266-279 (create_private_dirs, 0700), 280-289 (set_readable, 0644).
 What: the same write-sync-rename sequence and cfg(unix) permission shims. ic-platform skips the directory sync and uses 0644.
 Suggest: one shared atomic-write module (ic-config's files.rs is the fuller version) with the mode as a parameter, if the dependency graph allows ic-platform to use it.

15. Config and platform Io error variants are identical
 Copies: ic-config/src/error.rs:36-44 (Io variant; Display "{action} {}: {source}" at 37) and 111-118 (io constructor); ic-platform/src/error.rs:33-42 (Io variant; Display "cannot {action} {path}: {source}" at 34) and 59-66 (io constructor).
 What: same fields and constructor body; only the message text differs.
 Suggest: share one error variant or macro between the crates, and align the wording.

== ic-filter

16. Comparison match table written twice inside compare()
 Copies: ic-filter/src/ops.rs:433-441 (string arm, match at 436-439) and 443-453 (numeric arm, match at 448-451).
 What: the same four-arm operator match on a partial ordering, repeated for two value types.
 Suggest: one helper holds(op, Option<Ordering>) -> bool, returning false for None so NaN behaves as before, used by both arms.

17. Arity error text built four different ways
 Copies: ic-filter/src/functions.rs:194-200 (arity_error: "takes exactly 1 argument"), 182-192 (single_arg, hand-checks arity), 416 (range(): "takes 1 to 3 arguments"); ic-filter/src/methods.rs:265-275 (exact::<N>, plural-aware), 277-280 (at_least_one).
 What: four phrasings of the same error; only exact::<N> handles plurals.
 Suggest: one arity_message(name, expected, given) helper used everywhere.

18. Dead or test-only: Value::as_number
 Copies: ic-filter/src/value.rs:167 (as_number); the only callers are tests at value.rs:792 and 794.
 Suggest: remove it, or gate it with #[cfg(test)].

== ic-rules

19. Bounded FIFO memory duplicated across dedupe.rs, recent.rs and storm.rs
 Copies: ic-rules/src/dedupe.rs:31-43 (insert with pop_front eviction); ic-rules/src/recent.rs:38-60 (insert with eviction and stale-pair compaction); ic-rules/src/storm.rs:206-209 (push_back and pop_front to MAX_THRESHOLD) and 237-251 (forget_before pop loop).
 What: Dedupe is a Recent without refresh, and both implement the same eviction loop.
 Suggest: build Dedupe on Recent<Arc<str>, ()> with an insert-if-absent method, so eviction is written once.

20. State-to-flag mapping written three times
 Copies: ic-rules/src/engine.rs:1372-1383 (selects); ic-app/src/settings/model.rs:803-820 (SettingsSwitch::get) and 821-836 (set).
 What: the state-to-rule.states mapping appears in three matches, so adding a state needs three edits.
 Suggest: add StateFilter::selects(state) and get_mut(state) in ic-rules/src/settings.rs and call them from both places.

== ic-model

21. State names as words repeated in four places, with only one inverse parser
 Copies: ic-rules/src/text.rs:206-219 (state_slug); ic-app/src/format.rs:117-133 (state_word); ic-core/src/event_log/db.rs:471-484 (state_name) and 486-503 (parse_state, the inverse); ic-model/src/state.rs has no name() method. StateType words: ic-core/src/event_log/db.rs:506-511 (state_type_name) and 513-517 (parse_state_type); ic-app/src/format.rs:108-112 (attempt); ic-app/src/notifications/history.rs:76-77.
 What: identical lowercase name tables; only the db copy has a parser.
 Suggest: CheckableState::name() and from_name(), and StateType::name() and from_name(), in ic-model/src/state.rs; all callers use them.

22. Duration parsing and formatting split across ic-app and ic-model
 Copies: ic-model/src/time.rs:66-96 (format_compact, format_two_units); ic-app/src/operate/when.rs:97-150 (parse_duration, unit table s/m/h/d/w); ic-app/src/settings/model.rs:508-520 (parse_min_duration), 522-541 (format_min_duration, "1h30m"), 588-606 (parse_window, parse_seconds); ic-app/src/format.rs:86-93 (interval: under 120s, else format_two_units).
 What: one parser and two formatters with different separators ("1h30m" versus "1h 30m") spread across two crates.
 Suggest: move parse_duration next to format_two_units in ic-model, and choose one format for settings fields.

23. Clock-of-day parsing has two parsers with different rules, plus a formatter
 Copies: ic-app/src/settings/model.rs:540-565 (parse_clock to minutes after midnight, accepts "7" and "07.30"; format_clock at 562-564); ic-app/src/operate/when.rs:339-361 (parse_clock to NaiveTime, colon required); ic-app/src/operate/when.rs:24 and 303-305 (MORNING_HOUR, morning()).
 What: the two parsers accept different inputs for the same concept.
 Suggest: one parser in ic-model that returns minutes; convert to NaiveTime at the edge.

24. Dead or test-only: EventKind::from_api_name
 Copies: ic-model/src/event.rs:163-165 (from_api_name); the only callers in the workspace are tests at event.rs:373 and 375.
 Suggest: gate it with #[cfg(test)] or remove it.

== ic-platform

25. "Next 08:00 local" computed in two crates with two calendar libraries
 Copies: ic-platform/src/tray/menu.rs:49 (MORNING_HOUR = 8), 105-115 (until_morning), 117-130 (next_morning, jiff); ic-app/src/operate/when.rs:24 and 303-305 (morning(), chrono), 218-229 (tomorrow default, chrono). ic-core/src/ports.rs:82-89 also uses jiff.
 What: the same concept, rolling to tomorrow when 08:00 has passed, is implemented with jiff in ic-platform and chrono in ic-app. The constant is repeated.
 Suggest: one calendar library for the workspace, or at least one shared next_morning helper and constant in ic-core or ic-model.

Dead-code summary: no pub fn in the six crates is referenced only at its own definition. Test-only items are #11 (counts), #18 (as_number) and #24 (from_api_name). The only #[expect(dead_code)] in scope is in ic-config/src/error.rs:345 and 351, both in test code. Constants in scope all have real uses.

Checked and rejected as duplicates: ic-api/src/budget.rs and ic-filter/src/budget.rs (different concepts, a rate limiter and an evaluation limit); the two is_active autostart parsers (different file formats); Host::is_handled and Service::is_handled (different rules).