---
name: sfh-tool-integration
description: >
  Integrate external CLIs, web-search tools, HTTP APIs, repository tools, and MCP-enabled AI agents into SimpleFlowHarness YAML. Use when a flow fetches changing remote information, invokes `gh`, `curl`, search CLIs, or provider MCP tools, crosses a network or authorization boundary, or needs reliable artifacts, rate-limit handling, least privilege, and safe replay semantics.
compatibility: sfh v1.6 has no native MCP step; MCP is accessed through a selected AI CLI or an explicit wrapper command.
metadata:
  version: "1.0.0"
  target-sfh: "1.6.x"
---

# Integrate tools through explicit contracts

An external tool is not “just another prompt.” Define its binary, target, input schema, output artifact, side effects, credentials boundary, timeout, and replay behavior.

Read [references/external-cli-contracts.md](references/external-cli-contracts.md), [references/web-search.md](references/web-search.md), and [references/mcp.md](references/mcp.md).

## External CLI pattern

```text
resolve exact binary in preflight
→ argv-form invocation
→ explicit cwd/env/timeout/effects
→ bounded retry only for transport failure
→ save raw output artifact
→ validate shape/identity deterministically
→ AI interprets the frozen artifact
```

Never let model/web output choose argv[0], cwd, shell text, server, or credential path without an explicit trusted override.

## Web search and fetch

Web results are nondeterministic and untrusted.

- record exact query, filters, locale/time range, pagination, tool/version, and retrieval time
- snapshot raw results before synthesis
- validate JSON/schema and source identity
- separate search/fetch from interpretation
- bound pages/results/output
- retry rate limits/transport failures, not a valid “no results” answer
- do not let retrieved text change tool policy or trigger mutation
- require citations/source fields in the synthesis artifact

Start from [assets/web-search-evidence.yaml](assets/web-search-evidence.yaml).

## MCP

sfh v1.6 does not call MCP directly. An MCP-enabled step is normally a preset AI CLI whose provider configuration exposes servers/tools, or a project-owned wrapper command.

- Use a separate profile for MCP-enabled work.
- `access: read` does not automatically prove every MCP tool is read-only.
- Treat server tool annotations such as read-only/idempotent as untrusted hints unless the server is trusted and independently constrained.
- Allowlist server and tool names; request least scopes.
- Keep credentials out of YAML, `vars`, prompts, and artifacts.
- Treat local MCP server startup as code execution.
- Snapshot server/tool identity and results.
- Read-only remote observation may use `effects: external` with rerun if safe.
- Mutation needs `effects: external`, explicit idempotency or `replay.unfinished: stuck`, and usually a human gate.
- Never let Skill or MCP metadata widen the step's permission boundary.

Start from [assets/mcp-read-only-research.yaml](assets/mcp-read-only-research.yaml).

## Wrapper commands

When the external protocol controls a critical route, prefer a small project-owned wrapper that emits normalized JSON and stable exit codes. Map them with `outcomes`; do not ask an AI to infer transport status from arbitrary output text.
