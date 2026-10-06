# Contract tests against a real Icinga 2

`ic-mock` is only useful if it behaves like the real thing. This directory
pins that down:

- `run-icinga.sh` starts the official `icinga/icinga2` image in Docker with
  the fixtures in `icinga/` and prints the `ICYGUI_CONTRACT_*` variables the
  contract tests read. The `icygui` API user deliberately lacks the
  `filter-expression` permission, and `enforce_filter_expression_permission`
  is on, as it will be by default from Icinga 2.17.
- `samples/` holds real responses recorded from that instance (Icinga
  v2.15.6): every object type, status, info, action results, error bodies
  and an event stream, plus a lean service query (`services-lean.json`,
  `ic_api::Detail::Lean`'s attributes) and the answer to an unknown
  attribute (`error-400-invalid-field.json`). Tests use them as fixtures.
- `crates/ic-api/tests/contract.rs` checks the client against the running
  instance (read-only):

  ```sh
  set -a; eval "$(contract/run-icinga.sh)"; set +a
  cargo test -p ic-api --test contract
  ```

  The tests refuse any instance but this disposable one before sending a
  query (the URL must point to this machine and the fixture-only `viewer`
  user must log in): they load every object several times, which a
  production Icinga must not get from a test run. Load and scale tests run
  only against `ic-mock` or `scale/benchmark.sh`'s own local Icinga.

  Without the variables the tests pass without checking anything, unless
  `ICYGUI_CONTRACT_REQUIRED` is set. The `Contract` workflow
  (`.github/workflows/contract.yml`: nightly, on pull requests and pushes
  to `main` that change `ic-api`, `ic-model`, `contract/` or `Cargo.lock`,
  and by hand with another image tag) sets it, so a broken setup fails instead of passing. The
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
  nothing until the first check.
- A fresh start schedules every object's first check within
  `min(check_interval, 60 s)` (`Checkable::Start`).
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
