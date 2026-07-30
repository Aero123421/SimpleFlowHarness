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

Download a binary from [GitHub Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest),
verify its matching SHA-256 file, and install it on `PATH`.

```powershell
# Windows x64
$asset = "sfh-windows-x64.zip"
$installDir = Join-Path $env:LOCALAPPDATA "Programs\sfh"
irm "https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/$asset" -OutFile $asset
irm "https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/$asset.sha256" -OutFile "$asset.sha256"
$expected = ((Get-Content "$asset.sha256" -Raw) -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash $asset -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw "SHA-256 mismatch: expected $expected, got $actual" }
New-Item -ItemType Directory -Force $installDir | Out-Null
Expand-Archive $asset -DestinationPath $installDir -Force
$userPath = [string][Environment]::GetEnvironmentVariable("Path", "User")
if (-not (($userPath -split ';') -contains $installDir)) {
  $newUserPath = if ([string]::IsNullOrWhiteSpace($userPath)) { $installDir } else { "$userPath;$installDir" }
  [Environment]::SetEnvironmentVariable("Path", $newUserPath, "User")
}
$env:Path = "$installDir;$env:Path"
sfh --version
Remove-Item $asset, "$asset.sha256"
```

If SmartScreen blocks the downloaded binary, choose **More info → Run anyway**.

```bash
# Linux x64; substitute the macOS or Linux arm64 asset when needed
asset=sfh-linux-x64.tar.gz
base=https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download
curl -fLO "$base/$asset"
curl -fLO "$base/$asset.sha256"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "$asset.sha256"
else
  shasum -a 256 -c "$asset.sha256"
fi
mkdir -p "$HOME/.local/bin"
tar xzf "$asset" -C "$HOME/.local/bin" sfh
chmod +x "$HOME/.local/bin/sfh"
export PATH="$HOME/.local/bin:$PATH" # add this line to your shell profile for future shells
sfh --version
rm "$asset" "$asset.sha256"
```

For a browser-downloaded macOS binary, remove its quarantine attribute with
`xattr -dr com.apple.quarantine "$HOME/.local/bin/sfh"` (not needed for `curl`).

With Rust:

```bash
cargo install --git https://github.com/Aero123421/SimpleFlowHarness --tag v1.1.3 --locked
```

Rerun the same procedure to update. If an older executable still wins, inspect
all `PATH` matches with `Get-Command sfh -All` in PowerShell or `type -a sfh`
on Unix.

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
Use `{{prompt_file}}` when a command should read the fully rendered, possibly
multiline prompt without putting it through shell quoting:

```yaml
- id: analyze
  cmd: ["python", "analyze.py", "{{prompt_file}}"]
  prompt: |
    Analyze this input:
    {{steps.gather.output}}
```

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

With `foreach.split: json`, a clean JSON array is preferred. If prose surrounds
the data, sfh selects the last complete, parseable JSON array. This handles
citation text such as `[1]` before the final array without joining unrelated
brackets. String elements become their text; numbers, arrays, and objects
become compact JSON text in each `{{item}}`.

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
sfh run flow.yaml [--detach] [--resume DIR] [--run-dir DIR]
sfh status [RUN] [--json]
sfh wait [RUN] [--timeout SEC]
sfh stop [RUN]
sfh help [COMMAND]
sfh runs list|show|why|clean ...
```

`--run-dir` pins a deterministic artifact directory for advanced CI or nested
flows. Use a new or empty path; normal runs should prefer `--runs-dir`.

`sfh status` includes active fan-out members and completed/total counts.
`sfh runs why` reconstructs the last durable position, unfinished leaves, and
what a resume will rerun.

Human-readable `sfh status` is one ordered stdout document. Scripts should use
`status --json`. A successful `sfh wait` writes only the flow result to stdout;
it does not append a completion footer that could corrupt a pipeline.

`sfh config show` prints the fully merged effective configuration but redacts
every environment value by default. Use `--show-secrets` only when the actual
values are required for local diagnosis; that output is sensitive and must not
be pasted into public issue reports.

### Exit codes

| Command | Code | Meaning |
|---|---:|---|
| `run` / `status` / `wait` | 0 | Flow completed successfully; status observed `done` |
| `run` / `status` / `wait` | 1 | Flow failed, or status observed `failed` / `dead` / `stopped` |
| any command | 2 | Configuration or CLI usage error |
| `status` / timed `wait` | 3 | Still running; a wait timeout never cancels the run |
| `run` / `status` / `wait` | 4 | Flow routed to `stuck`; work is saved and needs a human |

Use exit 4 separately from an infrastructure failure when CI should hand saved
work to a reviewer.

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
