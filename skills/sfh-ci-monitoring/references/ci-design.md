# CI flow design

## Trigger and discovery

Best: a caller already supplies the exact CI run ID and expected SHA.

If the flow triggers CI itself, the trigger step must produce a durable correlation value. Discover by workflow + SHA + time/correlation. Require exactly one match. Some CI APIs are eventually consistent, so discovery may need bounded polling distinct from watching the run.

## `gh` patterns

Explicit watch:

```text
gh run watch RUN_ID --exit-status
```

Structured polling is preferable when the flow needs to distinguish failure classes:

```text
gh api repos/OWNER/REPO/actions/runs/RUN_ID
```

Verify `head_sha` before accepting any status.

## Concurrency

GitHub Actions may cancel an older run in a concurrency group. A cancelled run can mean “superseded by a newer commit,” not “the code failed.” Compare the observed SHA with the intended SHA and record the cancellation reason/attempt.

## Schedule and queue uncertainty

Scheduled workflows may be delayed under load. Queue time is not a completion percentage. Use a deadline and a handoff; do not encode “it should start within N seconds” as product failure unless that is the actual service-level contract.

## Artifact handling

Save:

- run status JSON
- failed job/step names
- failed logs or their bounded artifact paths
- run URL/ID/SHA/attempt
- timestamp

Then let a diagnosis step read the saved evidence. Do not repeatedly fetch mutable logs into an ever-growing prompt.

## Rerun policy

Allowed only under a declared classification:

- known infrastructure outage
- known flake with bounded rerun allowance
- cancelled before execution

A deterministic test failure should route to repair, not rerun.
