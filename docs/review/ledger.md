# Findings ledger

Every confirmed review finding, and every rejected one, from 2026-10-10 on, tagged with the rule it broke. It feeds the invariants catalogue for the pre-rc behaviour hardening (PLAN.md 4.2, "Pre-rc behaviour hardening"): the catalogue grows from what actually broke, and findings that fit no rule show where a rule is missing.

**How to add an entry:** one row per finding, newest at the bottom. Use one or more tags from the list below. When no tag fits, write `untagged` and propose a new tag in the notes; untagged rows are the gaps the brainstorm looks at first.

Columns:
- **Date**, and **Where**: stage, part and commit.
- **Finding**: one line.
- **Sev**: major or minor.
- **Outcome**: fixed (commit), or rejected (with the reason).
- **Tags**.
- **Found by**: review, probe test, running app, screenshot, CI, user.

## Tags (provisional, until the catalogue replaces them)

| Tag | Rule |
|---|---|
| `no-false-green` | The UI never looks better than reality: summaries are the worst of their parts, ages come from timestamps on the UI clock, and silence or a dead stream never stays green. |
| `one-cause-one-alert` | One cause raises one alert and one notification, never two. |
| `nothing-silent` | Whatever stops reporting or disappears is shown, never just dropped. |
| `nothing-moves` | Nothing moves when state changes: fixed slots. |
| `keyboard-reach` | Every page, link and action can be reached and used by keyboard. |
| `link-does-something` | Every control visibly does what it says, in every state. |
| `request-economy` | No needless requests to Icinga; failures back off; refused or unsupported things aren't asked again. |
| `no-load-on-real` | No load or contract tests against real masters or workers. |
| `failure-visible` | Every failure path ends in a visible state and a log line at the right level. |
| `honest-docs` | Docs and comments describe what the code does. |
| `one-rule-one-place` | One rule lives in one function; no second copy that can drift. |
| `demo-shows-it` | The demo shows every feature in a believable setup. |
| `platform-parity` | Behaviour matches across Linux, macOS and Windows, or the difference is documented. |
| `no-secrets` | No secrets in config, logs or commits. |

## Entries

| Date | Where | Finding | Sev | Outcome | Tags | Found by |
|---|---|---|---|---|---|---|
