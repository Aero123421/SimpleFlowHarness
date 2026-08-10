# Convergence and non-progress

## Signs the loop is not improving

- reviewer repeats the same blocker after a correction
- workspace diff grows while acceptance evidence stays unchanged
- writer rewrites unrelated areas each lap
- prompts grow by appending all prior conversation
- tests are rerun without a change to inputs or environment
- fallback models disagree but no deterministic gate settles the fact
- the same output hash or verdict appears repeatedly

## Stabilizers

### Fixed contract

Keep task and acceptance context unchanged. A changing contract creates a moving target, not an improvement loop.

### Current-state context

Pass current files, current test artifacts, and current open blockers. Do not replay every obsolete review.

### Small correction

The fix step should close the smallest current blocker. Large rewrites reset the evaluator's frame and increase oscillation.

### Independent evaluation

A writer should not certify its own work. Use a fresh read-only evaluator or a deterministic command.

### Hard ceiling

Use explicit `max_visits`, `max_total_steps`, wall-clock budget, and `handoff`/`stuck`. A loop without a ceiling is an outage waiting politely.

### Non-progress handoff

sfh v1.5 does not expose a generic route predicate for repeated output hashes. Use a bounded loop and have the handoff step report repeated blockers. Where non-progress can be mechanically detected, write a command gate that returns a stable exit code and map it through `outcomes`.

## Do not “fix” convergence with randomness

Changing models, temperature-like behavior, or retries can be useful for provider failure, but it is not a substitute for a clearer contract, better evidence, or a deterministic evaluator.
