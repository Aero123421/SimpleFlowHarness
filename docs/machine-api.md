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
| `SFH_SESSION_UNVERIFIED` | A fresh, resumed, or forked session could not be proven to be the expected session. |
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

## Validation, run browsing, and cleanup (v2)

Every `--json` command uses the common envelope above, including usage errors.
Existing `run`, `plan`, and watch envelopes keep schema version 1 and their
meaning. The formerly bare responses change in v2:

| Command | Command-specific fields |
| --- | --- |
| `validate` | `path`, `strict`, `api_version`, `warnings`, `steps`; failure uses `SFH_FLOW_INVALID` |
| `runs list` | `runs`, `total_cost_usd`, `total_own_cost_usd` |
| `runs show` | per-run summary, `flow`, `run_sfh_version`, `tools`, `budget_landed`, `steps` |
| `runs why` | `run_dir`, `state`, `current_step`, `run_error`, diagnostics, durable positions and unfinished work |
| `runs clean` | `runs_dir`, `removed`, `skipped` with reasons, `dry_run`, `kept_newest`, `errors` |

The top-level `ok` is response success, not the number of successful steps.
That count is now `successful_steps` in run summaries (including list rows).
The top-level `sfh_version` identifies the inspecting binary; `run_sfh_version`
is the recorded version. `runs why` can successfully inspect a failed run:
`ok: true`, `error: null`, and `run_error` describes the run's failure.
Missing/unreadable run data uses `SFH_PERSISTENCE_FAILURE`.

Cost fields keep their meanings: `own_cost_usd` is this run's spend,
`carried_cost_usd` is inherited spend, `budget_position_usd` is their sum,
and `lineage_cost_usd` is null when the complete ancestry cannot be verified.
`total_own_cost_usd` sums only listed runs' own spend. `steps[].attempts`
continues to count process attempts, with legacy events counting as one.

Cleanup's `removed` contains candidates when `dry_run` is true. Dry-run takes
the same lease and repeats the same ownership checks as deletion; it may
create a lock file. Active, ambiguous, unfinished, locked, and still-managed
workspace runs are retained. A skipped run is not an IO failure; actual
removal failures return exit 1 with `SFH_PERSISTENCE_FAILURE` and `errors`.

`exit_bound` is an additive protocol state for OpenCode 2: structured completed
text was observed, but success still depends on process exit. `terminal_seen`
is false and `terminal_success` is null unless an actual error event arrived.
It cannot justify `exit_conflict: trust_protocol` correcting a nonzero exit.
`valid` retains its existing meaning: a documented terminal record was seen.
