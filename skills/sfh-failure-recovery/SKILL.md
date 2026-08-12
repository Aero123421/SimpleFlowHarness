---
name: sfh-failure-recovery
description: >
  Design failure handling and recovery for SimpleFlowHarness flows. Use when choosing retry, retry_on, fallback, on_error, replay.unfinished, resume, force-resume, adopt-workspace, carry-budget-from, budget landing, timeout, stuck, or idempotency behavior; especially for external APIs, deployments, migrations, CI, and long-running AI work.
compatibility: Written for sfh v1.5 recovery, workspace checkpoint, budget, and machine-interface behavior.
metadata:
  version: "1.0.0"
  target-sfh: "1.5.x"
---

# Classify the failure before choosing the mechanism

Read [references/recovery-matrix.md](references/recovery-matrix.md).

## Failure classes

- **transport/infra:** rate limit, temporary network error, provider outage, hung stream
- **protocol:** expected structured terminal record missing or malformed
- **logical rejection:** tests failed, evaluator says revise, acceptance incomplete
- **flow definition:** bad route, wrong binary, invalid ceiling, changed context
- **external effect unknown:** process may have mutated outside state before crash
- **persistence:** sfh could not durably record required artifacts

These require different actions.

## Mechanisms

- `retry`: same invocation, only when another attempt is safe and justified.
- `fallback`: alternate tool/profile after retries; not a substitute for fixing logic.
- route to fixer: logical rejection or incomplete work.
- `replay.unfinished`: crash recovery policy for a started step with no durable end.
- `stuck`: preserve evidence and ask for a decision.
- `--resume`: unchanged flow and closure.
- `--force-resume`: explicitly accepts changed execution definition; it does not adopt workspace drift.
- `--adopt-workspace`: accepts changed workspace; it does not waive closure checks.
- `--carry-budget-from`: fresh corrected flow that inherits spend, not outputs/sessions/position.

## External mutation

Default pattern:

```yaml
effects: external
replay: {unfinished: stuck}
retry_on: never
```

Only add retry when the operation has an enforced idempotency key or a deterministic probe can prove whether it happened. Tool or MCP annotations are hints, not proof.

## Budgets

Long loops need `max_total_steps`, `max_visits`, timeouts, and wall-clock/cost ceilings. If using `on_budget`, reserve enough for the complete handoff/landing chain; a zero reserve lands too late.

## Error messages are evidence

Capture command stderr, remote response, exact target identity, and current workspace state. A recovery flow that only says “try again” teaches nothing.

Start from [assets/external-effect-safe.yaml](assets/external-effect-safe.yaml).
