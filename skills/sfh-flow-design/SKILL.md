---
name: sfh-flow-design
description: >
  Design, write, or refactor SimpleFlowHarness (sfh) YAML workflows. Use when turning a task, engineering process, review loop, CI process, research process, or tool chain into a stable sfh flow; when choosing steps, routes, workspaces, contexts, profiles, retries, budgets, and terminal states; or when an AI-authored sfh flow is vague, brittle, or uses invented syntax.
compatibility: Requires SimpleFlowHarness v1.5 syntax for the bundled examples. Use the installed sfh guide and schema as the final authority.
metadata:
  version: "1.0.0"
  target-sfh: "1.5.x"
---

# Design sfh flows as explicit state machines

Use this skill before writing or substantially changing an sfh YAML file.

This is an **authoring skill**. Do not add a `skills:` key to the generated flow: sfh v1.5 has no such key. Runtime instructions belong in existing `contexts:` or in provider-native configuration.

## Start with the execution contract

Before YAML, write down:

1. Goal and terminal success evidence.
2. Inputs and which may change during the run.
3. Side effects: read, workspace write, external mutation, or unknown.
4. Deterministic facts versus semantic judgments.
5. Required artifacts and handoffs.
6. Failure classes and whether another attempt is safe.
7. Loop progress signal and hard stop.
8. Workspace, context, model/profile, time, step, and cost limits.
9. Human decisions or irreversible operations.

If a material item is unknown, expose it as an assumption or route to `stuck`; do not hide it in a prompt.

## Build the state machine

- Each step should have one mechanical responsibility.
- Use `cmd:` for observable facts: builds, tests, schema validation, exact API status, parsing, checksums.
- Use an AI preset for synthesis, planning, diagnosis, implementation, or semantic review.
- Put a deterministic gate after nondeterministic generation whenever possible.
- Use `end`, `fail`, and `stuck` deliberately. `stuck` means work is preserved but sfh cannot safely decide.
- Keep route conditions local to the step whose evidence they inspect.
- Every cycle must have a visit bound and an escalation path.

Read [references/flow-design-method.md](references/flow-design-method.md) and [references/primitive-reference.md](references/primitive-reference.md) when designing a nontrivial flow.

## Separate deterministic and nondeterministic work

Classify each step:

- **Deterministic gate:** a command maps declared inputs to a machine-checkable result.
- **Nondeterministic worker:** an LLM, web search, remote service, or evaluator may vary.
- **External effect:** a call may mutate state outside the workspace.

Do not ask an AI whether a command exited successfully. Do not interpret test names or result payloads as transport errors. Freeze remote/web evidence before asking an AI to reason over it.

For detailed routing choices, activate `sfh-deterministic-gates`.

## Choose workspace and context explicitly

- Read-only flow: `workspace.mode: current` is often enough.
- Any writer: prefer `workspace.mode: auto` or `git-worktree` so one run owns one worktree.
- Do not run parallel writers in one workspace unless the flow explicitly accepts the race.
- Use named context for task contracts, acceptance criteria, review rules, and bounded handoffs.
- Give each role only the context it needs. Avoid copying the entire conversation.
- Treat native sessions as caches; artifacts and workspace state are durable truth.
- Treat context files as immutable during a running flow. If another process may edit them, snapshot them first.

Activate `sfh-context-workspace` for detailed choices.

## Design failure behavior, not only the happy path

- `retry` handles another attempt of the same invocation.
- `fallback` changes the profile/tool after retries.
- a route revisit is a domain improvement loop.
- `replay.unfinished` decides what resume does after a crash without a durable end.
- `--resume` continues an unchanged run.
- `--carry-budget-from` starts a corrected flow without erasing previous spend.
- external mutation normally needs `effects: external`, no blind logical retry, and `replay.unfinished: stuck`.

Activate `sfh-failure-recovery` when any of these matter.

## Authoring workflow

1. Inspect `sfh guide`, `sfh --help`, and the installed flow schema.
2. Draft the state graph in plain text.
3. Write the smallest YAML that expresses it.
4. Run `scripts/lint_sfh_flow.py FLOW` for design warnings.
5. Run `sfh validate FLOW --strict`.
6. Run `sfh preflight FLOW --json`.
7. Run `sfh plan FLOW --json --save` and inspect commands, context, workspace, warnings, and maximum work.
8. Correct the flow before any paid or external step runs.

## Output requirements

When asked to design a flow, return:

1. The YAML.
2. Assumptions and project-specific commands that must be replaced.
3. Why each loop terminates.
4. Which steps are deterministic, nondeterministic, and externally effectful.
5. Validation/preflight/plan commands.
6. Remaining risks or human gates.

Never invent an sfh field. If the installed version is unknown, say which keys require verification.

See [references/anti-patterns.md](references/anti-patterns.md) before finalizing. Start from [assets/stable-flow-skeleton.yaml](assets/stable-flow-skeleton.yaml) rather than an unbounded conversational chain.
