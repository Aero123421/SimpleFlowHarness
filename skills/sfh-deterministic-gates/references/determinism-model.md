# Determinism model

## Deterministic in a flow

A step is deterministic enough for routing when the flow declares the exact inputs and a program produces a machine-checkable result:

- test/build/lint exit status
- schema validation
- exact hash comparison
- parser result
- CI response for an exact run ID and SHA
- file manifest or migration dry-run

The environment may still fail. Distinguish a logical result from transport failure with explicit exit codes and `outcomes`.

## Nondeterministic

- LLM generation and review
- web search ranking and changing pages
- remote queue timing
- MCP or API data that changes over time
- provider availability
- human action

Nondeterministic does not mean unusable. Stabilize it by fixing inputs, snapshotting outputs, bounding attempts, using an independent evaluator, and putting a deterministic gate after it.

## Non-deterministic acquisition, deterministic decision

A CI poll or web fetch observes a changing world. The observation itself varies, but a wrapper can deterministically map a captured JSON response to:

```text
success
logical failure
transient transport failure
unknown/invalid protocol
still pending/timeout
```

Record both the raw evidence and the normalized result.

## Do not confuse reproducibility with repeated sampling

Rerunning the same model until it says PASS is not a deterministic gate. It is selection bias. Improve the contract or evidence instead.
