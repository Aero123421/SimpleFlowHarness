---
name: sfh-flow-review
description: >
  Audit an existing SimpleFlowHarness YAML flow for correctness, stability, convergence, recovery, security boundaries, portability, context/workspace use, external-tool identity, CI monitoring, and AI usability. Use when reviewing an AI-authored flow, diagnosing a brittle loop, checking a flow before release, or producing prioritized fixes without running paid or mutating steps.
compatibility: Review against the installed sfh version; bundled rubric targets v1.6.x.
metadata:
  version: "1.0.0"
  target-sfh: "1.6.x"
---

# Review the flow as a program, not as prose

Do not begin by rewriting prompts. Reconstruct the state machine and evidence contract.

Read [references/review-rubric.md](references/review-rubric.md) and [references/portability.md](references/portability.md).

## Review sequence

1. Load the current sfh guide/schema; identify the target version.
2. Parse all top-level steps, parallel members, routes, terminals, revisits, and implicit fallthrough.
3. Classify each step: deterministic, nondeterministic, workspace write, external effect, unknown.
4. Trace happy path, every failure path, max-visit path, budget landing, and resume path.
5. Verify workspace/context/profile/session contracts.
6. Check retry/fallback/replay semantics.
7. Check external IDs, credentials, timeouts, rate limits, and artifact validation.
8. Check loops for progress, independence, and hard bounds.
9. Run heuristic lint, `sfh validate --strict`, preflight, and plan when available.
10. Return prioritized findings and a minimal corrected YAML/diff.

## Finding format

```text
[severity] stable-id — title
Evidence: exact step/key/path
Why it fails: concrete live/resume/OS/AI scenario
Required fix: smallest contract-preserving change
Test: how to prove the fix and the pre-fix failure
```

Severities:

- blocker: data loss, unintended command/effect, false success, non-resumable corruption, wrong remote target
- high: likely loop failure, duplicate spend, wrong route, missing bound, hidden external mutation
- medium: fragile verdict, missing evidence, context bloat, misleading machine contract
- low: clarity, example, maintainability

## Review principles

- Distinguish legal YAML from good flow design.
- Do not propose nonexistent sfh fields.
- Do not weaken a deterministic gate to make the run pass.
- Do not call external/premium tools during review unless explicitly requested.
- Treat `access` as provider policy, not proof of OS/MCP isolation.
- Check parallel children recursively; top-level-only analysis is insufficient.
- Verify all examples work from a clean checkout and across intended OSes.

Use [assets/review-output-template.md](assets/review-output-template.md) for the final report.
