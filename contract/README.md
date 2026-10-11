# Contract tests against a real Icinga 2

`ic-mock` is only useful if it behaves like the real thing. This directory
pins that down:

- `run-icinga.sh` starts the official `icinga/icinga2` image in Docker with
  the fixtures in `icinga/` and prints the `ICYGUI_CONTRACT_*` variables the
  contract tests read. The `icygui` API user deliberately lacks the
  `filter-expression` permission, and `enforce_filter_expression_permission`
  is on, as it will be by default from Icinga 2.17.
- The same fixtures are part of the demo cluster (`demo/docker-compose.yml`,
  docs/demo.md): `icinga/icygui-test.conf` in its master zone,
  `icinga/icygui-groups.conf` (the groups, a file of their own for that) in
  a global zone. `demo/up.sh` prints the same variables for its master-01,
  so the contract tests run against a two-master cluster with satellites
  and changing states as well; the engine's cluster tests
  (`cargo test -p ic-core --test engine cluster::`) run there only.
- `samples/` holds real responses recorded from that instance (Icinga
  v2.15.6): every object type, status, info, action results, error bodies
  and an event stream, plus a lean service query (`services-lean.json`,
  `ic_api::Detail::Lean`'s attributes) and the answer to an unknown
  attribute (`error-400-invalid-field.json`). Tests use them as fixtures,
  read in place (`ic-mock`'s `tests/fidelity.rs` too; there is no copy to
  keep in sync). The recorded stream has no `Notification`, `Flapping` or
  `ObjectModified` event (they need a notification, a flapping object or a
  configuration change); until a recording adds them, `ic-mock`'s fidelity
  test checks those against the keys Icinga's `apievents.cpp` writes.
- `crates/ic-api/tests/contract.rs` checks the client against the running
  instance (read-only):

  ```sh
  set -a; eval "$(contract/run-icinga.sh)"; set +a
  cargo test -p ic-api --test contract
  ```

  The tests refuse any instance but these disposable ones before sending a
  query (the URL must point to this machine and the fixture-only `viewer`
  user must log in): they load every object several times, which a
  production Icinga must not get from a test run. Load and scale tests run
  only against `ic-mock`, `scale/benchmark.sh`'s own local Icinga or the
  demo cluster.

  Against the demo cluster:

  ```sh
  set -a; eval "$(demo/up.sh)"; set +a
  cargo test -p ic-api --test contract
  ```

  Without the variables the tests pass without checking anything, unless
  `ICYGUI_CONTRACT_REQUIRED` is set. The `Contract` workflow
  (`.github/workflows/contract.yml`: nightly, on pull requests and pushes
  to `main` that change `ic-api`, `ic-model`, `contract/` or `Cargo.lock`,
  and by hand with another image tag) sets it, so a broken setup fails instead of passing; so
  does the `Demo cluster` workflow (`.github/workflows/cluster.yml`). The
  script returns as soon as the API answers; tests that need checked
  objects wait until Icinga has run every active check once (within a
  minute of a fresh start).
- `record-queries.py` records `samples/queries.json`: read-only object and
  status queries with their answers (attribute selection, joins, meta,
  name lists, unknown attributes, filters, flags). `ic-mock`'s
  `tests/fidelity.rs` replays them against the mock.

Facts learned from the real instance that the client must respect:

- Whole numbers are JSON integers (`"state": 2`), other numbers floats.
- Filter expressions (`filter`) need the `filter-expression` permission;
  target objects by name instead (`hosts` / `services` arrays), which needs
  no extra permission.
- A name list with one unknown name fails the whole request with
  `404 No objects found.`
- `last_check_result.exit_status` stays 0 for passive results; the state is
  `last_check_result.state` / the object's `state`.
- `execute-command` fails per object with `Can't find a valid endpoint`
  unless an endpoint is given or the object has `command_endpoint`.
- An attribute the type doesn't have, in `attrs` or `joins`, fails the
  whole query: `400 {"error":400,"status":"Invalid field specified: <name>"}`
  (newer versions answer every object with that error instead). Icinga
  notices it only while serialising an object: a type without objects (no
  comments, say) answers `200 {"results":[]}` whatever `attrs` holds.
  Attributes users can't see (`state_raw`) and `service.host` are accepted
  but left out; `"attrs": []` returns objects without any attribute.
- A never-checked object has `last_check: -1`, no `last_check_result`, and
  `last_state_change`, `last_hard_state_change` and `previous_state_change`
  0. Services report `state` 3 (UNKNOWN, the default raw state); hosts map
  that default to `state` 1 (`Host::CalculateState`). The state means
  nothing until the first check, but `/v1/status/CIB` counts such a
  service as unknown (`num_services_unknown`, and `num_services_pending`).
- An endpoint's `connected` is `false` for the node that answers (its own
  endpoint); quiet mode's event stream (every type but `CheckResult`)
  opens with the `icygui` user's `events/*`.
- A fresh start schedules every object's first check within
  `min(check_interval, 60 s)` (`Checkable::Start`). In the demo cluster's
  HA zone fra on a loaded machine a few never ran (their `next_check`
  passed, 14 minutes later still unchecked) until a forced
  `reschedule-check` ran them at once; `demo/up.sh` reschedules such
  checks and doesn't say healthy before they ran.
- A fixed downtime counts in `downtime_depth` from its `start_time`
  (`Downtime::IsInEffect` asks the clock), but `DowntimeStarted` comes
  from a timer every 5 s: a state change in between already carries the
  count (`cluster_a_fixed_downtime_counts_from_its_start_like_icinga`, 3
  of 4 recoveries). A result that isn't OK triggers due downtimes first,
  so its `DowntimeTriggered` comes before its check result. An answer to a
  query sent right after downtimes are set often has them before the
  stream's lines do, but not always: a burst of downtime events (each
  `DowntimeAdded` and `DowntimeTriggered` about a kilobyte) is often
  written at once, and only its tail waits for the stream's next write.
  `cluster_downtimes_and_acknowledgements_around_a_wake_match_icinga`
  (four downtimes, set right after a write of the quiet stream was seen)
  saw an answer come before its line in about half of its rounds (10 of 19,
  each with 1 to 3 of the 4), so it tries up to 8 rounds; with two
  downtimes it was 3 of 10. Its targets are soft problems: a hard one
  notifies, the notification's prefetch loads it in full, and the refresh
  after waking leaves out what it holds in full with a current result.
- Check events (`CheckResult`, `StateChange`) carry `acknowledgement` as
  it was when Icinga built them. A passive result for an acknowledged
  problem sent 0.5 s after a quiet engine woke came only on the new live
  stream (no state change), the acknowledgement's removal right after it
  on both: at the handover the result was applied after the removal, and
  before the small fix 3 fixer the store said acknowledged while Icinga
  didn't (every round; the same test).
- A filter that fails for any object fails the whole query with
  `404 No objects found.`; an empty filter matches nothing. A filter that
  doesn't compile fails only when it is evaluated (Icinga compiles it into
  a `ThrowExpression`): a type without objects answers `200` with no
  results, and an invalid `filter_vars` (read first) is the reported
  error. Event streams treat `filter: ""` as no filter and open silently
  with a filter that doesn't compile. Status filters see the entry as
  `dictionary`, not `status`.
- `last_check_result` is a `CheckResult` object in filters: a field it
  doesn't have (`last_check_result.outptu`, even `.type`) fails the query.
- For `type` `Host` or `Service`, a filter that only compares names with
  constants (`host.name == "a" || host.name == "b"`, or
  `host.name == "h" && service.name == "s" || …`) is not evaluated: the
  named objects come in the filter's order, duplicates included, and
  unknown names are left out.
- `all_joins`, `pretty` and `verbose` are read through numbers: `"0"` is
  false and `"true"` an error (`pretty=true` answers 500).

Facts learned from the demo cluster (two masters, satellite zones; the
cluster tests in `crates/ic-core/tests/engine/cluster.rs` hold them):

- When an endpoint is gone (stopped or cut off), the relay queue
  (`json_rpc.relay_queue_items`) stays flat: Icinga 2.15 queues nothing
  for it.
- A check pinned to an endpoint that isn't connected (`command_endpoint`)
  gets an UNKNOWN result, `Remote Icinga instance 'X' is not connected to
  'Y'` (Y the node running the check), but only once that node has run for
  5 minutes (`Checkable::ExecuteCheck`'s cold-start window); until then the
  check stays silent, its last result in place.
- A zone with no connected endpoint goes silent: no other node runs its
  checks, nothing turns UNKNOWN, `last_check` stays where it was; the
  masters' `cluster-zone` check for it turns CRITICAL.
- A frozen node (`docker pause`, a hung process) keeps its connections open;
  its peers report it disconnected after about 90 s.
- In an HA zone `CheckerComponent` and `NotificationComponent` objects
  report `paused: true` on one of the masters (their own HA state), although
  that master runs its share of the checks (`check_source`) and sends its
  share of the notifications (the `Notification` objects' authority is
  split between the masters, and both log *Completed sending*).
- A checker feature that is off on one master of an HA zone isn't made up
  for: the other master keeps to its own share, and the off master's share
  goes unchecked (about 500 of the demo's 2 200 master-zone checks late 2.5
  minutes after `icinga2 feature disable checker` on master-02). Seen from
  the other master nothing else shows it: its own `CheckerComponent` reads
  as before (`paused: true`), both masters stay connected, and master-02's
  pinned heartbeat keeps beating (master-01 schedules it, master-02 only
  runs the command). icygui raises the zone's late checks as an alert of
  their own (`cluster_a_checker_off_on_the_other_master_shows_as_the_zones_late_checks`,
  `demo/scenario.sh checker-off`).
- Not checked against a real Icinga DB (the demo cluster runs none): which
  master writes while `IcingaDB` reports `paused` on the other. icygui says
  only `paused here` (`· HA with master-02` with a partner connected), and
  the IcingaDB tile `doesn't write from here`. The `IcingaDB` object
  doesn't return the Redis password (`objects/query/IcingaDB` checked on a
  single node).
- Without `objects/query/CheckerComponent` (and the other feature types)
  Icinga answers `403 Missing permission: objects/query/checkercomponent`.
- A passive check is overdue (`next_update`, and icygui's *late*) twice its
  interval after its last result.
- Icinga 2.15.6 writes each `/v1/events` stream at most every 5.0 s
  (a burst large enough is written at once, see the downtimes above),
  idle or busy: a raw stream got bursts at 0, 5, 10 and 15 s, the oldest
  event of each 5 s old, the newest fresh. A new stream sends its first
  events at once. A client that closes a stream loses what it still held
  (icygui's quiet/live switch waits 7 s for the old stream's batch;
  `cluster_switching_modes_during_a_problem_storm_logs_and_notifies_each_change_once`).
- A burst of passive results (`process-check-result`, three per service)
  makes a `StateChange` of every attempt; a soft state that turns hard
  keeps its `last_state_change` (the first attempt) and gets
  `last_hard_state_change` (the hard one).
- Attributes changed through the API (`enable_active_checks`) reach the
  satellites that run the checks; `restore_attrs` puts the configuration's
  value back.
- Objects made at run time through one master of an HA zone (downtimes,
  comments, acknowledgement comments) reach the other only if that one
  accepts configuration (`accept_config`), the config master included;
  Icinga ignores synced zone configuration for the zones a node has in
  `zones.d`, so that is safe. Without it the config master logs `'api' does
  not accept config` and the two masters show different downtimes.
- A `ScheduledDowntime` creates only its running or its next segment, about
  a minute after the start, and the range key `"monday - sunday"` creates
  nothing in 2.15 (one key per day does).
