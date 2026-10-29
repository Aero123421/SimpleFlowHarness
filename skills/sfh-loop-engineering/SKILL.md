---
name: sfh-loop-engineering
description: >-
  Design bounded feedback cycles in SimpleFlowHarness. Use for review/fix or planner/worker/evaluator loops, repeated route visits, or a loop that stalls.
compatibility: SFH v2.0; use the installed guide and schema for exact syntax.
metadata:
  version: "1.1.0"
  target-sfh: "2.0.x"
---

# Close the loop with evidence

SFH can run a worker → check → evaluator → correction loop. The check or evaluator supplies the verdict; SFH follows the declared route. Keep acceptance criteria fixed and pass current artifacts and blockers between visits.

Start from [the bounded evaluator loop](assets/bounded-evaluator-loop.yaml) with its task/acceptance files. Reuse one workspace, check after each write, and give the evaluator current evidence rather than relying on the writer's self-assessment. Replace the sample `git diff --check` with the project's acceptance checks.

- Use a route revisit for REVISE, `retry` for a safe repeat of the invocation, `fallback` for a different profile, and `replay.unfinished` for interrupted work on resume.
- Bound revisited steps with `max_visits` and `on_max_visits: goto:handoff`. Add a total-step or wall-clock ceiling; preserve blockers and land on `stuck` when exhausted. Reserve time for the full `on_budget` handoff.
- `max_total_steps` counts logical leaf runs, not their retry attempts. Inspect `plan`'s `max_attempts`; cost limits cover reported USD only.
- Separate a failed acceptance check from a check that could not run; map only documented outcomes to correction. Pass the check evidence and current evaluator blockers to both correction and handoff.
- Route exact evaluator verdicts or validated wrapper outcomes; send unknown verdicts to `stuck`. SFH does not infer progress from prose. If repeated blockers should stop the loop early, encode that decision in an evaluator/check and route it.
- Parallelize independent evaluators; serialize overlapping writers. Use `when_members` for exact member votes, with child `on_error: continue` when the group must inspect failures. Failed members do not vote.
- Long work can use sequential chunks, artifacts and bounded handoffs. Session continuation and `compact` do not replace durable evidence.

For other shapes, use [loop patterns](references/stable-loop-patterns.md); for oscillation or stale feedback, use [convergence](references/convergence.md). Validate and dry-run the resulting graph, including rejection and exhaustion paths.
