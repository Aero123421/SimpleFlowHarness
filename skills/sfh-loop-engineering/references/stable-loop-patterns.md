# Stable loop patterns

## Pattern A: writer → gate → reviewer → fixer

Best for one coherent code change.

- writer and fixer use one managed workspace
- command gate runs after every write
- reviewer is read-only and independent
- fixer receives only current blockers and evidence
- bounded visits plus `stuck`

## Pattern B: planner → sequential chunks → evaluator

Best for multi-hour work.

- planner creates dependency-ordered chunks
- foreach/body writer runs with `max_parallel: 1`
- each chunk leaves a machine-checkable artifact
- evaluator sees the final workspace and chunk ledger
- plan changes are explicit, not silently invented mid-loop

## Pattern C: generator → specialist council

Best when quality has orthogonal dimensions.

- correctness reviewer
- security reviewer
- simplicity/maintainability reviewer
- `when_members` or explicit deterministic aggregation
- one failed member cannot be counted as agreement

## Pattern D: failure → eval → fix → regression

Best for production incidents and flaky behavior.

- preserve raw evidence
- first create a reproduction/evaluation that fails for the right reason
- fix only after the eval exists
- rerun broader regression suite
- add the case permanently so the harness improves

## Pattern E: remote observation → snapshot → interpret

Best for web, CI, metrics, or MCP reads.

- poll/fetch exact target
- validate response shape and identity
- save immutable artifact
- AI interprets the frozen artifact
- remote mutation is a separate, explicitly guarded step
