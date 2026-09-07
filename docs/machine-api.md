# Machine API reference

This describes sfh 1.6.0's `--json` surface as it actually behaves today:
which commands answer with the shared envelope, what the envelope contains,
the error-code vocabulary, the stability guarantee a caller can rely on, and
where the surface is currently inconsistent. It documents behavior, not a
target design — see "Known inconsistency" below for the one place those
differ.

The envelope itself is implemented in `src/machine.rs`; this file is the
prose companion to that code.

## Commands that emit the envelope

`--json` produces the envelope described below on exactly these commands:

- `sfh run --json`
- `sfh plan --json`
- `sfh wait --json`
- `sfh stop --json`
- `sfh status --json`
- `sfh preflight --json`
- `sfh workspaces list --json`
- `sfh workspaces show --json`
- `sfh workspaces clean --json`
- `sfh workspaces remove --json`

Two rules hold across all of them:

- In JSON mode, stdout carries JSON and nothing else. Progress, warnings and
  human-readable notes go to stderr, so `sfh ... --json | jq` is always safe.
- A configuration or usage error is still an envelope, not prose on stderr
  with a bare exit code. That is the case a machine caller most needs to be
  able to parse, because it is the case most likely to happen when something
  upstream is wrong.

`doctor`, `graph` and `config show` have no `--json` flag at all; they are
not part of this surface.

## The envelope shape

Every envelope carries this header, built by `machine::envelope` /
`machine::error_envelope` and merged with command-specific fields at the
**top level** (not nested under a `body` or `data` key):

| Field | Type | Meaning |
| --- | --- | --- |
| `schema_version` | integer | The version of the envelope shape itself (see "Stability guarantee"). Currently `1`. |
| `command` | string | The command that answered, e.g. `"run"`, `"workspaces remove"`. |
| `ok` | boolean | Whether the command succeeded. |
| `exit_code` | integer | The process exit code sfh will return. |
| `sfh_version` | string | sfh's own release version (`CARGO_PKG_VERSION`) — separate from `schema_version` and expected to change every release. |
| `error` | object or `null` | `null` on success; otherwise `{"code": "SFH_...", "message": "..."}`. |
| `warnings` | array | Non-fatal notes. Empty array when there are none. |
| `next_actions` | array | Zero or more diagnosed follow-ups. `why` and `wait` are always `{"kind", "argv"}` — a runnable argv, never prose. `resume` and `carry_budget` are diagnosed rather than assumed from state alone: they also carry `resumable`/`carryable`, `reason` and `requires`, and `argv` is present only when the action can actually succeed — an unrunnable one is still reported, with `reason` explaining why, instead of being silently dropped from the list. |

Header keys always win: a command's own fields are merged in first and the
header is applied on top, so nothing sfh runs can accidentally redefine `ok`
or `exit_code` and lie about whether it succeeded.

Command-specific fields (e.g. `run`'s `run_dir`, `run_id`, `result`,
`result_file`) live alongside the header at that same top level and vary by
command; see each command's `--help` or the worked example in the main
README's "Driving sfh from a program" section for a concrete instance.

Terminal `run`, `status`, and `wait` answers preserve one classification: a
protocol failure is `SFH_PROTOCOL_INVALID` or `SFH_TERMINAL_MISSING` in all
three commands, a human-decision terminal is `SFH_STUCK`, and an ordinary
run-time step failure is `SFH_STEP_FAILED`. The on-disk
`status.json` keeps its historical prose `error` string and records the same
classification additively as `error_code`; `status --json` converts those to
the envelope's required `{"code", "message"}` object.

`preflight --json` also separates flow validity from machine readiness. Its
body carries `flow_valid` (`true`, `false`, or `null` for a flowless survey)
and `failure_kind` (`flow_invalid`, `capability_unavailable`, or `null`). A
static flow error uses `SFH_FLOW_INVALID`; a valid flow blocked by a missing or
unverifiable program uses `SFH_CAPABILITY_UNAVAILABLE`.

## Error-code vocabulary

`error.code` is always one of the following (`src/machine.rs::ErrorCode`).
Branch on the code, never on `error.message` — the message is prose and is
allowed to be reworded at any time.

| Code | Meaning |
| --- | --- |
| `SFH_USAGE` | The command line itself was wrong. |
| `SFH_FLOW_INVALID` | The flow file could not be loaded or failed static validation. |
| `SFH_STEP_FAILED` | A flow step failed at run time (non-zero exit, timeout, failed fan-out, or an exhausted budget/visit ceiling) and the flow routed to failure. Distinct from `SFH_FLOW_INVALID`, which is a static authoring error. |
| `SFH_PROTOCOL_INVALID` | A structured tool protocol did not hold (see `src/protocol.rs`). |
| `SFH_TERMINAL_MISSING` | A structured protocol ended without its documented terminal record. |
| `SFH_SESSION_UNVERIFIED` | A resume or fork could not prove it landed in the expected session. |
| `SFH_EXECUTION_CLOSURE_CHANGED` | The pinned execution inputs differ from the run being resumed. |
| `SFH_WORKSPACE_MISSING` | A managed workspace that should exist does not. |
| `SFH_WORKSPACE_DRIFT` | A managed workspace changed underneath a resume. |
| `SFH_WORKSPACE_BUSY` | Another live run holds this workspace. |
| `SFH_RUN_BUSY` | Another live process owns this run directory. |
| `SFH_WORKSPACE_UNOWNED` | A path sfh was asked to manage is not one it created. |
| `SFH_REPLAY_REFUSED` | A replay policy refused to re-run an unfinished effect. |
| `SFH_PERSISTENCE_FAILURE` | A required durable artifact could not be written. |
| `SFH_CAPABILITY_UNAVAILABLE` | A capability the flow requires is unavailable or unverifiable, including a failed `require_version` check. |
| `SFH_STUCK` | The flow deliberately stopped for a human decision, including a `goto: stuck` reached through `max_visits`. |
| `SFH_INTERRUPTED` | The run was stopped or its recorded owning process disappeared. |

## Stability guarantee

The guarantee is tied to **`schema_version`** (`machine::SCHEMA_VERSION`), not
to sfh's own release number:

- A code's meaning is fixed for as long as `schema_version` does not change.
  Adding a new code is fine; repointing an existing one at a different
  meaning is not, for as long as `schema_version` stays the same value.
- The envelope's header shape (which fields exist and what they mean) is
  fixed under the same rule.
- `sfh_version` is expected to change on every release and carries no
  stability promise by itself — it is informational, not a version to branch
  on. `schema_version` is what a caller should check before trusting the
  header fields, and it is deliberately independent of `sfh_version`: sfh can
  ship many releases without ever bumping it.

`schema_version` is currently `1`. A caller that checks it before parsing the
rest of the envelope is protected against a future bump; a caller that
instead branches on `sfh_version` is not — that field moves every release for
reasons unrelated to the envelope's contract.

## Known inconsistency: commands with no envelope

Four `--json` commands predate this module and still print their own bare
JSON. They carry none of the header above: no `schema_version`, no
`command`, no `exit_code`, and no code from the `SFH_*` vocabulary. A caller
that assumes every `--json` command answers with the envelope will parse
these four incorrectly.

- **`sfh validate --json`** — on success: `{"ok": true, "path", "strict",
  "api_version", "warnings", "steps"}`. On failure: `{"ok": false, "path",
  "strict", "error"}`, where `error` is a plain string, not the
  `{"code", "message"}` object the envelope uses.
- **`sfh runs list --json`** — `{"runs": [...], "total_cost_usd",
  "total_own_cost_usd"}` (the two totals are identical; `total_cost_usd` is
  kept for existing consumers and sums each listed run's `own_cost_usd` —
  never `budget_position_usd`, which would double-count a carried run's
  inherited dollars against its ancestor's own row). Each entry in `runs` has
  `run_dir`, `status`, `started_utc`, `exit`, `ok`, `failed`, `visit`,
  `repeat`, `cost_usd`, `own_cost_usd`, `carried_cost_usd`,
  `budget_position_usd`, `lineage_cost_usd` — four differently-named answers
  to "how much did this cost" (own spend; inherited via
  `--carry-budget-from`; own+carried, what `max_cost_usd` is judged against;
  and the full carry ancestry). `cost_usd` is kept, identical to
  `budget_position_usd`, for existing consumers. `lineage_cost_usd` is `null`
  — never a partial sum — the moment an ancestor in the carry chain has been
  removed by `runs clean` and so can no longer be verified. There is no
  lineage total across the listing: rows that share an ancestor would
  double-count it.
- **`sfh runs show --json`** — the same per-run fields as above flattened
  into the top level (no nested `summary` key) alongside `flow`,
  `sfh_version`, `tools`, `budget_landed`, `steps` — with no wrapper at all.
  Each `steps[]` entry includes `attempts`, the total process attempts across
  its leaf events; a legacy `step_end` without that field counts as one.
- **`sfh runs why --json`** — `{"run_dir", "state", "current_step", "error",
  "harness_diagnostic", "protocol_failure", "explanation", "last_event",
  "last_position", "unfinished_leaves", "unfinished_fanouts",
  "unfinished_fallbacks", "unfinished_postprocessing"}`. Note that `error`
  here is a bare string-or-`null` from the run's `status.json`, which looks
  superficially like the envelope's `error` field but is not shaped like it.

These legacy commands still honor the basic stdout guarantee on early
failures. Missing arguments, unknown flags, and a missing/unsafe run directory
produce valid bare JSON with `"ok": false` and a prose `"error"` string;
they never leave stdout empty when `--json` was requested.

This is a real trap, not just a hypothetical one: a `run --json` envelope's
own `next_actions` routinely suggests `sfh runs why <dir> --json` as the next
call, and that response is one of the four bare-JSON shapes above, not
another envelope. Detect which kind of response you have by checking for
`schema_version` rather than assuming based on which command you called.

**Intended direction, not yet scheduled:** unifying `validate` and `runs
list|show|why` onto the envelope is the known fix for this gap. It has not
happened as of 1.6.0 because it is a breaking change to four response shapes
at once, and it is out of scope for the fixes in this document — treat the
shapes above as current fact, not as something about to change underfoot.
