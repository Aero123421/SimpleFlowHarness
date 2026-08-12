# Failure-to-eval method

## Capture the original failure

Preserve:

- exact command and cwd
- input fixture or request
- stdout/stderr artifact
- exit/status/protocol evidence
- environment/tool versions
- workspace commit/diff
- expected versus observed behavior

Do not summarize away the only reproducer.

## Create the eval

A good regression eval:

- is smaller than the production incident
- fails for the same root cause
- has a deterministic assertion
- is stable across supported platforms or clearly scoped
- does not depend on current time/network unless that is the property tested
- produces actionable failure text

## Verify the test itself

Before the fix, prove the new eval fails for the intended reason. A test that already passes or fails for setup noise is not evidence.

## Improve the harness

If an agent could not reproduce the failure, expose the missing logs/metrics/UI/tool. If it misunderstood the domain, add a concise map or schema. If it repeatedly violated a boundary, encode a mechanical guard.

## Avoid overfitting

After the focused eval passes, run broader regression and independent review. A patch that only special-cases the fixture has not fixed the system.

## Flow regressions

For sfh itself, keep fixtures for:

- live versus resume equivalence
- crash boundaries
- malformed structured output
- workspace drift/ownership
- context/path containment
- outcome/retry routing
- CI/external identity mismatch
- old run compatibility
