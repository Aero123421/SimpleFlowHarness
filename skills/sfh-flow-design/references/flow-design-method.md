# Flow design method

## 1. Normalize the request

Turn prose into a contract:

```text
inputs
preconditions
work to perform
acceptance evidence
side effects
failure policy
limits
human decisions
```

Do not start with agent roles. Start with facts and state transitions. Roles are replaceable profiles; state semantics are not.

## 2. Draw the state graph

For each node, record:

```text
id
kind: command | AI | parallel | foreach
reads
writes
external effects
evidence produced
success route
failure route
crash replay policy
```

A state with no meaningful evidence should usually be merged with a neighbor or removed.

## 3. Classify evidence

### Mechanically observable

Use commands and exit/status data:

- compiler/build/test result
- schema validation
- formatter/linter
- file existence or hash
- exact CI run conclusion
- JSON parsing
- migration dry-run result

### Semantic

Use independent AI evaluation with explicit criteria:

- design quality
- requirement coverage not expressible as tests
- maintainability
- threat-model completeness
- documentation clarity

### Mixed

Split it:

```text
nondeterministic generator
→ deterministic verifier
→ semantic evaluator only for the remaining judgment
```

## 4. Define the improvement loop

A stable loop has:

- a fixed contract that does not drift per visit
- current workspace as truth
- a fresh or independent evaluator
- a bounded correction step
- deterministic checks between writer and evaluator
- `max_visits`
- `on_max_visits: goto:handoff` or `stuck`
- a wall-clock/step budget for long work
- a durable handoff containing blockers and next action

Do not use `retry` to handle evaluator rejection. That is a route revisit, not an infrastructure retry.

## 5. Define workspace and context

One change series should normally use one managed worktree for the run. Reviewers read the same workspace; correction visits reuse it.

Context should be role-specific:

```text
worker: task + acceptance + implementation rules + latest blockers
reviewer: task + acceptance + review rules + current evidence
```

Avoid feeding the implementer's self-assessment to the reviewer as primary evidence.

## 6. Define external tool boundaries

For every web/API/MCP/CI step, decide:

- exact tool and binary
- exact target identity (URL, repo, run ID, server, tool name)
- read versus mutation
- credentials source (never YAML or `--var` for secrets)
- replay/idempotency
- rate limits and timeout
- raw response artifact and validation
- how untrusted content reaches an AI

## 7. Define limits

At minimum for loops and remote calls:

- `max_visits`
- `max_total_steps`
- `timeout_sec`
- `wall_clock_sec`
- `on_budget` plus nonzero reserve when a landing chain exists
- `max_prompt_chars`
- `max_context_chars`
- bounded parallelism

## 8. Validate before execution

Validation sequence:

```text
heuristic lint
sfh validate --strict
sfh preflight --json
sfh plan --json --save
human/agent review of the rendered plan
run
```

A plan warning is not a decorative message. Resolve it or record why the risk is accepted.
