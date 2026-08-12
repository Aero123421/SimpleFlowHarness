---
name: sfh-loop-engineering
description: >
  Design stable, bounded SimpleFlowHarness loops: review/fix cycles, planner-builder-evaluator systems, long-running chunked work, multi-agent councils, and iterative improvement. Use when a route revisits a step, an AI must improve work over several laps, a run may last hours, or an existing sfh loop oscillates, repeats work, loses context, or never converges.
compatibility: Written for sfh v1.6 loop, route, fan-out, budget, context, and resume semantics.
metadata:
  version: "1.0.0"
  target-sfh: "1.6.x"
---

# Engineer loops that make progress or stop honestly

A loop is not “call the model again.” It is a state machine with a progress contract.

## Required loop contract

Before YAML, name:

- invariant that must remain true every lap
- evidence that changes when progress occurs
- evaluator and its independence from the writer
- deterministic checks between write and evaluation
- maximum visits and total work
- budget landing and durable handoff
- behavior when the same blocker persists
- behavior after a crash mid-step

Read [references/stable-loop-patterns.md](references/stable-loop-patterns.md) for patterns and [references/convergence.md](references/convergence.md) for failure modes.

## Canonical improvement loop

```text
fixed task contract
→ writer changes current workspace
→ deterministic checks
→ independent evaluator
→ PASS: end
→ REVISE: smallest correction
→ repeat under explicit bound
→ bound/budget exhausted: durable handoff → stuck
```

Use the same workspace for one change series. Do not create a new worktree per visit. Give the reviewer current files and evidence, not only the writer's story.

## Distinguish four repetitions

- **retry:** same invocation after transient transport/infra failure
- **fallback:** different profile/tool after retries
- **revisit:** domain loop because work is not accepted yet
- **resume replay:** crash recovery for work without a durable end

Never use retry to “roll the dice again” on a logical rejection.

## Prevent oscillation

- Keep acceptance criteria immutable during the run.
- Carry current unresolved findings, not every historical review.
- Ask corrections to close named blockers and avoid unrelated rewrites.
- Re-run deterministic checks after each write.
- Prefer a fresh evaluator session when independence matters.
- Set `max_visits` on the revisited writer and `on_max_visits: goto:handoff`.
- Use `max_total_steps` and wall-clock landing as a second ceiling.
- If output repeats without workspace/evidence progress, hand off rather than enlarge prompts indefinitely.

## Long-running work

Chunk work into dependency-ordered units with verifiable completion. Serialize writers in one workspace. Use structured artifacts and `notes`/named context to hand off between phases. `compact` is a transport optimization, not the source of truth; retain the original artifact.

## Fan-out

Parallelize independent readers/evaluators, not overlapping writers. For votes, use `when_members`; failed members do not vote. Give each child `on_error: continue` when the group route must inspect all verdicts.

## Final checks

A loop is acceptable only if you can answer:

1. What improves per lap?
2. Who verifies it?
3. What machine evidence is refreshed?
4. What stops the loop?
5. What survives a crash?
6. What happens when progress is ambiguous?

Start from [assets/bounded-evaluator-loop.yaml](assets/bounded-evaluator-loop.yaml).
