# Outcomes and routing

## Example exit vocabulary

A project-owned wrapper may define:

```text
0  accepted
2  valid execution, acceptance incomplete
10 transient API/provider error
20 deterministic rejection
30 malformed or unknown response
```

Then:

```yaml
outcomes:
  0:  {result: complete, label: accepted}
  2:  {result: continue, label: incomplete}
  10: {result: retryable, label: transport_error}
  20: {result: continue, label: rejected}
  30: {result: fail, label: invalid_response}
```

`continue` is useful for a valid negative result that should route to improvement rather than `on_error`.

## `on_error` versus route

- route handles a successful/continued step result.
- `on_error` handles a failed step after retry/fallback.
- use `on_error: goto:diagnose` for genuine command failure.
- do not mark a valid rejection as failure merely to reach the fixer; map it to `continue` and label it.

## Protocol evidence

For preset AI tools, sfh verifies the documented structured protocol. `outcomes` cannot turn an incomplete protocol into success. `exit_conflict: trust_protocol` should be used only for a known CLI whose terminal protocol certifies success despite a nonzero process exit.

## Exact AI trailer

When no wrapper is practical:

```yaml
route:
  - {when_last_line_is: "REVIEW: PASS", goto: end}
  - {when_last_line_is: "REVIEW: REVISE", goto: fix}
  - {goto: stuck}
```

The catch-all is part of the contract. Without it, formatting drift may silently fall through.
