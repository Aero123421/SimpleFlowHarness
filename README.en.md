# sfh — SimpleFlowHarness

[![ci](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml/badge.svg)](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/Aero123421/SimpleFlowHarness)](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

English | [日本語](README.md)

`sfh` is a small, single-binary workflow runner for AI coding CLIs and ordinary
commands. It connects Codex, Claude Code, opencode, Grok, Antigravity (`agy`),
Pi, Cursor, or custom commands into YAML-defined flows with routing, retries,
parallel fan-out, session continuation, bounded execution, and crash recovery.

The engine owns plumbing, not judgment: your commands and agents decide the
work; sfh records facts and follows the routes you declared.

## Why sfh

- Keep orchestration outside the main agent's context window.
- Put only the selected final result on stdout; retain every prompt, output,
  event, token count, and reported cost in a run directory.
- Detach long flows and later inspect, stop, wait for, or resume them.
- Combine deterministic command gates with AI review and repair loops.
- Resume a crashed fan-out without rerunning members whose completion was
  already durably recorded.
- Validate complex control flow before making a paid tool call.

## Install

Download a binary from [GitHub Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest).
Each archive has a matching SHA-256 file.

```powershell
# Windows x64
irm https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-windows-x64.zip -OutFile sfh.zip
Expand-Archive sfh.zip -DestinationPath sfh-bin -Force
.\sfh-bin\sfh.exe --version
```

```bash
# Linux x64; substitute the macOS or Linux arm64 asset when needed
curl -fsSL https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-linux-x64.tar.gz |
  tar xz sfh
./sfh --version
```

With Rust:

```bash
cargo install --git https://github.com/Aero123421/SimpleFlowHarness
```

## Quick start

```bash
sfh init flow.yaml
sfh validate flow.yaml --strict
sfh plan flow.yaml --var topic="Rust async runtimes"
sfh run flow.yaml --var topic="Rust async runtimes"
```

A minimal deterministic repair loop:

```yaml
api_version: 1
name: repair
defaults:
  max_visits: 3
  max_total_steps: 12
  wall_clock_sec: 1800
steps:
  - id: test
    cmd: ["cargo", "test"]
    on_error: goto:fix
  - id: done
    cmd: "echo tests passed"
    route: [{goto: end}]
  - id: fix
    tool: codex
    access: full
    prompt: |
      Fix the failure. Diagnostics: {{steps.test.stderr_file}}
    route: [{goto: test}]
```

Array-form `cmd` is spawned directly and is the portable default. String-form
commands use `cmd /C` on Windows and `sh -c` on Unix.

## Complex-flow primitives

`route` evaluates rules in order. A condition-free rule is the catch-all and
must be last. Strict validation warns when a conditional route can fall through
implicitly.

```yaml
api_version: 1
steps:
  - id: review
    tool: claude
    access: read
    prompt: "Review the change. End with PASS or REVISE."
    route:
      - {when_last_line_is: PASS, goto: end}
      - {when_last_line_is: REVISE, goto: revise}
      - {goto: stuck}
  - id: revise
    tool: codex
    access: full
    prompt: "{{steps.review.output}}"
    route: [{goto: review}]
```

Use `parallel` for heterogeneous workers and `foreach` for data-driven fan-out.
`when_members` counts only members that exited successfully and ended with the
exact requested line.

```yaml
api_version: 1
steps:
  - id: council
    max_parallel: 3
    parallel:
      - {id: a, tool: claude, access: read, on_error: continue, prompt: "End with PASS or FAIL."}
      - {id: b, tool: codex, access: read, on_error: continue, prompt: "End with PASS or FAIL."}
    route:
      - {when_members: {last_line_is: PASS, all: true}, goto: end}
      - {goto: stuck}
```

Session reuse is explicit:

- `continue_from: step` continues one recorded session.
- `fork_from: step` branches a supported provider session.
- The source must dominate the consumer on every control-flow path and must
  fail closed; otherwise validation rejects the flow.

The same dominance check applies to `{{steps.ID.*}}` dependencies. If a missing
branch is intentional, say so:

```yaml
prompt: "{{steps.optional_branch.output | optional}}"
# or:
prompt: "{{steps.optional_branch.output | default:not-run}}"
```

## Operational commands

```text
sfh validate flow.yaml [--strict] [--json]
sfh plan flow.yaml                         # isolated, side-effect-free dry run
sfh graph flow.yaml [--mermaid]
sfh config show flow.yaml                  # merged profiles; env values redacted
sfh config show flow.yaml --show-secrets   # explicit sensitive output
sfh run flow.yaml [--detach] [--resume DIR]
sfh status [RUN] [--json]
sfh wait [RUN] [--timeout SEC]
sfh stop [RUN]
sfh runs list|show|why|clean ...
```

`sfh status` includes active fan-out members and completed/total counts.
`sfh runs why` reconstructs the last durable position, unfinished leaves, and
what a resume will rerun.

`sfh config show` prints the fully merged effective configuration but redacts
every environment value by default. Use `--show-secrets` only when the actual
values are required for local diagnosis; that output is sensitive and must not
be pasted into public issue reports.

## Resume and durability contract

A run stores append-only events and content artifacts under `.sfh/runs`.
Required writes are fail-closed. A leaf is reusable only after its artifacts
and `step_end` have been synced. Fan-out completions are recorded as each member
finishes rather than after the entire pool joins.

Resume verifies both:

1. the flow file fingerprint; and
2. the execution-relevant effective configuration after merging
   `~/.sfh/profiles.yaml`.

Tool, model, access, arguments, environment, working directory, and default
changes therefore stop a resume unless `--force-resume` is given. Changes to
an unrelated global profile that this flow never references do not. Wall-clock
usage is cumulative across attempts.

Resume also verifies that named chain, plain, and pre-compaction checkpoint
artifacts exist and match any recorded output hash. It will not silently restore
a missing or modified checkpoint as empty output. If a paid attempt finishes but
publishing its artifacts fails, sfh retains its token and cost accounting and
records `persistence_failure`. That run is deliberately non-resumable because
sfh cannot know whether the external side effect completed; verify it before
starting a new run.

## Limits and honest boundaries

- `max_total_steps` bounds logical leaf runs, including fallback and compact
  calls. Retry attempts inside one leaf are controlled separately by
  `retry.max`.
- One `foreach` expansion is limited to 100 items. Split or batch a larger
  collection explicitly; the limit is checked before any member starts.
- `wall_clock_sec` is an engine-enforced deadline and includes fan-out queue
  time. A process can take a short time to be killed and reaped.
- `max_cost_usd` uses costs reported by provider CLIs after an attempt. It is a
  soft accounting guard, not a prepaid or provider-side hard billing cap.
  It is evaluated between top-level steps, so retries and fallbacks inside the
  current leaf, plus already-running fan-out siblings, can report spend beyond
  the threshold. Providers that report no cost cannot be bounded by this field.
- `access` maps a common read/write/full vocabulary onto provider-specific
  flags; it is not an OS sandbox. Review the resolved command with `sfh plan`.
- Ctrl+C, `sfh stop`, timeouts, and catchable termination signals stop owned
  process trees. Background descendants remain owned by their leaf and are
  reaped when its root command exits; use a separate detached sfh run for work
  that must outlive a step. On Windows each leaf has its own nested job, so a
  timed-out member's descendants are killed without terminating parallel siblings.
  Windows process-wide jobs and Linux parent-death signaling also cover a
  hard-killed sfh process. macOS has no equivalent parent-death
  primitive, so an uncatchable `SIGKILL` or host crash cannot guarantee that
  every descendant is reaped; prefer the normal stop path.
- `--force-resume`, `unsafe_shell_template`, `allow_access_override`, and
  `allow_dynamic_exec_paths` are explicit escape hatches. Their names are
  intentionally uncomfortable.

## Public formats

- [Flow JSON Schema](schema/flow.schema.json)
- [Durable log-event JSON Schema](schema/log-event.schema.json)
- [Status snapshot JSON Schema](schema/status.schema.json)

All current public formats use `schema_version`/`api_version` 1. Readers should
ignore unknown object fields so additive changes remain compatible.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), and
[SUPPORT.md](SUPPORT.md). The project is MIT licensed.
