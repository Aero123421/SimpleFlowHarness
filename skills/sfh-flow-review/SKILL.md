---
name: sfh-flow-review
description: >-
  Review an existing SimpleFlowHarness YAML flow. Use to find concrete control-flow, evidence, recovery or portability defects before execution.
compatibility: SFH v2.0; use the installed guide and schema for exact syntax.
metadata:
  version: "1.1.0"
  target-sfh: "2.0.x"
---

# Review the execution graph

Use the installed `sfh guide`/schema. Trace success, rejection, failure, exhaustion and resume paths, including nested parallel/foreach children. Look for false success, missing bounds, repeated uncertain effects and decisions dependent on hidden session state.

Check the parts implicated by the flow: workspace ownership, immutable context/closure, adapter completion evidence, route predicates, retry versus revisit, external target identity and authorization. `access` is provider policy, not proof of OS or MCP isolation.

Use `sfh validate FLOW --strict`, preflight and plan to inspect the contract without paid/mutating leaf execution. State what was actually checked; legal YAML alone does not prove correct behavior.

Return prioritized findings with the step/key, a concrete failure scenario, smallest fix and a way to test it. Keep model-quality judgments separate from engine/process facts. Do not weaken a gate to make the flow pass.

Use [the rubric](references/review-rubric.md) for a broader audit, [portability](references/portability.md) for OS/shell issues, and [the report template](assets/review-output-template.md) when a structured report helps.
