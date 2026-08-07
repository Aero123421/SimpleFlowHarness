# sfh — SimpleFlowHarness

[![ci](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml/badge.svg)](https://github.com/Aero123421/SimpleFlowHarness/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/Aero123421/SimpleFlowHarness)](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)
[![license](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

English | [日本語](README.ja.md)

`sfh` is a lightweight, single-binary workflow runner for AI coding CLIs and shell commands. It orchestrates AI agents—including **Codex**, **Claude Code**, **opencode**, **Grok**, **Antigravity (`agy`)**, **Pi**, and **Cursor**—or arbitrary executables into YAML-defined multi-step flows.

The engine handles execution plumbing, routing, process lifecycle management, retries, and audit logging. It records process facts and follows declared routes, leaving task judgment to your commands and agents.

---

## Why sfh?

- **Context Preservation**: Move multi-step agent orchestration loops out of your primary AI agent's context window.
- **Clean Output & Full Audit**: Progress stays on `stderr`; the selected result is emitted to `stdout`. Prompts, step output, token counts, reported costs, and event logs are saved under `.sfh/runs/`.
- **Background Execution**: Run long-running flows detached (`--detach`), then monitor status (`sfh status`), await results (`sfh wait`), or stop execution (`sfh stop`).
- **Resilient Fan-out & Recovery**: Run parallel workers (`parallel`) or data-driven loops (`foreach`). Resume crashed or interrupted flows (`--resume`) without re-executing completed steps.
- **Safety & Budget Controls**: Set execution boundaries with soft cost limits (`max_cost_usd`), wall-clock deadlines (`wall_clock_sec`), and explicit access scopes (`access: read | write | full`).

---

## Installation

### Official One-Line Installers (No Package Manager Required)

**macOS / Linux:**
```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-installer.sh | sh
```

**Windows PowerShell:**
```powershell
irm https://github.com/Aero123421/SimpleFlowHarness/releases/latest/download/sfh-installer.ps1 | iex
```

The installer detects your OS and architecture, verifies the SHA-256 checksum, extracts the binary, and updates your `PATH`.
You can inspect the [Shell](installers/sfh-installer.sh) and [PowerShell](installers/sfh-installer.ps1) scripts prior to execution.
To pin a specific version or customize installation behavior:
- `SFH_VERSION=1.1.5`: Pin the target release version.
- `SFH_INSTALL_DIR=/path/to/bin`: Specify a custom installation directory.
- `SFH_NO_MODIFY_PATH=1`: Skip automatic `PATH` modifications.

### Package Managers & Direct Downloads

**Homebrew (macOS / Linux):**
```bash
brew install Aero123421/tap/sfh
```

Pre-built binaries and SHA-256 checksums are available on [GitHub Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest).

---

## Quick Start

Create a minimal workflow file (`flow.yaml`) combining a deterministic test command with an AI fix step:

```yaml
api_version: 1
name: test_and_repair
defaults:
  max_visits: 3
  wall_clock_sec: 1800

steps:
  - id: test
    cmd: ["cargo", "test"]
    on_error: goto:fix

  - id: ship
    cmd: ["sfh", "--version"]
    route: [{goto: end}]

  - id: fix
    tool: codex
    access: write
    prompt: |
      Open this stderr file, diagnose the failure, and fix it:
      {{steps.test.stderr_file}}
    route: [{goto: test}]
```

### Validate and Execute

```bash
# 1. Statically validate syntax, variables, and control-flow routes
sfh validate flow.yaml --strict

# 2. Preview the execution plan in an isolated temporary directory
sfh plan flow.yaml

# 3. Run the workflow
sfh run flow.yaml
```

---

## Core Concepts & Mental Model

### Step Types & AI Tools

`sfh` supports two step types:
1. **AI Tool Steps**: Built-in presets for `codex`, `claude`, `opencode`, `grok`, `agy`, `pi`, and `cursor`. AI steps require an explicit access level (`access: read | write | full`).
2. **Deterministic Commands (`cmd`)**: Executed directly without shell wrapping when passed as an array (`["cargo", "test"]`). String commands run via `sh -c` (Unix) or `cmd /C` (Windows). Use `{{prompt_file}}` to pass rendered prompt text without shell quoting issues.

### Control Flow & Routing

Each step evaluates `route` rules sequentially to determine the next step:
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
Available targets include `<step-id>`, `goto:end` (exit 0), `goto:fail` (exit 1), and `goto:stuck` (exit 4, requiring human intervention).

### Parallel Fan-out & Consensus

- **`parallel`**: Runs heterogeneous worker sub-steps concurrently.
- **`foreach`**: Fans out dynamically over lines or JSON arrays (`split: lines | json`).
- **`when_members`**: Counts consensus votes across fan-out members based on exact last-line matches and clean exit status (`exit == 0`):

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

### Sessions: Continuation & Forking

- **`continue_from: step_id`**: Continues a single server-side session from a prior step.
- **`fork_from: step_id`**: Branches an independent child session from a parent step (supported on `claude`, `opencode`, `grok`, and `pi`).

### Detached Runs & Operations

Run long tasks in the background without blocking your terminal or parent process:
```bash
# Launch a background run (prints its run directory and exits)
sfh run flow.yaml --detach

# Commands without a path use the newest run
sfh status --json

# Wait for completion and receive final stdout result
sfh wait --timeout 3600

# Stop a running flow and clean up process trees
sfh stop
```

### Durability & Resumption

Interrupted runs are saved in `.sfh/runs/<run_dir>`. If a run fails or is stopped, resume it with:
```bash
sfh run flow.yaml --resume-latest
```
`sfh` verifies flow file integrity and configuration compatibility before resuming, skipping already completed steps.

### Exit Codes

| Exit Code | Meaning |
| :---: | :--- |
| `0` | Flow completed successfully (`goto:end`) or status observed `done`. |
| `1` | Flow failed (`goto:fail`), tool error, or status observed `failed` / `dead` / `stopped`. |
| `2` | Configuration error, invalid CLI flags, or static validation failure. |
| `3` | Flow is still running (returned by `status` or timed-out `wait`). |
| `4` | Flow routed to `stuck` (`goto:stuck`), awaiting human intervention. |

---

## Artifacts & Public Schemas

Every run generates durable, append-only records inside `.sfh/runs/<run-id>/`:
- `log.jsonl`: Structured event stream (step start, completion, token usage, cost).
- `<step_id>.out.txt` & `<step_id>.err.txt`: Bounded raw stdout/stderr snapshots. Streams over 32 MiB retain their head and tail with an omission marker; structured final answers and accounting are processed independently from the complete stream.
- `status.json`: Real-time status snapshot.

When forwarding a command's verbose output into an AI prompt, prefer an explicit bound such as `{{steps.verify.output | tail:80 | truncate:8000}}`; the full artifact remains available through `{{steps.verify.output_file}}`.

Public JSON Schemas:
- [Flow JSON Schema](schema/flow.schema.json)
- [Durable Log Event JSON Schema](schema/log-event.schema.json)
- [Status Snapshot JSON Schema](schema/status.schema.json)

---

## Progressive Disclosure & Documentation

For complete reference material, explore:
- Built-in syntax guide: Run `sfh guide` in your terminal.
- CLI options: Run `sfh --help` or `sfh <command> --help`.
- Sample workflows: Browse [examples/](examples/) (`research.yaml`, `hypotheses.yaml`, `parallel-ideas.yaml`).
- Project governance & policies:
  - [CONTRIBUTING.md](CONTRIBUTING.md)
  - [SECURITY.md](SECURITY.md)
  - [SUPPORT.md](SUPPORT.md)
  - [LICENSE](LICENSE)
  - [Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)

Supported Platforms: **Windows**, **macOS**, **Linux**
