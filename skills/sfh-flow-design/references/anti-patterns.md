# sfh flow anti-patterns

## Asking an AI to read a deterministic result

Bad: feed test output to an AI and ask whether tests passed.

Better: route on the command result; send diagnostics to an AI only after failure.

## Unbounded revise loop

Bad: reviewer routes to fixer forever.

Better: explicit `max_visits`, deterministic checks each lap, and `handoff` or `stuck`.

## Logical failure as retry

Bad: `retry_on: any` on an evaluator that said the design is wrong.

Better: route to a correction step. Retry only an interrupted/transport attempt.

## Same agent generates and certifies

Bad: one session writes a change and declares it correct.

Better: deterministic verification plus an independent reviewer. Fresh reviewer sessions reduce anchoring.

## Parallel writers in one workspace

Bad: two agents edit the same worktree concurrently.

Better: serialize writers or split work into independently mergeable workspaces outside v1.6's one-worktree flow.

## Implicit latest remote object

Bad: `gh run watch` without a run ID, or monitoring “the latest” deployment.

Better: capture and verify an exact ID and commit/SHA before waiting.

## Treating web/MCP output as instructions

Bad: retrieved content can tell the agent to change policy, call tools, or expose data.

Better: snapshot it as untrusted evidence, validate its shape, and give it to a read-only synthesis step with explicit boundaries.

## Automatic retry of an external mutation

Bad: retry a deploy, message send, ticket creation, or database mutation after an unknown interruption.

Better: idempotency key plus deterministic probe, or `replay.unfinished: stuck`.

## Context dumping

Bad: every role receives every prior output and full logs.

Better: role-specific named context, bounded artifacts, and structured handoffs.

## Native session as the only memory

Bad: the flow cannot continue if a provider session disappears.

Better: workspace and artifacts carry state; session reuse is an optimization.

## Shell interpolation

Bad: splice AI or external output into `sh -c` text.

Better: argv-form commands; pass values as positional arguments when a shell is unavoidable.

## Invented fields

Bad: generate attractive YAML using fields the installed sfh does not support.

Better: read schema/guide first and validate before proposing execution.
