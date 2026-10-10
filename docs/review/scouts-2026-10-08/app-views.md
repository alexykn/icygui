FINDINGS: 24 entries, most valuable first.
Top 3: (1) Selection bar built twice (dashboard/bulk.rs and lists/view.rs). (2) Fold chevron written four times; lists/draw.rs:chevron() exists but dashboard does not use it. (3) The "···" menu-trigger wrapper repeated about nine times.

1. Selection bar and its "···" menu built twice
   Copies: dashboard/bulk.rs:66-157 (render_selection_bar), dashboard/bulk.rs:179-201 (selection_more); lists/view.rs:2116-2215 (render_selection_bar), lists/view.rs:2218-2254 (selection_more). Constants: lists/view.rs:75, 77; dashboard/bulk.rs:24, 27.
   Duplicated: same bar (id, SELECTION_BAR_HEIGHT, border and background, count slot sized by COUNT_SLOT_CHARS, buttons, "···", flex spacer, "clear" link with Esc hint) and the same "···" trigger and `.above()` popover.
   Suggest: one shared SelectionBar builder (count, buttons, more-menu, compact flag) in a shared module such as lists/draw.rs.

2. Fold chevron written four times; the shared helper is unused by the dashboard
   Copies: dashboard/draw.rs:398-432 (band chevron wrapper, icon at 415-422); dashboard/header.rs:995-1021 (view-header chevron, icon at 1006-1011); lists/view.rs:2855-2866 (fold line, via draw::chevron); lists/view.rs:3026-3040 (thread band chevron wrapper, via draw::chevron). Helper: lists/draw.rs:407-417 chevron(open, theme), called only at lists/view.rs:2865 and 3039.
   Duplicated: an absolute or flex 12px chevron wrapper with mouse-down prevention and a click that stops propagation, plus the ChevronRight/ChevronDown switch.
   Suggest: extend draw::chevron into fold_chevron(id, open, geometry, on_click) and use it from the dashboard band and view-header code.

3. "···" menu-trigger wrapper repeated about nine times
   Copies: dashboard/header.rs:417-447 (options_trigger), 875-915 (dashboard_options_trigger), 1271-1303 (view_options_trigger); lists/view.rs:1564-1588 (options_trigger), 2218-2254 (selection_more); dashboard/bulk.rs:179-201; sidebar/mod.rs:804-851 (group_actions, `more` at about 828-846), 950-998 (item_trailer); pane/mod.rs:1049-1076 (more_trigger).
   Duplicated: GlyphButton "···" at 13px, muted colour, selected(open), tooltip only while closed, Popover only while open, inside a relative flex_none wrapper.
   Suggest: a MenuTrigger in ic-ui-kit (next to Popover) that takes the glyph, open state, tooltip and menu.

4. Actions gated by action_denial written in five or more places
   Copies: dashboard/bulk.rs:161-176 (bulk_button, denial at 169) and 204-216 (menu item closure, denial at 208); pane/mod.rs:880-898 (action_buttons closure, denial at 886) and 1104-1121 (more_menu item closure, denial at 1108); pane/downtime.rs:93-119 (remove_button, match at 106-117); pane/thread.rs:146-184 (remove_button, match at 176-183).
   Duplicated: the denied case becomes disabled with the denial as tooltip; the allowed case gets on_click that calls request(action), often with menus.close() first.
   Suggest: one helper, action_button(state, id, label, action) and action_item(...), shared by bulk and pane.

5. Clipboard copy with "Copied X" notice repeated
   Copies: lists/view.rs:1215-1221 (copy method); pane/mod.rs:544-550 (copy method); workspace.rs:1637-1643 (copy method); dashboard/bulk.rs:216-224 (copy closure); pane/mod.rs:1119-1126 (copy closure). "copy filter expression" item: dashboard/header.rs:496-503, lists/view.rs:1647-1654, lists/view.rs:2242-2249, dashboard/bulk.rs:255-260, pane/mod.rs:1171-1176.
   Duplicated: write_to_clipboard, then inform("Copied {what}"), then notify; the copy-filter item is written five times.
   Suggest: one copy_item(id, label, what, text) builder and one AppState copy-with-notice helper.

6. Dashboard options trigger duplicates options_trigger; "edit dashboard" item written four times
   Copies: dashboard/header.rs:875-915 (dashboard_options_trigger, its menu holds only the edit item at 882-905) vs header.rs:417-447 (options_trigger); edit item also at header.rs:485-496 (view_menu), lists/view.rs:1604-1613 (options_menu), sidebar/menus.rs:277-282 (emit_item variant).
   Duplicated: the same trigger, and the same edit-dashboard item in four menus.
   Suggest: a single edit_dashboard_item(reference, cx) and one trigger that takes its menu as a parameter.

7. Summary-to-state tallies repeated in five places
   Copies: dashboard/header.rs:1444-1462 (problem_dots), 1531-1576 (summary_items); dashboard/view_controls.rs:65-148 (count_slots); dashboard/draw.rs:1457-1485 (band_counts), 1495-1528 (tile_parts).
   Duplicated: the Summary to (CheckableState, count[, word]) mapping, including the critical+down, unknown+unreachable and pending/ok folds, each with its own ordering and zero filter.
   Suggest: one ordered Summary::states(kind) (in ic-core or ic-app) that every caller filters or labels.

8. thread_chip reimplements ic_ui_kit::Chip; other chips are hand-rolled
   Copies: lists/view.rs:2485-2542 (thread_chip); dashboard/header.rs:919-950 (filter_chip); dashboard/header.rs:731-772 (summary chip inside render_summary, 636-792); dashboard/header.rs:1585-1600 (demo_chip). Kit: ic-ui-kit/src/components/form.rs:493-650 (Chip with leading, selected, filled, marked, CHIP_HEIGHT, px 8, hover and border).
   Duplicated: height, padding, radius, border, selected accent, text colour and hover, plus a leading mark, all written out again.
   Suggest: give Chip a min_width and a trailing slot, then use it for thread_chip and filter_chip.

9. Chip label and width logic repeated across render_chips, chips_width and the thread chips
   Copies: lists/view.rs:1670-1745 (render_chips: words, `px(11. + 6.)` mark, wide and narrow); dashboard/view_controls.rs:161-182 (chips_width: same sums); dashboard/view_controls.rs:323-369 (render_thread_chips, `word` at 339-344); lists/view.rs:2485-2542 (thread_chip, width from ic_ui_kit::chip_width and `px(11. + 6.)` at 2503).
   Duplicated: "{count} {label}" text, the mark width (11 + 6), and the narrow-versus-wide choice.
   Suggest: one chip_text(count, label) and one chips_width(), both used by lists and dashboard.

10. Picked-state chip toggle duplicated (summary chips vs count slots)
   Copies: dashboard/header.rs:731-772 (picked = chip.is_some() && view.state == chip; next = if picked None else chip; tooltip "Showing only {label} · click to show all" at 765); dashboard/view_controls.rs:257-319 (count_slot: same picked logic, same toggle, tooltip at 302-304).
   Duplicated: state-filter toggle, tooltip wording, stop_propagation and mouse-down prevention.
   Suggest: a shared state_filter_toggle(view, chip, label) helper.

11. Sort menu built twice; the list sort trigger hand-rolls the header sort word
   Copies: dashboard/view_controls.rs:433-465 (thread_sort_menu, "sort by" at 441); lists/view.rs:1522-1562 (sort_trigger, inline loop over kind.sorts() with checked, "sort by" at 1547); dashboard/header.rs:1309-1348 (sort_word, the shared pressed-look trigger that lists does not use).
   Duplicated: the same loop of MenuItem::checked over ListKind::sorts(), and a text trigger that duplicates sort_word.
   Suggest: build the sort menu once from ListKind and use sort_word in the list header.

12. timeline | list mode switch built twice
   Copies: dashboard/view_controls.rs:373-408 (render_mode_switch); lists/view.rs:1443-1478 (inline in header controls).
   Duplicated: Segmented options, the Mode to index mapping and back, and the same selected value.
   Suggest: one mode_switch(id, mode, compact, on_pick) builder used by both.

13. threads::Folds toggles duplicated in dashboard and lists; Folds has no impl block
   Copies: dashboard/mod.rs:1859-1882 (toggle_more, set_more); dashboard/mod.rs:1137-1148 and 1270-1276 (inline open toggle); dashboard/mod.rs:1262-1268 and 1690-1694 (inline collapsed toggle); lists/view.rs:1092-1097 (toggle_band), 1100-1105 (toggle_fold), 1108-1122 (toggle_more). Struct: lists/threads.rs:357.
   Duplicated: `if !set.remove(x) { set.insert(x) }` written about eight times, and two toggle_more functions with the same meaning.
   Suggest: methods on threads::Folds (toggle_collapsed, toggle_open, toggle_more, set_more) and delete the copies.

14. Dashboard fold dispatch repeated in three functions
   Copies: dashboard/mod.rs:1095-1156 (activate), 1190-1291 (fold_at_cursor's match), 1660-1698 (click_chevron). Each re-derives "collapsed" with page.view_by_id(view).is_some_and(|v| v.collapsed) and .group(group).is_some_and(|g| g.collapsed).
   Duplicated: the Header, Band and Thread(Band|Fold|More) fold cases and the collapsed lookups.
   Suggest: a Page::is_collapsed(stop) lookup and one fold_stop(stop, open) dispatcher.

15. Host and fold paging computation repeated in three places
   Copies: lists/threads.rs:1089-1112 (fold paging: count problems, shown_count, take, pages, More line with hidden); pane/model.rs:266-286 (host_services: sort by service_order, count not_ok, shown_count, truncate, pages); dashboard/page.rs:1567-1581 (group paging: sort, count problems, shown_count, pages). Helpers in paging.rs (shown_count, pages, more_label, service_order) are already shared, but the orchestration is not.
   Duplicated: the sequence sort, count problems, shown_count, pages, hidden.
   Suggest: a paging::Plan::new(total, problems, expanded) returning shown, hidden and pages, plus one sort-and-count helper.

16. Paging row rendered three times with the same base style
   Copies: dashboard/draw.rs:541-611 (render_more, id "more:..."); lists/view.rs:2931-2946 (more_line); pane/host.rs:204-226 (the "more-ok" row).
   Duplicated: text_size small, faint colour, cursor pointer, hover to muted or text colour, border and label from paging::more_label.
   Suggest: a shared paging_row(label, emphasis) in lists/draw.rs.

17. Text-width formula reimplemented at least six times, despite ic-ui-kit helpers
   Copies: lists/draw.rs:110-112 (chars); dashboard/header.rs:1350-1357 (small_chars); dashboard/header.rs:1365-1369 (handled_slot_width, the same formula with a const); dashboard/view_controls.rs:191-194 (small_label_chars); dashboard/bulk.rs:118-120 (inline); sidebar/mod.rs:1142 (inline, no ceil). Also dashboard/header.rs:1064 and 1098 (inline). Kit already has the same formula in sub_tab_width (ic-ui-kit/src/components/header.rs:372-379), chip_width (form.rs:472-479), Button::width_for (button.rs:156-168), summary.rs:129 and list.rs:505, 737-740.
   Duplicated: `(size * (chars * CHAR_WIDTH)).ceil()`.
   Suggest: one ic_ui_kit::text_width(size, chars) -> Pixels, used by the kit and by the app.

18. Constants duplicated across modules
   Copies: lists/view.rs:75 (SELECTION_BAR_HEIGHT = 40.) vs dashboard/bulk.rs:24 (same, pub(crate)), which dashboard/mod.rs:32 re-exports and workspace.rs:2862 uses; lists/view.rs:77 (COUNT_SLOT_CHARS = 12.) vs dashboard/bulk.rs:27 (same).
   Duplicated: the same two constants, one of them already exported.
   Suggest: lists/view.rs imports dashboard::SELECTION_BAR_HEIGHT, or both move to a shared module.

19. Sticky band overlay duplicated; a redundant first condition in the lists version
   Copies: dashboard/draw.rs:110-152 (sticky_band, shadow at 142-148); lists/view.rs:1897-1932 (sticky_band, shadow at 1922-1928). Redundant branch: lists/view.rs:1908-1910 (`if band == top && layout.top(band) >= offset`) is fully covered by 1911-1913 (`if layout.top(band) >= offset`), so the first `if` can never change the result.
   Duplicated: the absolute top-left-right overlay with the same BoxShadow literal, and the same offset-check shape.
   Suggest: a shared sticky_overlay(element, height, cx) helper, and delete the redundant branch at lists/view.rs:1908-1910.

20. dismiss_listener written three times in scope (five across the app)
   Copies: dashboard/header.rs:619-627; lists/view.rs:1657-1664; sidebar/menus.rs:127-134. Outside scope, same body: editor/inspector.rs:1286-1294, settings/pages.rs:1560-1568. Also menu_state.rs:70-78 (OpenMenu::dismissed) is the method they all call.
   Duplicated: cx.listener that calls menus.dismissed(*dismissal) and cx.notify().
   Suggest: an OpenMenu-level listener helper in ic-app/src/menu_state.rs, parameterised by the menus accessor.

21. Empty-state variants duplicated
   Copies: "No permission" with Lock icon, detail and max_width 560: dashboard/mod.rs:1756-1765 and lists/view.rs:1787-1795. Filter error with TriangleAlert: dashboard/mod.rs:1773-1783 (width 560) and 2106-2115 (width 520, different title). "No environment yet": dashboard/mod.rs:2358 (no_environment) and lists/view.rs:1781-1785.
   Duplicated: the same EmptyState builder shapes, differing only in text and width.
   Suggest: EmptyState::denied(denial), EmptyState::filter_error(title, error, width) and a shared no_environment().

22. Pluralisation helpers written five times in scope
   Copies: lists/view.rs:3130-3132 (plural); lists/removal.rs:109-116 (counted method); lists/removal.rs:146-148 and 419-421 (two identical inline `plural` closures in the same file); dashboard/rows.rs:119-126 (count_label). Out of scope, same rule: editor/model.rs:371-373 (counted), app_state/connection.rs:693 (counted).
   Duplicated: format!("{count} {}", if count == 1 { one } else { many }).
   Suggest: one plural(count, one, many) in a shared util module, used everywhere.

23. Hover-reveal pattern repeated
   Copies: sidebar/mod.rs:720-731 (reveal closure); sidebar/mod.rs:966-983 (item_trailer: invisible, group_hover); pane/mod.rs:1343-1363 (copy_button: invisible, group_hover visible).
   Duplicated: invisible().group_hover(group, visible) to show trailing icons on hover.
   Suggest: an ic-ui-kit reveal_on_hover(element, group, always) helper.

24. Empty-text helpers and literals repeated inside dashboard/mod.rs
   Copies: "No events yet: they show here as they happen." literal at dashboard/mod.rs:116 (nothing_to_show) and 1745 (inline). Plain text helpers: dashboard/mod.rs:2373-2383 (note, centred muted text) and sidebar/mod.rs:1390-1426 (empty_note); dashboard/view_controls.rs:236 uses lowercase "nothing to show" beside mod.rs:117 "Nothing to show.".
   Duplicated: the same empty copy written twice, and three separate empty-text components with no shared style.
   Suggest: call nothing_to_show(EventStream) at 1745 (or use a const), and fold note and empty_note into the kit EmptyState or one shared helper.

DEAD CODE
- No unused production functions in scope. Every non-test fn in dashboard, lists, pane and sidebar is referenced elsewhere in the crate. The only unreferenced-looking functions are UI-test accessors under #[cfg(all(test, target_os = "linux"))] (for example dashboard/mod.rs:2385 onward, lists/view.rs:3143 onward), so they are test-only.
- One dead branch: lists/view.rs:1908-1910 (see entry 19).
- No TODO, FIXME or #[allow(dead_code)] markers in scope.

KIT OBSERVATIONS
- ic-ui-kit already exposes ListRow, CompactRow, EmptyState, Chip, PaneHeader, SubTabs, Menu, Popover, SummaryBar and Segmented; these are used. The gaps are the items above (chip with trailing slot and min width, menu trigger, text_width, reveal_on_hover, and EmptyState presets).
- Two different Tone enums exist (lists/words.rs:30 and app_state/connection.rs:38) and two detail_line fns (sidebar/menus.rs:767 and sidebar/centre.rs:1009). They look similar by name but do different things, so they are not duplicates.