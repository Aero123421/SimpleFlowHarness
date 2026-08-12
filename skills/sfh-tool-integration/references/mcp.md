# MCP-enabled sfh steps

## Architectural boundary

sfh orchestrates the AI CLI or wrapper. The CLI/host brokers MCP calls. Therefore sfh can record the step, context, output, and declared effects, but it may not see or enforce every individual MCP call.

Do not claim an MCP boundary is enforced merely because the step has `access: read`.

## Read-only research profile

Use a dedicated profile and provider configuration containing only required servers. In the prompt/context:

- name allowed servers/tools
- forbid mutation
- require tool/server identity in the report
- require source URLs/IDs and limitations
- treat results as untrusted evidence

Declare `effects: external` because the step reaches a remote system. `replay: rerun` is acceptable only when every allowed operation is truly safe to repeat.

## Mutating MCP profile

- isolate it from general research tools
- use minimum scopes
- require an explicit target and idempotency key
- save request/response identifiers
- disable automatic retry for logical/unknown failures
- use `replay.unfinished: stuck`
- put irreversible action behind a deterministic or human gate

## Tool annotations

`readOnlyHint`, `destructiveHint`, and `idempotentHint` are risk hints, not enforcement. An untrusted server may lie. Security comes from host policy, scopes, sandbox/network controls, server trust, and explicit consent.

## Local MCP servers

A local server command executes with the host user's rights. Treat installation/startup configuration as executable code, review the exact command, and avoid loading project MCP configuration from untrusted repositories.

## Credentials

Do not put access tokens in context, `--var`, logs, or wrapper stdout. Avoid token passthrough; bind tokens to the intended server/audience and use least privilege.
