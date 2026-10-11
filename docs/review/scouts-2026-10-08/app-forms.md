FINDINGS: 25. TOP 3: (1) Platform shortcut labels hand-written in five files. (2) Dropdown trigger built twice (settings and dashboard inspector). (3) ObjectAction matched by six parallel tables.

PATHS: All paths below are absolute. Abbreviations: APP = /home/claude/icygui/crates/ic-app/src, KIT = /home/claude/icygui/crates/ic-ui-kit/src.

1. Platform shortcut labels hand-written in five files, not derived from the keymap.
Copies: APP/editor/mod.rs:1190-1205 (undo_hint, save_key); APP/settings/mod.rs:143-167 (settings_key, quit_key, navbar_key); APP/palette/model.rs:496-500 (ALL_MATCHES_KEY) and 952-956 (Toggle sidebar, ⌘B); APP/operate/dialog.rs:1894-1898 (secondary_enter, same value as ALL_MATCHES_KEY); APP/editor/inspector.rs:1368-1393 (reorder_hint, move_hint, remove_hint). APP/keymap.rs:250-270 already formats keystrokes.
Duplicated: each label is a separate cfg!(target_os = "macos") if/else; the dialog repeats the palette constant verbatim.
Suggestion: derive every label from the keystroke string with one helper (keymap's formatter), and have the dialog reuse ALL_MATCHES_KEY.

2. Dropdown trigger built twice.
Copies: APP/settings/pages.rs:1499-1558 (dropdown); APP/editor/inspector.rs:1213-1263 (dropdown).
Duplicated: the same trigger box (field_height, code_radius, accent border when open, code_background, ChevronDown, mouse-down prevent_default, click toggles menu, Popover while open). Settings adds a fixed width and focus step; the inspector adds an optional icon.
Suggestion: one ui-kit Dropdown trigger (value, icon, open, width) with a menu slot; callers pass their Menu and focus step.

3. ObjectAction matched by six parallel tables, with repeated wording.
Copies: APP/operate/tracker.rs:177-185 (named_count), 187-244 (verbs), 247-260 (mark_label); APP/palette/model.rs:474-481 (action_key), 641-659 (action_label); APP/operate/dialog.rs:172-205 (for_action, for_review, action), 207-217 (title), 219-229 (submit_label).
Duplicated: the same 11 variants matched 5-6 times. "Acknowledge", "Schedule downtime", "Check now", "Add comment", "Run command", "Submit check result", "Remove downtime" appear as strings in palette/model.rs:641-659 and dialog.rs:207-217. The named-removal branches repeat in tracker.rs:177-185 and palette/model.rs:647-653.
Suggestion: put label, progressive, past, infinitive, mark label and key on ObjectAction (or one table keyed by kind), so a new kind changes one match.

4. "N noun(s)" pluralization written inline about 15 times.
Copies: APP/editor/model.rs:371-373 (counted); APP/palette/model.rs:464-472 (count_detail); APP/palette/mod.rs:353; APP/settings/files.rs:76-82 (LogSummary::text); APP/settings/pages.rs:247, 2087; APP/settings/mod.rs:1294, 1459; APP/operate/dialog.rs:1523-1524, 1627, 1651-1652, 2127; APP/lists/removal.rs:109-115 (counted; same helper as editor/model.rs, outside scope).
Duplicated: `if count == 1 { one } else { many }` next to the formatted count.
Suggestion: one plural(count, one, many) helper in the format module, used at every site.

5. dismiss_listener repeated per menu owner, though MenuState already has dismissed().
Copies: APP/settings/pages.rs:1560-1567; APP/editor/inspector.rs:1286-1293. Same name also at APP/dashboard/header.rs:619, APP/lists/view.rs:1657, APP/sidebar/menus.rs:127 (grep hits only, not read).
Duplicated: identical body `this.menus.dismissed(*dismissal); cx.notify()`. The generic state already exists at APP/menu_state.rs:70 (dismissed).
Suggestion: a generic dismiss_listener in menu_state.rs that takes an accessor to the owner's MenuState; delete the per-module copies.

6. Display-section menu built twice.
Copies: APP/editor/inspector.rs:445-471 (add_view_menu); APP/editor/inspector.rs:720-749 (display_field's build closure).
Duplicated: the same loop over model::DISPLAY_SECTIONS with separators, section labels, and MenuItems with display_icon and display_name. Only the id prefix, the selected flag and the click action differ.
Suggestion: one display_menu(id_prefix, current: Option<ViewDisplay>, on_pick) helper.

7. Pane headers hand-built although ui-kit has PaneHeader.
Copies: APP/editor/mod.rs:1036-1122 (render_header; height at 1080); APP/settings/mod.rs:1280-1450 (render_header; height at 1428); APP/editor/inspector.rs:85-133 (render_inspector; header at 94-106, title at 99-105 with heading size and FontWeight::MEDIUM).
Duplicated: height, bottom rule, padding, title and subtitle styling. KIT/components/header.rs:31-100 (PaneHeader: title, subtitle, leading, status, on_close, from grep signatures) is already used at APP/dashboard/header.rs:116, APP/lists/view.rs:1349, APP/pane/mod.rs:614, APP/cluster.rs:175, APP/recovery.rs:83.
Suggestion: render these three headers with PaneHeader; extend it only if they need a slot it lacks.

8. Text inputs with a problem re-wrapped in four helpers.
Copies: APP/environments/editor.rs:717-738 (text_field), 741-773 (path_field), 733-737 and 768-772 (the same hint match); APP/operate/dialog.rs:1053-1068 (text_field); APP/settings/pages.rs:1348-1366 (field, bordered TextField without Field). Bare `TextField::new(..).bordered(true).invalid(..)` also at APP/editor/inspector.rs:159, 183, 848 and APP/editor/mark.rs:296, 515.
Duplicated: `.bordered(true).invalid(error.is_some())` plus `Field::new(label).control(..).error(error)` plus `match hint { Some(h) => f.hint(h), None => f }`.
Suggestion: ui-kit Field::text(label, input, error); let Field::hint take an Option.

9. Certificate trust button written twice.
Copies: APP/environments/certificate.rs:249-290 (with_buttons: "trust the new certificate" / "trust this certificate", Danger or Primary); APP/environments/editor.rs:1217-1274 (certificate_offer: "trust this certificate", primary, next to certificate_details).
Duplicated: the trust button and its label and variant rules.
Suggestion: a shared trust_button(mismatch, on_click) helper, used by both.

10. Cancel-with-Esc dialog button repeated.
Copies: APP/environments/certificate.rs:264-267; APP/environments/editor.rs:1421-1426; APP/operate/dialog.rs:1912-1916. (APP/editor/mod.rs:1111 uses key_hint("esc") on a header control, not a cancel.)
Duplicated: Button::new(id, "cancel").key_hint("esc").on_click(emit close). Only the id and the event differ.
Suggestion: DialogBody::cancel(id, on_cancel) in KIT/components/modal.rs (DialogBody API at lines 164-197), so the Esc hint lives in one place.

11. PaletteItem literal written nine times; setting() and command() nearly identical.
Copies: APP/palette/model.rs:386, 423, 618-630 (setting), 680, 719-728 (host_item), 758-769 (service_item), 851-862 (target_item), 874, 921-933 (command).
Duplicated: the struct literal with the same defaults (dot None, several false, matched Vec::new(), denied None). setting() and command() differ only in section (Environments vs Commands) and key_hint.
Suggestion: PaletteItem::new(section, label, command) with builder methods; setting() and command() then collapse into one.

12. Index-list edits duplicated for URLs and views.
Copies: APP/environments/form.rs:198-235 (add_url, remove_url, move_url); APP/environments/editor.rs:334-356 (remove_url, move_url wrappers); APP/editor/model.rs:198-249 (insert_view, duplicate_view, remove_view, move_view); APP/editor/mod.rs:668-688 (add_view, move_view, remove_view wrappers).
Duplicated: bounds check, keep at least one element (or cap at MAX), then remove, insert or swap.
Suggestion: two generic helpers (move_within(list, from, to), remove_keeping_one(list, index)) used by all four sites.

13. Click-forwarding listener repeated about ten times in settings.
Copies: APP/settings/pages.rs:1109, 1250-1255, 1284-1295, 1335-1338, 1380-1395, 1414-1420, 1454-1461, 1896, 2029-2037; APP/settings/mod.rs:1377-1384, 1408-1416.
Duplicated: `let click = x.clone();` followed by cx.listener(move |this, _: &ClickEvent, window, cx| click(this, window, cx)), then the same action passed to focusable().
Suggestion: Activate exposes a method that returns the listener, or focusable() builds the click handler from the Activate itself.

14. Pause chip rows copied in three places.
Copies: APP/settings/pages.rs:1403-1426 (pause_control, None branch); APP/sidebar/centre.rs:767-781 (pause chips); APP/sidebar/menus.rs:740-758 (per-environment mute chips; partly read).
Duplicated: Chip::new(id, choice.short_label()) with on_click pausing until choice.until(now). Settings and centre are the same shape; menus differs by scope.
Suggestion: one builder in the notifications module, pause_chips(choices, id_prefix, on_pick), used by all three.

15. Dot colour mapping and 7px dot duplicated.
Copies: APP/settings/pages.rs:374-394 (summary_color) and 396-405 (mark_dot, 7px in a 14px slot); APP/palette/mod.rs:475-489 (dot_of, dot_color, 7px).
Duplicated: the state-to-colour fallback (checkable state, ok, pending) appears in summary_color and dot_color; the 7px StateDot construction appears in mark_dot and dot_of.
Suggestion: express summary_color as a Dot-style value, and add a small-dot constructor or a Dot-to-colour helper to ui-kit StateDot.

16. Highlighted-match text builders duplicated.
Copies: APP/palette/mod.rs:459-473 (highlighted, by char index); APP/settings/rows.rs:77-89 (marked, by byte range from match_ranges).
Duplicated: both build StyledText with HighlightStyle { color: accent_text } over a list of ranges.
Suggestion: a ui-kit helper that takes ranges; the palette converts its char indices to byte ranges.

17. Settings rows re-create ui-kit text styles.
Copies: APP/settings/rows.rs:44-75 (section_label) vs KIT/components/text.rs:14-38 (SectionLabel: label size, faint, nowrap); APP/settings/rows.rs:269-276 (title: heading size, MEDIUM, text_strong) vs KIT/components/modal.rs:232 and header.rs:159; APP/settings/rows.rs:249-266 (key_cap, framed chip-height key) vs KIT/components/button.rs:22-52 (KeyHint, unframed).
Duplicated: the same text styles. key_cap is a second key-hint style that the kit does not have.
Suggestion: build section_label on SectionLabel (it only adds heading, note and marked); add a title helper and a framed KeyCap to the kit beside KeyHint.

18. expand_home and tilde duplicated.
Copies: APP/environments/form.rs:318-331 (expand_home: trims, handles a lone "~"); APP/workspace.rs:2987-2994 (expand_home: no trim, no lone "~"); APP/settings/files.rs:30-42 (tilde, the inverse).
Duplicated: the HOME lookup and "~/" handling in two modules, with different behaviour.
Suggestion: one paths helper pair (expand and tildify) in ic-config; keep the stricter environments version.

19. note() identical in two modules.
Copies: APP/editor/mod.rs:1207-1219; APP/dashboard/mod.rs:2372-2384.
Duplicated: centred muted line with the same body. The editor adds px(24) horizontal padding.
Suggestion: one shared helper with a padding parameter.

20. Two parse_clock functions with different contracts.
Copies: APP/settings/model.rs:546-559 (parse_clock to minutes as u16; accepts "22:00", "7", "07.30") and 562-564 (format_clock); APP/operate/when.rs:341-365 (parse_clock to NaiveTime; ":" only, two-digit parts).
Duplicated: same name and job, different accepted separators and bare-hour rules.
Suggestion: one clock parser returning hour and minute, shared by settings and when; decide whether "07.30" is intended.

21. Toast and banner tone enums mapped one to one.
Copies: APP/operate/toasts.rs:15-23 (tone: tracker::ToastTone to ic_ui_kit::ToastTone); APP/operate/tracker.rs:43-56 (ToastTone: Pending, Success, Partial, Failed, Info); KIT/components/toast.rs:25-33 (Pending, Success, Warning, Critical, Info). Outside scope: APP/banner.rs:108-113 (tone: Tone to BannerTone, one to one).
Duplicated: a second tone enum whose only difference is naming.
Suggestion: have tracker use the kit's ToastTone directly (Partial maps to Warning, Failed to Critical).

22. Choice menus rebuilt by hand.
Copies: APP/settings/pages.rs:1571-1590 (environment_menu), 1594-1616 (log_level_menu); APP/editor/inspector.rs:1112-1141 (sort_menu), 1265-1284 (group_menu), 445-471 (add view).
Duplicated: Menu::new(id).min_width(..), a loop of MenuItem::new(ElementId::Name(format!(..)), label).checked or selected, on_click closes the menu and acts, then on_dismiss.
Suggestion: choice_menu(id, width, choices, current, on_pick) returning Menu; the owner attaches dismiss.

23. Bounded number parsers repeat one shape.
Copies: APP/settings/model.rs:471-483 (parse_retention), 490-500 (parse_reconcile), 572-581 (parse_threshold), 588-594 (parse_window).
Duplicated: trim, parse, check min and max, return a message. Messages are inline.
Suggestion: parse_bounded(text, range, messages) helper.

24. Duration formatting: a third formatter (low priority).
Copies: APP/settings/model.rs:521-538 (format_min_duration, "1h30m", "90s"); /home/claude/icygui/crates/ic-model/src/time.rs:66-75 (format_compact) and 79-94 (format_two_units, "2m 5s").
Duplicated: a duration-to-text formatter. The settings one is a compact form meant to round-trip with parse_duration, so it may be intentional.
Suggestion: keep it, but document the round-trip requirement, or build it from ic-model's parts.

25. Name collision: two enums named FormField (low priority).
Copies: APP/environments/form.rs:12 and APP/operate/forms.rs:22.
Duplicated: same name, different meaning. I did not see a file that imports both.
Suggestion: rename to EnvironmentField and ActionField before they meet in one file.

DEAD CODE: none found in production code. Method: a reference count over the workspace, excluding ui_tests/ and #[cfg(test)]-gated items (brace-matched), found no non-test function or constant referenced only by its definition. Earlier candidates (editor/mod.rs accessors, operate/dialog.rs listed_objects/box_geometry/etc., settings set_locations/search_counts, environments add_url_row/move_url_row/remove_url_row) are all inside #[cfg(all(test, target_os = "linux"))] blocks, so I excluded them. start_elsewhere and dismiss_failure are used from APP/app_state/operations.rs.

COVERAGE GAPS (not read in full): APP/environments/editor.rs:1137-1355 (render_test, failure_view); APP/palette/model.rs candidate builders around 380-460 and 660-1000 (partial); APP/operate/tracker.rs:490-607 (finish); APP/settings/pages.rs:412-870 and 1617-2220 (render_section, render_search, render_item, keymap). Cross-scope copies at dashboard/header.rs:619, lists/view.rs:1657, sidebar/menus.rs:127 and lists/removal.rs:109 were read by grep or partly, not in full.