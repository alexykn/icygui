Duplication scout report (ui-kit). Count: 20 findings. Top 3: (1) count chips hand-built twice instead of a kit chip; (2) dropdown trigger re-implemented in two app modules, no kit Select; (3) menu plumbing copied across five app modules.

1. Clickable state-count chips built twice in app, not from kit Chip/SummaryItem
- crates/ic-app/src/dashboard/header.rs:735-790 (summary-chip-{label}: picked accent border/tint, tooltip "Show only X" / "Showing only X · click to show all", change_view toggle)
- crates/ic-app/src/dashboard/view_controls.rs:257-321 (count_slot: same picked styling, same tooltip wording, same toggle)
What: both draw a 20-24px pill with a StateDot and count, accent border/tint when picked, and click-to-filter. Neither uses kit Chip (which has leading/selected/filled) and the tooltip strings are duplicated.
Suggest: one shared count-chip helper (kit Chip with leading(StateDot), or a kit CountChip) used by both sites.

2. Dropdown trigger re-implemented in two app modules; kit has no Select
- crates/ic-app/src/editor/inspector.rs:1211-1263 (fn dropdown)
- crates/ic-app/src/settings/pages.rs:1492-1557 (fn dropdown, documented as "the dashboard editor's style"; adds focusable wrapper)
What: both build the same framed trigger (field_height, px10, code_radius, border accent when open else border_header, code_background, label, ChevronDown) and wrap a Popover when open.
Suggest: kit Select/DropdownTrigger taking value, icon, open, and a toggle handler that receives the press position. Also share the frame used by kit TextArea (form.rs:685-706) and bordered TextField (text_field.rs:78-89) via one field_frame helper.

3. Menu plumbing copied across five app modules (dismiss_listener x5, down_position x2, sort-by menus x4)
- dismiss_listener, identical body (this.menus.dismissed(*d); cx.notify()): crates/ic-app/src/editor/inspector.rs:1286-1293, crates/ic-app/src/settings/pages.rs:1560-1567, crates/ic-app/src/lists/view.rs:1657-1664, crates/ic-app/src/sidebar/menus.rs:127-134, crates/ic-app/src/dashboard/header.rs:619-626
- down_position, identical body: crates/ic-app/src/menu_state.rs:86-92 (pub(crate)) and crates/ic-app/src/dashboard/header.rs:98-104 (private copy)
- sort-by menus (loop over sorts, MenuItem checked + on_click): crates/ic-app/src/dashboard/header.rs:349-416 (sort_menu), crates/ic-app/src/lists/view.rs:1547-1560, crates/ic-app/src/dashboard/view_controls.rs:441-464, crates/ic-app/src/editor/inspector.rs:1112-1140
What: the same helpers are copied per view; menu_state.rs already holds the shared menu state.
Suggest: move dismiss_listener onto OpenMenu in menu_state.rs (generic over the view), delete the header.rs down_position copy, and add one choice-menu helper for the sort menus.

4. "Mark or pending" StateDot match copied six times
- crates/ic-app/src/lists/view.rs:2656-2657, 2885-2886, 3022-3023
- crates/ic-app/src/lists/dialog.rs:189-190
- crates/ic-app/src/operate/dialog.rs:1246-1247
What: match facts.mark { Some(m) => StateDot::mark(m).size(S), None => StateDot::with_color(theme.states.fill.pending).size(S) }, differing only in S (7 or 9).
Suggest: kit StateDot::mark_or_pending(Option<ObjectMark>, theme) so the grey "pending" rule lives in the kit with mark().

5. Dot-to-StateDot mapping duplicated in two app modules
- crates/ic-app/src/sidebar/mod.rs:1378-1387 (fn dot over sidebar::model::Dot)
- crates/ic-app/src/palette/mod.rs:475-489 (dot_of plus dot_color over the same Dot)
- related: crates/ic-app/src/cluster.rs:322-329 (node_dot, same ok/critical/pending palette)
What: Dot::{State, Handled, Ok, Empty} is mapped to StateDot and colours in two places, with palette re-deriving colour and hollowness separately.
Suggest: impl Dot { fn state_dot(self, theme) -> StateDot } in sidebar/model.rs, used by sidebar and palette (palette passes size 7).

6. Progress bar re-drawn; kit ProgressBar has no colour or width option
- crates/ic-ui-kit/src/components/banner.rs:261-276 (ProgressBar::render, height only, fixed accent)
- crates/ic-ui-kit/src/components/banner.rs:536-547 (PaneBanner's bottom track: same 2px track and accent fill with relative(fraction), inline)
- crates/ic-app/src/lists/draw.rs:359-368 (fn progress(fraction, color, theme): 3px rounded track with a colour parameter)
What: the kit draws its own progress track in two places, and the app adds a fourth copy because ProgressBar cannot take a colour or width.
Suggest: give ProgressBar .color() and .width(), have PaneBanner call it, and replace lists/draw.rs::progress.

7. Row emphasis painting re-implemented for app list rows
- crates/ic-app/src/lists/view.rs:2546-2564 (fn emphasised: bg from RowEmphasis::background, hover when None, MARK_WIDTH accent bar when marked)
- crates/ic-ui-kit/src/components/list.rs:411-425 (ListRow: same three rules; MARK_WIDTH at list.rs:21, app copy in lists/draw.rs)
What: the handling/downtimes rows cannot use ListRow, so they copy its emphasis painting.
Suggest: expose RowEmphasis::paint(Div) -> Div (or a ListRow-free wrapper) in the kit and call it from lists/view.rs.

8. Unread count badge hand-built; kit has no badge component
- crates/ic-app/src/sidebar/mod.rs:1196-1214 (absolute 12px pill, accent fill, on_accent text, 8.5px semibold, text from notifications::entry::badge at entry.rs:502)
What: a count pill is built inline; the kit has no Badge.
Suggest: kit CountBadge::new(text) with placement left to the caller.

9. Summary-bar fit and compact decision computed twice in the dashboard header
- crates/ic-app/src/dashboard/header.rs:694-705 (texts from SummaryItem::text, then SummaryBar::fits, then compact)
- crates/ic-app/src/dashboard/header.rs:796-824 (fn counts_bar: the same map, fits and compact, then SummaryItem::compact)
What: the same fitting logic is repeated in both bars; the kit exposes fits() but leaves the caller to repeat the sequence.
Suggest: SummaryBar takes the items and width and decides compact itself, or add one fitted(items, end, width, theme) helper.

10. Dead kit API: NoteEntry (whole component)
- crates/ic-ui-kit/src/components/notice.rs:15-132 (struct, impl with header_text at 58-66, Debug, render), exported via crates/ic-ui-kit/src/components/mod.rs and crates/ic-ui-kit/src/lib.rs
What: no reference outside the kit (grep on NoteEntry across crates). The pane's comment, acknowledgement and downtime lines are drawn by the app's lists/draw.rs::entry, called from crates/ic-app/src/pane/thread.rs:~90-137.
Suggest: delete NoteEntry, or move pane/thread.rs onto it if its look matches draw::entry's author/time header.

11. Dead kit API: SubTabs::marked_tab (and Tab.marked), sub_tab_width, sub_tab_gap
- crates/ic-ui-kit/src/components/header.rs:189-196 (Tab.marked field), 242-253 (marked_tab), 367-385 (sub_tab_width, sub_tab_gap)
- exports: crates/ic-ui-kit/src/components/mod.rs:27 and crates/ic-ui-kit/src/lib.rs
What: no callers outside the kit. The only SubTabs user is crates/ic-app/src/pane/host.rs:41-46, which uses neither helper. crates/ic-ui-kit/src/components/menu.rs:251-252 cites marked_tab in a doc comment only.
Suggest: delete all three, or keep sub_tab_width only if an overflow menu is about to land.

12. Text-width formula duplicated in kit and app
- Kit: crates/ic-ui-kit/src/components/form.rs:472-479 (chip_width), crates/ic-ui-kit/src/components/header.rs:371-379 (sub_tab_width), crates/ic-ui-kit/src/components/button.rs:156-168 (width_for), crates/ic-ui-kit/src/components/summary.rs:125-129 (fits closure), crates/ic-ui-kit/src/components/list.rs:502-505 (time slot)
- App: crates/ic-app/src/lists/draw.rs:109-112 (chars), crates/ic-app/src/dashboard/header.rs:1350-1355 (small_chars), crates/ic-app/src/dashboard/header.rs:1364-1368 (handled_slot_width), crates/ic-app/src/dashboard/view_controls.rs:189-193 (small_label_chars), crates/ic-app/src/dashboard/header.rs:1064 and 1098, crates/ic-app/src/dashboard/bulk.rs:118, crates/ic-app/src/sidebar/mod.rs:1142
What: every site computes size * (chars * CHAR_WIDTH) with the same clippy::cast_precision_loss expect boilerplate.
Suggest: one pub fn text_width(size: Pixels, chars: usize) in the kit, used by all twelve sites.

13. Header bars re-implemented beside PaneHeader
- crates/ic-app/src/editor/mod.rs:1074-1095 (editor header: header_height bar, border_b, px list_padding, heading title, accent subtitle)
- crates/ic-app/src/settings/mod.rs:1423-1441 (settings page header: same bar with title row and faint subtitle)
- crates/ic-ui-kit/src/components/modal.rs:221-235 (DialogBody title row: same bar, heading MEDIUM text_strong)
- versus crates/ic-ui-kit/src/components/header.rs:126-186 (PaneHeader::render, which already does this)
Suggest: give PaneHeader a subtitle colour option and use it from DialogBody and from the editor and settings headers.

14. KvTable and TreeTable rows duplicated in the kit
- crates/ic-ui-kit/src/components/table.rs:99-123 (KvTable rows)
- crates/ic-ui-kit/src/components/notice.rs:329-359 (TreeTable rows)
What: both draw a flex row with border_t_1 border_row, a fixed-width truncated muted key column and a flex_1 value column; TreeTable adds indent and a summary colour.
Suggest: a shared kv_row(key, value, key_width, indent) helper.

15. CompactRow duplicates ListRow's compact layout
- crates/ic-ui-kit/src/components/list.rs:588-643 (CompactRow::render)
- crates/ic-ui-kit/src/components/list.rs:434-451 and 604-627 (the title/detail column, with detail hidden when compact, in both ListRow and CompactRow)
- only app user: crates/ic-app/src/pane/host.rs:183
Suggest: express the host pane's rows as ListRow::density(Compact) with leading state circle and trailing time, then delete CompactRow; or document the 22px-column difference.

16. Tone helpers duplicated across kit and app
- 2px tone bar drawn inline: crates/ic-ui-kit/src/components/banner.rs:162-170, banner.rs:526-534, crates/ic-ui-kit/src/components/toast.rs:180-188, list.rs:415-425; app copies at crates/ic-app/src/dashboard/header.rs:1201-1212 (focused header bar) and lists/view.rs:2553-2562 (emphasised)
- tone-to-colour maps: crates/ic-ui-kit/src/components/banner.rs:40-46 (BannerTone::color) and toast.rs:42-49 (ToastTone::color), same Critical/Warning/accent mapping, with ToastTone adding Success
Suggest: one pub(crate) tone_bar(color) helper, and a single tone-to-colour function or a shared Tone type.

17. Press, stop-propagation and tooltip boilerplate repeated across the kit (about 20 copies)
- prevent_default on mouse down: menu.rs:135, 394; button.rs:283, 423, 569; form.rs:282, 452, 641; link.rs:130; header.rs:347
- stop_propagation then handler wrapper: menu.rs:139-142, 410-415; button.rs:287-293, 427-433, 573-578; form.rs:283-288, 453-458, 642-647; header.rs:348-353; link.rs:134-139
- .when_some(tooltip, |x, t| x.tooltip(t.builder())): menu.rs:136-138, 407-409; button.rs:284-286, 424-426, 570-572; link.rs:131-133
Suggest: an extension trait (for example Pressable::on_press and an optional-tooltip builder) so the propagation rule lives in one place.

18. Host tooltip re-implements the kit Tooltip card in the app
- crates/ic-app/src/dashboard/draw.rs:1727-1828 (HostTooltip: card with border_window, element_background, shadow (0,2)/8, small_radius, text.label, a multi-line detail, and a pt(18)/pl(4) wrapper); used at draw.rs:889-891 and 983-985
- crates/ic-ui-kit/src/components/tooltip.rs:59-91 (the same card and wrapper, but a single title plus key line)
Suggest: let kit Tooltip take extra body lines or an element (Tooltip::body) so host tooltips reuse the card; keep the cursor offset in one place.

19. Dead or test-only kit API (zero callers outside the kit)
- IconButton::hover_color: crates/ic-ui-kit/src/components/button.rs:349-353
- GlyphButton::hover_color: crates/ic-ui-kit/src/components/button.rs:501-505
- SummaryItem::with_color: crates/ic-ui-kit/src/components/summary.rs:43-51 (no callers at all, tests included)
- ListRow::after_title: crates/ic-ui-kit/src/components/list.rs:272-277 (only the kit's own render reads it)
- DividerColor::Row: crates/ic-ui-kit/src/components/divider.rs:18-19
- contrast_ratio (exported at crates/ic-ui-kit/src/lib.rs:39): crates/ic-ui-kit/src/theme.rs:922-923, used only by kit tests
- PerfdataRow (exported; struct at crates/ic-ui-kit/src/components/table.rs:127-163): no outside use
- test-only accessors: Field::error_text (crates/ic-ui-kit/src/components/form.rs:103-107), Switch::is_on (form.rs:213-217), Segmented::options (form.rs:374-378)
Suggest: delete the unused builders and variant; make contrast_ratio pub(crate) or keep it for theme checks; move the test-only getters under cfg(test).

20. Hand-built pills instead of Chip or a key-cap component (also 1px rules hand-drawn)
- crates/ic-app/src/dashboard/header.rs:919-949 (filter_chip: h22 px8 rounded pill with a clear button; kit CHIP_HEIGHT is at form.rs:466)
- crates/ic-app/src/dashboard/header.rs:1585-1598 (demo_chip: framed accent pill)
- crates/ic-app/src/settings/rows.rs:249-268 (key_cap: CHIP_HEIGHT framed key, same border and fill as Chip)
- hand-drawn 1px rules where Divider exists: crates/ic-app/src/editor/inspector.rs:176, crates/ic-app/src/controls.rs:129, crates/ic-app/src/settings/mod.rs:1167
Suggest: a non-interactive Chip variant (tag, or removable with a close glyph) and a KeyCap kit component, so pills share one size and border.