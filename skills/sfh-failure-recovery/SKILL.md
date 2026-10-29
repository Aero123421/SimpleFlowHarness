---
name: sfh-failure-recovery
description: >-
  Handle failed or interrupted SimpleFlowHarness runs. Use when choosing retry, fallback, replay, resume, workspace adoption or carried budgets, especially with uncertain side effects.
compatibility: SFH v2.0; use the installed guide and schema for exact syntax.
metadata:
  version: "1.1.0"
  target-sfh: "2.0.x"
---

# Recover without losing evidence

Inspect `sfh runs why RUN`, the attempt artifacts and actual workspace/remote state before repeating work. A process failure does not prove its effects did not happen.

| Need | SFH mechanism |
|---|---|
| Safe repeat of one invocation | Bounded `retry` with a justified `retry_on` |
| Equivalent work through another profile | `fallback`, after retries |
| Tests/evaluator reject the artifact | Route to correction, not blind retry |
| Started work is unfinished on resume | `replay.unfinished: rerun`, `stuck` or `fail` |
| Continue the same execution definition | `--resume` |
| Accept reviewed definition / workspace drift | `--force-resume` / `--adopt-workspace` respectively; neither waives the other's checks |
| Start a corrected flow with previous spend | `--carry-budget-from`; does not import outputs, sessions or position |

For external mutation, start with `effects: external`, `replay: {unfinished: stuck}`, `retry_on: never` as in [the external-effect example](assets/external-effect-safe.yaml). Repetition needs proven idempotency or a probe of the actual result. A declared replay policy is not an exactly-once guarantee; inspect uncertain effects before resume, including after a cooperative stop.

- An empty final message differs from invalid completion evidence. `allow_empty: true` suits artifact-producing workers with artifact gates; it never bypasses protocol validation.
- Completion is adapter-specific: OpenCode 2 binds completed structured text to exit (`exit_bound`); Pi requires final assistant outcome and `agent_settled`. Do not demand one universal terminal record.
- To salvage malformed/incomplete output, use `on_error: continue` and explicit `when_protocol_is` routes to a separate interpreter. Raw `{{steps.<id>.outputs}}` is evidence, not a successful answer; do not send every failure through salvage.
- `timeout_sec` covers the entire invocation, including tool use; `hang_after_sec` classifies failed attempts using silence duration; it is not an idle watchdog. Retry backoff can reach the wall-clock reserve before the next attempt; it does not pre-empt a running attempt. Size the reserve for the full landing chain.
- Preserve partial artifacts and checkpoints. Persistence failure is not completion. `runs clean` only deletes proven `done` runs; other states remain, even if not resumable. Managed-workspace cleanup is separate.

Use [the recovery matrix](references/recovery-matrix.md) for classification and lineage details. Verify recovery with disposable fixtures before relying on it for an external effect.
