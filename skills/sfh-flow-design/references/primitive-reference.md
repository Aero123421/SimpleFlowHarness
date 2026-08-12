# sfh v1.5 primitive reference for authors

Use the installed schema as final authority. This file is a design map, not a replacement for `sfh guide`.

## Top level

- `api_version: 1`
- `name`
- `vars`
- `defaults`
- `profiles`
- `workspace`
- `contexts`
- `steps`

## Step execution

- preset AI: `tool`, `use`, `model`, `effort`, `access`, `agent`, `args`
- command: `cmd` as argv array preferred; string form invokes a shell
- `stdin: prompt` for command prompt input
- `cwd`, `env`, `env_remove`, `timeout_sec`
- `effects: read | workspace | external | unknown`

## Control

- `route`
- `on_error: fail | continue | goto:<id>`
- `max_visits`, `on_max_visits`
- `parallel`, `foreach`, `max_parallel`
- terminals: `end`, `fail`, `stuck`

## Deterministic routing

- `when_exit`
- `when_stderr_matches`
- `outcomes` mapping raw process exit codes to `complete | continue | retryable | fail`
- `when_label_is`
- `when_outcome_is`
- `when_members` for exact per-member votes

Text predicates are available but weaker for contracts:

- `when_contains`, `when_matches`
- `when_last_line_contains`, `when_last_line_is`, `when_last_line_matches`

`when_label_is` only helps when a command or wrapper maps the semantic result to an exit code. A normal AI CLI generally exits 0 for both PASS and REVISE, so `outcomes` cannot magically distinguish its prose.

## Recovery

- `retry`, `retry_on`, `hang_after_sec`
- `fallback`
- `replay.unfinished: rerun | stuck | fail`
- `continue_from`, `fork_from`
- CLI: `--resume`, `--force-resume`, `--adopt-workspace`, `--carry-budget-from`

## Workspace

- `current`
- `directory`
- `git-worktree`
- `auto`

A managed run owns at most one worktree in v1.5. Parallel writers share that worktree and are refused unless explicitly allowed.

## Context

Named source:

- `file`
- `inline`
- `template`
- optional `max_chars`, `optional`, `allow_external`

Step:

- `context: [name, ...]`
- `context_delivery: prepend | file`

## Limits and evidence

- `max_total_steps`
- `max_prompt_chars`
- `max_context_chars`
- `max_cost_usd`
- `wall_clock_sec`
- `on_budget`, `budget_reserve`
- `notes: append`
- `compact`

## Inspection commands

```text
sfh validate --strict
sfh preflight --json
sfh plan --json --save
sfh graph --mermaid
sfh status / wait / stop
sfh runs list / show / why
sfh doctor
```
