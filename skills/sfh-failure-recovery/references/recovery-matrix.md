# Recovery decision matrix

| Situation | Mechanism | Avoid |
|---|---|---|
| provider rate limit / temporary API error | bounded retry, transient classification | `retry_on: any` |
| model returns valid REVISE | route to fixer | retrying the same review as infra |
| deterministic tests fail | route to diagnosis/fix | asking AI whether exit code means pass |
| structured protocol malformed | fail closed, doctor/preflight/update adapter | accepting raw text |
| alternate provider may continue equivalent work | fallback | fallback for semantic disagreement |
| crash in pure read/compute step | replay rerun | manual duplication |
| crash in deploy/send/migration | replay stuck or deterministic probe | blind rerun |
| flow file/context/tool version changed | new run or explicit force after review | silent resume |
| workspace changed externally | inspect, then adopt or refuse | conflating with force-resume |
| corrected flow after spend | carry-budget-from | resetting counters by hand |
| loop ceiling reached | handoff + stuck | raising limit without diagnosis |
| required artifact persistence failed | non-resumable failure | recording completion anyway |

## Retry policy

A retry should have:

- bounded count
- exponential or increasing backoff
- step and wall-clock deadline
- explicit reason
- no new logical inputs
- safe repeated effects

## Fallback policy

Fallback must preserve the step contract, access level, workspace, context, and expected protocol. If a fallback changes the meaning of the work, it is a new route or a new flow, not a fallback.

## Corrected-flow lineage

When the flow itself was wrong:

1. preserve old run evidence
2. fix and validate the flow
3. start fresh with `--carry-budget-from`
4. do not reuse old outputs, routing position, session, or workspace as if definitions were unchanged
