# sfh flow review rubric

## Syntax and version

- api version supported
- only documented keys
- schema URL/tag current
- step/profile IDs valid and unique ignoring case
- route targets exist
- examples and overlays present

## State machine

- first step and implicit fallthrough intentional
- every semantic decision has a catch-all
- terminal choice correct: end/fail/stuck
- backward routes bounded
- `on_error` and `on_max_visits` do what comments claim
- parallel child restrictions respected

## Determinism

- command facts not delegated to AI
- AI output not used for irreversible action without gate
- outcomes map raw codes correctly
- labels cannot be unreachable
- fan-out votes use member records
- remote target identity pinned

## Loop

- fixed contract
- progress evidence refreshed
- independent evaluator
- deterministic checks per lap
- no logical failure retried as transport
- visit/step/time/cost ceilings
- durable handoff

## Workspace and context

- writers isolated appropriately
- run artifacts outside any writer-visible current workspace
- no concurrent overlapping writers
- cwd/root explicit enough
- context role-specific and bounded
- mutable critical inputs avoided
- session is not sole memory
- profile overlays preserve required access/behavior

## Recovery

- retry count/backoff/deadline
- fallback contract equivalent
- external/unknown replay policy
- resume versus corrected-flow carry understood
- persistence failure cannot certify completion
- workspace drift and closure changes handled separately

## External tools / CI / MCP

- binary and target explicit
- argv not shell interpolation
- exact CI run ID/SHA
- output schema validated
- credentials not in flow data
- MCP server/tool allowlist and scope
- annotations treated as hints
- mutating calls idempotent or stuck on uncertainty

## Observability and evidence

- actionable stderr/output artifacts
- context/workspace identity visible
- status is not a fake percentage
- partial output bounded
- long logs not copied into every prompt
- next action is safe and explicit

## Portability

- direct argv for cross-platform commands
- shell dependencies documented
- path/case/Unicode assumptions
- OS-specific commands separated or parameterized
- clean-checkout test
