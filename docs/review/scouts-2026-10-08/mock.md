Scout report: crates/ic-mock/src (tests skipped). Findings: 15. Top 3: (1) Targeted-filter lexer and parser re-implement ic-filter; (2) Severity formula duplicated with ic-model; (3) EventType duplicates ic_model::EventKind.

COVERAGE: Read in full: lib.rs, filter.rs, filter/targeted.rs, json.rs, error.rs, auth.rs, events.rs, control.rs, http/mod.rs, http/params.rs, http/response.rs, http/objects.rs, http/targets.rs, http/events.rs, http/info.rs, http/actions.rs, model/types.rs, model/attrs.rs, model/mod.rs, model/snapshot.rs, scenario/mod.rs, scenario/lab.rs, scenario/staging.rs, scenario/build.rs. Partly read: model/logic.rs (1-180, 684-1023, 1224-1320), model/load.rs (1-140, 561-720), model/stats.rs (1-130), sim.rs (80-240), rng.rs (1-60), scenario/prod_cluster.rs (selected ranges), large.rs (139-160), main.rs (165-200). Not read: server.rs, config.rs, outputs.rs, model/checker.rs, most of tls.rs, stats.rs 130-545, load.rs 140-560 and 720-1000, sim.rs 240-595, prod_cluster.rs bulk, large.rs bulk, main.rs bulk.

DEAD CODE: no unused private or pub(crate) non-test function found. MockControl pub methods with no caller in src (touch_object, emit_raw, emit_raw_line, effective_method, etc.) are public test API; their callers are in tests/, which I skipped.

---

1. Targeted-filter lexer and parser re-implement ic-filter
Copies: ic-mock/src/filter/targeted.rs:181-353 (Token, lex, Node, Parser); ic-filter/src/lexer.rs:287 (tokenize); ic-filter/src/parser.rs:46 (parse); ic-filter/src/ast.rs:31-57 (Expr/ExprKind, pub(crate)).
What: About 170 lines of tokenizer and precedence parser for `host.name == "x" || ...`, where ic-filter already parses the same grammar. Its AST is pub(crate), so the mock cannot reuse it.
Suggest: Have ic-filter expose a read-only "compared names" query built on its AST, then delete targeted.rs lex/Parser. Keep the Icinga-specific rules (no escapes, no keywords) as parameters.

2. Severity formula duplicated with ic-model
Copies: ic-mock/src/model/logic.rs:99-128 (World::severity); ic-model/src/severity.rs:16-20 (constants), 24-50 (service_severity, host_severity), 45+ (handling_weight).
What: Same constants (pending 16, downtime 256, ack 512, unreachable 1024, unhandled 2048, state weights 32/64/128) and same branch structure, typed twice. A divergence here changes sorting and handled behaviour silently.
Suggest: Expose a pure severity(state_weight, handling) helper from ic-model and call it from logic.rs, or compute service_snapshot(c).severity() in the mock.

3. EventType duplicates ic_model::EventKind
Copies: ic-mock/src/events.rs:20-81 (EventType, ALL, name, from_name); ic-model/src/event.rs:62-165 (EventKind, ALL, api_name, from_api_name).
What: The same 15 event names in the same order, with equivalent name and lookup functions. ic-model's EventKind also derives Hash, so it works in the mock's HashSet.
Suggest: Use ic_model::EventKind directly (the mock already depends on ic-model), delete EventType, and map name()/from_name() to api_name()/from_api_name().

4. Acknowledgement rules duplicated between control and HTTP actions
Copies: ic-mock/src/control.rs:221-250 (acknowledge), 256-263 (remove_acknowledgement), 230-235 (inline OK check); ic-mock/src/http/actions.rs:490-556 (acknowledge), 230-241 (remove-acknowledgement).
What: Both paths refuse OK/UP objects, expire then refuse already-acknowledged, and call World::acknowledge or clear with the same arguments. Only the error shape differs (MockError vs HTTP 409). control.rs:230-235 re-derives Checkable::is_state_ok (model/types.rs:289) by hand.
Suggest: Move the checks into one World method returning an enum (NoProblem, AlreadyAcknowledged); the HTTP handler and MockControl each map it to their own error.

5. Exit-code and state mapping repeated six times
Copies: ic-mock/src/control.rs:85-95 (service_code), 140-148 (host code in set_host_state), 185-197 (process_check_result); ic-mock/src/http/actions.rs:348-366 (process-check-result); ic-mock/src/scenario/build.rs:320-325 (apply_problem), 378-382 (service_in_state), 407-411 (generated_problem).
What: Each caller writes its own ServiceState or HostState to exit-status table. ic-model has ServiceState::from_code and HostState::from_code (state.rs:47, 93) but no inverse, and the Pending rejection differs per copy.
Suggest: Add ServiceState::exit_code() (Pending gives None) and a host equivalent next to from_code in ic-model, then use them everywhere.

6. Stream-body and Frame adapters duplicated
Copies: ic-mock/src/http/events.rs:24-48 (EventBody: mpsc to Body); ic-mock/src/http/objects.rs:270-306 (StreamedBody: mpsc of Chunk to Body, with a finished flag); ic-mock/src/events.rs:156-178 (EventFrame); ic-mock/src/http/info.rs:62-85 (StatusFrame).
What: Two mpsc-to-hyper body adapters with the same "aborted" io::Error mapping. Two Frame impls differ only in variable names and the input value.
Suggest: One generic channel body that maps closure to io::ErrorKind::ConnectionAborted, and one JsonFrame { names: &[&str], value: &Json, now }.

7. Scenario check-state seeding repeated in build.rs
Copies: ic-mock/src/scenario/build.rs:150-180 (host) and 230-272 (service): same check_command, attempts, zone, since, timing, result, last/next check seeding; 183-227 (host_down) and 310-359 (apply_problem): both set state, last_state_change, state_type, attempt, last_hard_state_change, result, last_check, next_check; 364-393 (service_in_state) and 396-421 (generated_problem): same code table and generated-output call.
What: Each scenario state change writes the same eight or so check fields in two or three places.
Suggest: A set_check_state(&mut CheckInfo, outcome) helper used by host_down, apply_problem and the two constructors.

8. Downtime creation and service fan-out duplicated
Copies: ic-mock/src/scenario/build.rs:549-585 (downtime) and 590-628 (downtime_with): both mint object!uuid, compute the in-effect rule, and push a Downtime; ic-mock/src/scenario/mod.rs:255-262 (apply_notification in_downtime closure); ic-mock/src/model/types.rs:407-415 (DowntimeData::is_in_effect); ic-mock/src/http/actions.rs:681-692 and 721-734 (services-of-host fan-out in schedule_downtime, written twice).
What: The in-effect rule appears four times. downtime() is a narrower copy of downtime_with(). The service fan-out loop is duplicated in schedule_downtime.
Suggest: Make downtime() a wrapper over downtime_with(); put one in_effect(start, end, fixed, trigger, duration, now) in model; add one fan_out_to_services helper in actions.rs. ic-model's Downtime::phase and effective_start/end (object.rs:483-520) differ slightly, so check before reuse.

9. Object indexes and create/remove plumbing duplicated in World
Copies: ic-mock/src/model/mod.rs:265-297 (index_comment, unindex_comment, index_downtime, unindex_downtime: same bodies over different maps); mod.rs:204-211 (insert_notification indexes the same way); ic-mock/src/model/logic.rs:793-836 (add_comment) and 862-940 (add_downtime): lookup checkable, clone zone, host and service, mint name, runtime_meta, index, insert, emit, ObjectCreated; logic.rs:839-857 (remove_comment) and 977-1017 (remove_downtime): check package "_api", unindex, emit removed event with to_event_json, emit ObjectDeleted.
What: Four index helpers with identical bodies, and two create/remove pairs with the same skeleton.
Suggest: One generic name index (BTreeMap<String, BTreeSet<String>>) with add, remove and get, and shared create and remove helpers for the common prefix and tail.

10. Permission pre-checks repeated across handlers
Copies: ic-mock/src/http/targets.rs:117-122 (filter-expression); http/events.rs:108-117 (same, 403 JSON); http/info.rs:133-142 (same); http/info.rs:96-104 (status/query); http/events.rs:87-92 (events/<type>); http/objects.rs:174-177 and http/targets.rs:56-61 (Missing permission format); http/actions.rs:113-114 path via filter_targets.
What: "require permission X, else 403 'Missing permission: x'", with the enforce_filter_permission flag, is written out in about six places.
Suggest: One fn require(user, permission, enforce) -> Result<(), String> in params or response, which each handler maps to its own error shape.

11. Glob matching implemented three times, with divergence
Copies: ic-mock/src/auth.rs:78-101 (glob_match: * and ? only); ic-api/src/info.rs:48-100 (glob_match, also handles backslash escapes, documented as Icinga's Utility::Match); ic-filter/src/pattern.rs:40-100 (Glob, glob_to_regex; pub(crate)). Out of scope, same idea: ic-core/src/dashboards/groups.rs:56, ic-app/src/pane/model.rs:50.
What: The mock's permission matcher lacks the backslash escape that ic-api implements, so a permission written with an escaped wildcard can match differently in mock and client.
Suggest: Put one glob matcher in ic-model (pure, no deps), used by ic-filter, ic-api and ic-mock, with escape handling in all.

12. Helpers that ic-model and ic-filter already provide
Copies: ic-mock/src/model/mod.rs:116-120 (wall_clock) vs ic-model/src/time.rs:25-31 (Timestamp::now); ic-mock/src/http/params.rs:179-188 (to_bool) vs ic-filter/src/value.rs:32-43 (Value::is_truthy, same rules); params.rs:146-152 (format_number) vs ic-filter/src/value.rs:305-315 (format_number, pub(crate)); ic-mock/src/tls.rs:269-285 (sha256, format_fingerprint) vs ic-api/src/tls.rs:36-50 (identical bodies); ic-mock/src/snapshot.rs:91-100 (HostState derivation) vs ic-model/src/state.rs:93-101 (HostState::from_code(code, reachable)).
What: Five helpers reimplemented with identical or equivalent bodies.
Suggest: Use Timestamp::now, Value::from_json(json).is_truthy(), and HostState::from_code; make ic-filter's format_number public; move the pure format_fingerprint into ic-model. Keep sha256 per crate, because ic-mock does not depend on ic-api (ic-api/Cargo.toml:40 dev-depends on ic-mock).

13. attr() rebuilds whole Comment/Downtime maps per attribute; event headers repeated
Copies: ic-mock/src/model/attrs.rs:713-729 (Comment and Downtime attr build the full insert_own_attrs map, then remove one key); ic-mock/src/model/types.rs:355-371 and 455-499 (insert_own_attrs, which repeats host_name, service_name and author lines); types.rs:344-353 and 445-453 (to_event_json, same 6-line header); ic-mock/src/model/logic.rs:566-587, 589-612, 708-710, 744-745 (event header "object fields + state + state_type" written four times).
What: For each requested comment or downtime attribute, the whole map (about 12 entries) is built and all but one key is discarded, and the attribute names live both in insert_own_attrs and in attr()'s match. The event header is repeated four times.
Suggest: Serialize each object once from a single own_attr(name) source, and add a checkable_event_header(checkable) helper.

14. Scenario-building boilerplate and lookups duplicated
Copies: Linux host service set: ic-mock/src/scenario/staging.rs:37-66, lab.rs:18-31, prod_cluster.rs:22-31 (STANDARD) and 436-447, large.rs:28-46. Duration helpers: prod_cluster.rs:13-20 (mins, hours reimplement Duration::from_mins and from_hours, which staging.rs already uses). Vars helper: ic-mock/src/model/load.rs:696-702 (json_vars) vs scenario/build.rs:67-73 (vars): identical bodies, both producing Vars. Lookups: scenario/mod.rs:232-242 (Scenario::host, service); build.rs:274-286 (service_mut, service_index); build.rs:189-190, 202-207, 500, 531 (inline host find).
What: The same host-with-standard-services pattern is written four times, and two helpers duplicate std or existing functions. Name lookups are written three ways.
Suggest: A Builder::linux_host(name, address, role, groups, env, services) helper; delete mins, hours and json_vars; one host_index and service_index on Scenario.

15. Name tables overlap ic-model and each other
Copies: ic-mock/src/http/actions.rs:558-581 (child_options maps string or number to 0/1/2) vs ic-model/src/action.rs:28-50 (ChildOptions and api_value; no parse). ic-mock/src/http/actions.rs:897-910 and 972-977 (command_type names) vs ic-model/src/action.rs:52-70 (CommandType; no NotificationCommand). ic-mock/src/model/stats.rs:14-32 (STATUS_FUNCTIONS) and model/attrs.rs:461-490 (EMPTY_TYPES): 16 of the 17 status names also appear in EMPTY_TYPES. ic-mock/src/http/actions.rs:427-449 (split_perfdata) vs ic-model/src/perfdata.rs:138-157 (parse_perfdata tokenizer, same whitespace and quote rules).
What: Icinga name tables and tokenizing rules are typed again in the mock.
Suggest: Add ChildOptions::from_api_value and CommandType::from_api_value beside api_value; derive EMPTY_TYPES and STATUS_FUNCTIONS from one table; expose ic-model's entry tokenizer for raw strings.

---

Smaller items not in the top 15: control.rs:432-443 and 569-580 are identical polling loops (wait_for_event_streams, wait_for_queued_checks). model/mod.rs:139-151 (checkable and checkable_mut) repeat the same split. The SOFT/HARD consts (logic.rs:74-75) are bypassed by literal 1 in control.rs:163 and snapshot.rs:42. Scenario::summary and Summary (scenario/mod.rs:178-190, 420-446) are used only by tests in that file. Security nit, not duplication: main.rs:168-177 write_file writes the private key with the default umask and then chmods to 0600, leaving a brief world-readable window.