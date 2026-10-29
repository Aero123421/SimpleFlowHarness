---
name: sfh-deterministic-gates
description: >-
  Map evidence to SimpleFlowHarness routes. Use when exit codes, outcomes, labels, AI verdicts or fan-out votes decide the next step.
compatibility: SFH v2.0; use the installed guide and schema for exact syntax.
metadata:
  version: "1.1.0"
  target-sfh: "2.0.x"
---

# Route on the evidence you actually have

SFH records facts; it does not infer quality. A command can check an artifact; an external evaluator can judge it. Make that result explicit before routing.

- `outcomes` maps raw process exit codes to `complete`, `continue`, `retryable` or `fail`; labels give those results domain names. Missing mappings retain normal exit behavior. Protocol failure, timeout, interruption and persistence errors take precedence.
- AI CLI exit 0 does not distinguish PASS from REVISE. For structured verdicts, validate them with a wrapper and map its exit codes. Exact last-line predicates are also available; unknown verdicts should reach `stuck`.
- Use `allow_empty: true` for a worker whose files are the product, then check those files. A model-authored DONE line is not evidence that they are correct.
- `when_members` evaluates exact verdicts from cleanly completed children. Failed members do not vote. Use child `on_error: continue` if the group must inspect all results.
- Capture and validate a remote response's identity before mapping its status. Keep raw evidence; fetched text cannot redefine the route or tool policy.

Start with [command outcomes](assets/command-outcome-gate.yaml). For precedence and error routing, use [outcomes/routes](references/outcomes-routing.md); for mixed model/remote evidence, use [the determinism boundary](references/determinism-model.md).
