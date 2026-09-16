# sfh — SimpleFlowHarness

[![ci](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml/badge.svg)](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/Aero123421/SimpleFlowHarness)](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

English | [日本語](README.ja.md)

A single-binary workflow runner that chains AI coding CLIs — **Codex**, **Claude Code**, **opencode**, **Grok**, **Antigravity (`agy`)**, **Pi**, **Cursor** — and plain shell commands into multi-step YAML flows, with conditional routing, retries, parallel fan-out, resumable execution, and a full audit log.

sfh handles process lifecycle, routing, logging, and recovery. Your commands and agents keep the say on what to do; a step is judged by exit codes and machine-readable protocols, not by parsing prose.

**Contents** — [What you get](#what-you-get) · [Installation](#installation) · [Quick Start](#quick-start) · [Flow format](#the-flow-format-in-one-table) · [Exit codes](#exit-codes) · [Programmatic use](#driving-sfh-from-a-program) · [Run artifacts](#what-a-run-leaves-behind) · [Documentation](#documentation)

## What you get

| Capability | How |
|---|---|
| Conditional routing | `route:` rules on last line, protocol state, labels, members — or `goto:end` / `fail` / `stuck` |
| Parallel fan-out | `parallel` (heterogeneous workers) and `foreach` (lines / JSON arrays) |
| Background runs | `sfh run --detach`, then `status` / `wait` / `stop` |
| Resume after interruption | `--resume-latest`; refuses to resume if the execution closure (flow, context, tool versions, workspace) changed since the run started |
| Managed workspaces | `workspace: {mode: git-worktree}` — writes land on a `sfh/<flow>/<run-id>` branch, your checkout untouched; dirty state is never discarded automatically |
| Budgets | `max_cost_usd`, `wall_clock_sec`, `max_visits`, `max_total_steps`; carry spend into a corrected flow with `--carry-budget-from` |
| Access scopes | AI steps require an explicit `access: read \| write \| full` |
| Machine interface | `--json` envelope on `run`/`plan`/`wait`/`stop`/`status`/`preflight`/`workspaces`, with stable `SFH_*` error codes |
| Free pre-flight checks | `sfh preflight` verifies binaries, versions, and flags **without** model calls; `sfh doctor` sends one real token to catch protocol drift |

## Installation

**macOS / Linux:**

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-installer.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-installer.ps1 | iex
```

**Homebrew:**

```bash
brew install Aero123421/tap/sfh
```

Pre-built binaries and SHA-256 checksums: [GitHub Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest). Pin a version with `SFH_VERSION`, choose directories with `SFH_INSTALL_DIR` / `SFH_DATA_DIR`, skip `PATH` changes with `SFH_NO_MODIFY_PATH=1`. What each channel verifies — and what piped installs inherently trust — is documented in [docs/distribution.md](docs/distribution.md).

## Quick Start

A flow that runs tests and sends failures to an AI agent for repair, looping until they pass or give up twice (`max_visits: 3`):

```yaml
api_version: 1
name: test_and_repair
defaults:
  max_visits: 3
  wall_clock_sec: 1800
workspace:
  mode: git-worktree

steps:
  - id: test
    cmd: ["cargo", "test"]
    effects: workspace
    on_error: goto:fix

  - id: ship
    cmd: ["sfh", "--version"]
    effects: read
    route: [{goto: end}]

  - id: fix
    tool: codex
    access: write
    prompt: |
      Open this stderr file, diagnose the failure, and fix it:
      {{steps.test.stderr_file}}
    route: [{goto: test}]
```

```bash
sfh validate flow.yaml --strict   # static checks: syntax, variables, routes
sfh plan flow.yaml                # execution plan, in a throwaway temp dir
sfh run flow.yaml                 # run it
```

More examples: [examples/](examples/) (`research.yaml`, `parallel-ideas.yaml`, `managed-loop.yaml`, and [examples/ponytail/](examples/ponytail/) — 20 flows for real repository work: regression-first bugfix, dual review, release gates).

## The flow format, in one table

Two step types:

| Type | Written as | Judged by |
|---|---|---|
| AI tool step | `tool: codex` (+ `access`, `prompt`) | the tool's machine-readable protocol (fail-closed: output shape drift fails the step) |
| Shell command | `cmd: ["cargo", "test"]` (array: no shell; string: `sh -c` / `cmd /C`) | exit code — optionally remapped semantically with `outcomes:` |

Preset AI steps default to `allow_empty: false`: a turn that finishes without a final message fails the step. For a worker whose product is the diff, set `allow_empty: true` and let a `cmd:` verification step prove the work instead.

Routing predicates available on any step:

| Predicate | Matches |
|---|---|
| `when_last_line_is: X` | exact final line of output |
| `when_protocol_is:` `plain / valid / missing_terminal / invalid` | adapter protocol state after a failed leaf (requires `on_error: continue`) |
| `when_label_is: X` | a label you assigned to an exit code via `outcomes:` |
| `when_members: {last_line_is: X, all/n: ...}` | consensus across `parallel` / `foreach` members |

A review step routing on its final line:

```yaml
api_version: 1
steps:
  - id: review
    tool: claude
    access: read
    prompt: "Review the code change. End your response with PASS or REVISE."
    route:
      - {when_last_line_is: PASS, goto: end}
      - {when_last_line_is: REVISE, goto: stuck}
      - {goto: stuck}
```

Two agents reviewing in parallel, shipping only on unanimous PASS:

```yaml
api_version: 1
steps:
  - id: council
    max_parallel: 3
    parallel:
      - {id: rev_a, tool: claude, access: read, on_error: continue, prompt: "End with PASS or FAIL."}
      - {id: rev_b, tool: codex, access: read, on_error: continue, prompt: "End with PASS or FAIL."}
    route:
      - {when_members: {last_line_is: PASS, all: true}, goto: end}
      - {goto: fail}
```

Everything else — sessions (`continue_from` / `fork_from`), replay policy for interrupted steps (`replay.unfinished`), context pinning (`contexts:`), required tool versions (`require_version`), profile overlays (`--profiles`), exit-code conflicts (`exit_conflict`) — is documented in `sfh guide`, [CHANGELOG.md](CHANGELOG.md), and the schema.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Flow ended at `goto:end`; status is `done` |
| 1 | Failed (`goto:fail`), tool error, or observed `failed` / `dead` / `stopped` |
| 2 | Configuration, CLI, or validation error |
| 3 | Still running (`status`, or a `wait` that timed out) |
| 4 | `goto:stuck` — human intervention requested |

## Driving sfh from a program

```bash
sfh preflight flow.yaml --json          # free checks, no model calls
sfh plan      flow.yaml --json --save   # what would run, starts nothing
sfh run       flow.yaml --json --detach # returns a handle + next_actions
sfh wait <run-dir> --json               # blocks, returns the result
```

Stdout carries a JSON envelope and nothing else (progress goes to stderr); failures carry stable codes (`SFH_USAGE`, `SFH_STEP_FAILED`, `SFH_PROTOCOL_INVALID`, `SFH_EXECUTION_CLOSURE_CHANGED`, …) you can branch on. Full contract: [docs/machine-api.md](docs/machine-api.md).

## What a run leaves behind

Append-only records under `.sfh/runs/<run-id>/`: `log.jsonl` (events, tokens, cost, protocol evidence), capped `<step>.out.txt` / `.err.txt` (32 MiB, head+tail kept), `status.json`, `execution-closure.json`, workspace and context snapshots. Every resume replays decisions from this log rather than re-running completed steps.

Public JSON schemas: [flow](schema/flow.schema.json) · [log events](schema/log-event.schema.json) · [status](schema/status.schema.json) · [retention](schema/retention.schema.json)

## Documentation

- `sfh guide` — built-in syntax reference (also what an AI writing flows should read)
- `sfh --help`, `sfh <command> --help` — CLI options
- [docs/README.md](docs/README.md) — index of current vs. historical docs
- [skills/](skills/) — [Agent Skills](https://agentskills.io/specification) that teach an authoring AI sfh's design rules (`cp -R skills/sfh-* .agents/skills/`); optional, and not a runtime feature
- [AGENTS.md](AGENTS.md) · [CONTRIBUTING.md](CONTRIBUTING.md) · [SECURITY.md](SECURITY.md) · [CHANGELOG.md](CHANGELOG.md)

Supported platforms: Windows, macOS, Linux.
