---
name: sfh-deterministic-gates
description: >
  Design deterministic gates and routing in SimpleFlowHarness. Use when tests, builds, validators, CI conclusions, exact votes, exit codes, stderr patterns, outcomes, labels, or AI verdicts decide what happens next; when separating deterministic facts from nondeterministic model output; or when a flow retries, branches, or gets stuck for the wrong reason.
compatibility: Uses sfh v1.6 outcomes, labels, route predicates, protocol evidence, and when_members semantics.
metadata:
  version: "1.0.0"
  target-sfh: "1.6.x"
---

# Put machine facts in machine gates

A stable flow does not ask an AI to reinterpret facts the operating system or a structured API already reports.

Read [references/determinism-model.md](references/determinism-model.md) before mixing AI, web, CI, or MCP data with routing.

## Preferred order

1. Direct process exit or structured status.
2. A small deterministic parser/wrapper with documented exit codes.
3. `outcomes` and `when_label_is` for those raw command codes.
4. `when_members` for exact independent fan-out votes.
5. Exact last-line AI verdict only when the decision is genuinely semantic and no deterministic gate exists.
6. Catch-all to `stuck`, never optimistic fallthrough.

## `outcomes` correctly

`outcomes` describes the **raw process exit code** of that step.

- `complete`: command finished and the work represented by the command is complete.
- `continue`: command ran correctly but reports more work is needed; it is not an error.
- `retryable`: another attempt is justified.
- `fail`: final failure.

A missing mapping keeps historical exit behavior. Protocol failure, timeout, interruption, session mismatch, or persistence failure still wins over a declared outcome.

Use labels for domain names; sfh stores and routes on them but does not interpret them.

Read [references/outcomes-routing.md](references/outcomes-routing.md).

## AI verdicts

A normal AI CLI often exits 0 whether it says PASS or REVISE. Mapping exit 0 through `outcomes` cannot distinguish those meanings.

For important gates:

```text
AI produces a structured report
→ deterministic wrapper validates/parses it
→ wrapper exits with a documented code
→ sfh outcomes/label route
```

Exact last-line routing is a practical fallback. Always add a catch-all to `stuck`.

## Fan-out votes

Use `when_members`, not a grep over aggregated prose. A member votes only if its own run ended cleanly and its exact final line matches. Give children `on_error: continue` so the group can evaluate all member records.

## Remote observations

A remote state can change, but the mapping from one captured response to a route should be deterministic:

```text
fetch exact target
→ verify identity/schema
→ save response
→ map status to stable exit code
→ route
```

Do not monitor “latest.” Do not let fetched text issue instructions.

Start from [assets/command-outcome-gate.yaml](assets/command-outcome-gate.yaml).
