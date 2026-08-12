---
name: sfh-eval-engineering
description: >
  Turn failures, regressions, flaky behavior, weak prompts, and unsuccessful sfh runs into reusable evaluations and better harnesses. Use when improving a flow through evidence, creating regression-first repair loops, separating quality and regression suites, promoting repeated review feedback into deterministic checks or repository guidance, or diagnosing why an agent loop keeps failing.
compatibility: Uses sfh v1.6 commands, contexts, artifacts, loops, budgets, and recovery semantics.
metadata:
  version: "1.0.0"
  target-sfh: "1.6.x"
---

# Improve the harness from failures, not from prompt inflation

When an sfh run struggles, classify what was missing:

- a deterministic test/eval
- tool access or observability
- repository/context knowledge
- a trust boundary
- a clearer acceptance contract
- a recovery rule
- a smaller chunk
- an independent evaluator

Do not default to a longer prompt or more retries.

Read [references/failure-to-eval.md](references/failure-to-eval.md).

## Regression-first loop

```text
preserve failure evidence
→ create minimal reproduction/eval
→ prove it fails for the intended reason
→ implement smallest fix
→ prove focused eval passes
→ run broader regression suite
→ independent review
→ keep the eval permanently
```

The reproduction step is deterministic whenever possible. An AI may design it, but a command must prove the before/after behavior.

## Flow-level improvement

When the YAML itself failed:

1. use `runs why`, artifacts, and exact route history
2. identify whether the problem was flow definition, tool drift, context, environment, or work quality
3. add a fixture or static lint rule reproducing the flow failure
4. fix the YAML/tooling
5. start a corrected flow with carried budget when appropriate
6. retain the failing case as an example/eval

## Quality versus regression

- **quality suite:** broader cases that measure current capability and trade-offs
- **regression suite:** focused cases that must never break again

Do not let one giant flaky suite be the only gate. Use focused evidence early and broad gates before completion/release.

## Encode recurring feedback

If the same reviewer comment appears repeatedly, promote it to one of:

- deterministic linter/test
- schema/static validation
- small repository document/context
- Agent Skill reference
- reusable flow example

This reduces entropy and future prompt load.

Start from [assets/failure-to-regression.yaml](assets/failure-to-regression.yaml).
