---
name: sfh-flow-design
description: >-
  Write or refactor SimpleFlowHarness YAML from a task. Use for a new flow or a change to its overall execution graph.
compatibility: SFH v2.0; use the installed guide and schema for exact syntax.
metadata:
  version: "1.1.0"
  target-sfh: "2.0.x"
---

# Write an SFH flow

Use `sfh guide` and the installed flow schema for syntax. These are authoring instructions; SFH has no `skills:` field. Pass runtime instructions through `contexts:` or the selected CLI's native configuration.

SFH records process facts and follows declared routes. It does not judge work quality. Put acceptance checks in commands or an explicit evaluator, then route on their evidence. This supports closed feedback loops without making the engine the judge.

- Choose `cmd:` for mechanical checks and a CLI profile for agent work. If the artifact is the product, use `allow_empty: true` and verify that artifact; this never waives the adapter's completion contract.
- Choose the workspace explicitly. One managed `git-worktree` is reused across visits; serialize writers. Keep run evidence outside a tree the worker can change.
- Declare effects and error paths. Use `end` for accepted completion, `fail` for failure, `stuck` for an unresolved decision. External effects with uncertain results need inspection before repetition.
- Bound cycles with `max_visits` / `on_max_visits` and total work with `max_total_steps` or wall-clock limits. A cost ceiling sees only adapter-reported USD.
- Keep retry (same invocation), fallback (another profile), route revisit (new improvement pass), and replay (unfinished work on resume) distinct.

Start with [the skeleton](assets/stable-flow-skeleton.yaml) and its adjacent context files; replace project-specific checks. Check the resulting flow with `sfh validate FLOW --strict` and `sfh run FLOW --dry-run`. Before live execution, `sfh preflight FLOW --json` checks local capabilities and `sfh plan FLOW --json` exposes resolved commands and limits. Neither proves authenticated success.

Use only the detail the task needs:

- [Primitive map](references/primitive-reference.md): keys and commands.
- [Design choices](references/flow-design-method.md): evidence, state and boundaries.
- [Anti-patterns](references/anti-patterns.md): suspicious control flow; [linter](scripts/lint_sfh_flow.py) offers heuristic warnings.
- `sfh-loop-engineering` for feedback cycles; `sfh-failure-recovery` for replay/resume; other specialists only when their problem occurs.

Return usable YAML with required inputs, meaningful assumptions and validation results. Identify unresolved gates instead of implying they passed.
