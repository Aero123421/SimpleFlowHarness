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
- **Fail-Closed Protocols**: Every preset tool must complete its documented machine-readable protocol. A CLI that prints an error and exits, or whose output shape has drifted, fails the step instead of having its text passed on as an answer.
- **Machine Interface**: `--json` on `run`, `plan`, `wait`, `stop`, `status`, `preflight` and `workspaces`. stdout carries the envelope and nothing else, and failures carry stable `SFH_*` codes you can branch on.

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
- `SFH_VERSION=1.2.0`: Pin the target release version.
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

Since v1.2 the check covers the whole **execution closure** — everything outside the flow file that decides what the run does: profile overlays, the contents of any context files, the resolved tool versions, the workspace mode and base commit, and the set of explicitly accepted risks. Each is hashed into `execution-closure.json` at run start. If any of them moved, the resume is refused and the changed entries are named:

```text
SFH_EXECUTION_CLOSURE_CHANGED: the execution closure changed since this run started
  context.task: sha256:9bc86fb26f3c -> sha256:d1811c82b034
```

`--force-resume` accepts that deliberately and records a `force_resume` event. It is a separate question from `--adopt-workspace` (below), and one flag never waives the other.

---

## Workspace, Context and Replay (v1.2)

Three opt-in mechanisms. **Omit all of them and nothing changes**: your flow runs in the caller's working directory, writes to `.sfh/runs`, and resumes exactly as it did in 1.1.

### Managed workspace

`workspace:` says where a run's side effects belong.

```yaml
workspace:
  mode: auto        # current | directory | git-worktree | auto
  cleanup: auto     # auto | keep
```

`auto` decides from the flow's declared `effects:` alone — it never guesses from your prompts. If every step is `effects: read`, no workspace is created. If any step may write, the run gets **exactly one** Git worktree for its entire life, however many steps it has and however many times a loop revisits them. Later steps see earlier steps' changes because it is the same directory.

The worktree is created outside the repository it branches from (under `--state-dir`, or the platform user-state directory), on a branch named `sfh/<flow>/<run-id>`. Your own checkout is untouched.

Two rules are absolute:

- **sfh deletes only what sfh created.** Removal requires an ownership marker inside the directory and a matching nonce in the run's manifest, re-checked immediately before the deletion. A directory that fails either check is kept, with a warning — never removed.
- **Uncommitted work is never discarded automatically.** A dirty workspace survives cleanup whatever the run's outcome, and the branch is never deleted. `sfh workspaces remove <run-dir> --discard` is the only path that drops changes, and a human has to type it.

Failed, stuck, stopped and dead runs always keep their workspace: that is where the evidence is.

```bash
sfh workspaces list --state-dir ~/.local/state/sfh
sfh workspaces show <run-dir> --json
sfh workspaces clean --older-than 30 --dry-run
```

On resume, sfh fingerprints the workspace (HEAD, staged and unstaged changes, every untracked file hashed, submodule state) and compares it to the run's last durable checkpoint. A difference that no unfinished step explains means something outside the run edited it, and the resume is refused. `--adopt-workspace` accepts the current contents as the new baseline and writes a `workspace_adopted` event.

### `effects:` — what a step touches

```yaml
- id: deploy
  effects: external      # read | workspace | external | unknown
```

A declaration, not an inference. Omitted, it is derived from `access:` for a preset step and is `unknown` for a custom `cmd:` — which counts as a potential writer, because assuming otherwise is the assumption that loses somebody's work. It decides workspace selection, static warnings and replay policy, and nothing else.

### Named context

`contexts:` pins what a step was handed: which sources, in what order, at what hash.

```yaml
contexts:
  task:          {file: ./TASK.md}
  house_rules:   {inline: "prefer the design that is already there"}
  latest_review: {template: "{{steps.review.output | optional}}"}

steps:
  - id: implement
    context: [task, house_rules, latest_review]
    context_delivery: prepend        # prepend (default) | file
```

The assembled bundle is saved as `<tag>.context.txt` with a manifest in `<tag>.context.json` recording each source's origin, hash and size. The durable log records the hash, never the content. `{{context}}` and `{{context_file}}` are available in both delivery modes.

Context files are read no-follow and must resolve inside the flow directory or the workspace; a symlink pointing out of them is refused. `allow_external: true` on a source is the only way out, and using it is recorded as an unsafe override. A bundle over `defaults.max_context_chars` fails **before anything is spawned** — sfh never summarizes or drops sources to make it fit. What to leave out is your decision, expressed with a template filter, a `max_chars`, or an upstream `compact:`.

### Replay policy

What a resume should do with a step that started and never recorded an end — the one case where sfh genuinely cannot know whether the work already happened.

```yaml
defaults:
  replay: {unfinished: rerun}    # rerun (default) | stuck | fail

steps:
  - id: deploy
    effects: external
    replay: {unfinished: stuck}
```

`rerun` is the default and is what every earlier release did. `stuck` (exit 4) and `fail` (exit 1) launch nothing at all, keep the workspace and every partial artifact, and answer with `SFH_REPLAY_REFUSED`.

This is not retry (another attempt at the same invocation), not fallback (a different profile), not a route revisit, and not the reuse of a completed step's result. sfh does not promise exactly-once for external effects; it promises not to silently re-run one, and not to call an uncertain outcome a success.

### Portable flows: `--profiles`

A shared flow can name roles instead of tools, and let whoever runs it decide:

```yaml
steps:
  - id: review
    use: judge          # no tool, no model, no binary in the flow
```

```bash
sfh run flow.yaml --profiles team.yaml --profiles my-machine.yaml
```

Repeatable, later wins. An overlay replaces only the fields it mentions — `args` is replaced when present and preserved when absent, `env` merges key by key. Precedence: step field > `--profiles` overlay > flow inline profile > `~/.sfh/profiles.yaml` > defaults. Writing `tool:` straight into a step keeps working exactly as before; an overlay file is never required.

### State root

```bash
sfh run flow.yaml --state-dir ~/.local/state/sfh     # or SFH_STATE_DIR
```

Puts `runs`, `workspaces`, `plans` and `doctor` under one directory. `--runs-dir` still moves run artifacts and only those, and with neither flag runs still land in `.sfh/runs`. A managed workspace with no state root falls back to the platform user-state directory (`$XDG_STATE_HOME/sfh`, `$HOME/.local/state/sfh`, `%LOCALAPPDATA%\sfh`) and errors rather than writing inside your repository if none can be determined.

---

## Driving sfh from a program

`--json` makes stdout an envelope and nothing else — progress and warnings go to stderr, and a configuration error is still an envelope rather than prose.

```bash
sfh preflight flow.yaml --json          # free: no model calls
sfh plan      flow.yaml --json --save   # what would run; starts nothing
sfh run       flow.yaml --json --detach # returns a handle plus next_actions
sfh wait <run-dir> --json               # blocks, then the result
```

```json
{
  "schema_version": 1,
  "command": "run",
  "ok": true,
  "state": "done",
  "terminal": true,
  "exit_code": 0,
  "run_id": "20260808-120000-flow",
  "run_dir": "/...",
  "result": "...",
  "result_file": "/.../review.chain.txt",
  "error": null,
  "next_actions": [{"kind": "why", "argv": ["sfh", "runs", "why", "/...", "--json"]}]
}
```

Failures carry a code whose meaning is fixed for all of v1.2.x — branch on the code, not on the message, which is allowed to improve:

`SFH_USAGE`, `SFH_FLOW_INVALID`, `SFH_PROTOCOL_INVALID`, `SFH_TERMINAL_MISSING`, `SFH_SESSION_UNVERIFIED`, `SFH_EXECUTION_CLOSURE_CHANGED`, `SFH_WORKSPACE_MISSING`, `SFH_WORKSPACE_DRIFT`, `SFH_WORKSPACE_BUSY`, `SFH_WORKSPACE_UNOWNED`, `SFH_REPLAY_REFUSED`, `SFH_PERSISTENCE_FAILURE`, `SFH_CAPABILITY_UNAVAILABLE`.

**Always pass the run directory explicitly.** A command given no path selects the newest run and says so with `"implicit_target": true`, which is rarely what an agent wants.

`result` obeys `max_emit_chars`; `result_file` always names the complete text on disk. A detached run answers with `"terminal": false` and the argv that blocks for the answer.

### `preflight` vs `doctor`

```text
sfh preflight  — free. Binary present? version? required flags still in --help?
                 which protocol, session support, cost coverage, access gaps?
                 what workspace and context would this flow build?
sfh doctor     — paid. Sends a real one-token prompt and checks sfh can still
                 parse the answer. The only way to catch protocol drift.
```

`doctor` probes from an isolated scratch directory, so it reports on the adapter rather than on whatever instruction files happen to be in the directory you ran it from.

sfh pins **no minimum version** for any adapter. Rather than assert a floor it has not verified against each CLI's documentation and a live probe, `preflight` reports the installed version and says the requirement is unknown.

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
- `log.jsonl`: Structured event stream (step start, completion, token usage, cost, protocol evidence, workspace checkpoints).
- `<step_id>.out.txt` & `<step_id>.err.txt`: Bounded raw stdout/stderr snapshots. Streams over 32 MiB retain their head and tail with an omission marker; structured final answers and accounting are processed independently from the complete stream.
- `status.json`: Real-time status snapshot.
- `execution-closure.json`: The hashed inputs this run is pinned to.
- `workspace.json`: The managed workspace, when the flow asked for one.
- `<step_id>.context.txt` & `<step_id>.context.json`: The assembled context and its manifest, when the step named any.

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
- Sample workflows: Browse [examples/](examples/) (`research.yaml`, `hypotheses.yaml`, `parallel-ideas.yaml`, `managed-loop.yaml`, `workspace-smoke.yaml`).
- Project governance & policies:
  - [CONTRIBUTING.md](CONTRIBUTING.md)
  - [SECURITY.md](SECURITY.md)
  - [SUPPORT.md](SUPPORT.md)
  - [LICENSE](LICENSE)
  - [Releases](https://github.com/Aero123421/SimpleFlowHarness/releases/latest)

Supported Platforms: **Windows**, **macOS**, **Linux**
