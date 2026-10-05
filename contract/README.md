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
  and an event stream. Tests use them as fixtures.

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
