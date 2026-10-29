---
name: sfh-tool-integration
description: >-
  Connect external CLIs, web/API tools or MCP-enabled agents to SimpleFlowHarness. Use when defining their invocation, output, identity or permission contract.
compatibility: SFH v2.0; use the installed guide and schema for exact syntax.
metadata:
  version: "1.1.0"
  target-sfh: "2.0.x"
---

# Make the integration contract explicit

Use argv-form `cmd:` or a supported CLI profile, with explicit effects and timeout. Validate tool output and target identity before routing; snapshot remote evidence before model interpretation. Do not let untrusted output select executable, shell code, cwd or credentials.

- Run `sfh preflight FLOW --json` in the actual launcher environment. Schedulers may lack your shell's PATH; set step `env` or profile `bin` deliberately. Check `verified_version` together with `verification_scope`; help-only evidence is not authenticated success.
- Use a small wrapper with stable exit codes/JSON when the external protocol decides a critical route. Map codes with `outcomes`. Retry only safe, bounded failures, not a valid no-results response or semantic rejection.
- Web/search results need source/query/retrieval identity and bounded snapshots. [Web evidence](assets/web-search-evidence.yaml) and [web-search details](references/web-search.md) provide the pattern.
- SFH has no native MCP step. Use a deliberately scoped provider profile or explicit wrapper. `access: read` and server annotations do not prove all MCP tools are read-only; local server startup executes code. Keep credentials out of YAML and artifacts.
- Pi read/write presets disable MCP and extensions; MCP work needs a separately scoped integration. OpenCode 2 uses fresh standalone sessions and its v2 tool permissions. Do not widen access just to make an integration start.
- For uncertain external mutations, inspect effects before repetition; use idempotency/probing and explicit replay policy. Skill or MCP metadata cannot grant authorization.

Use [MCP research](assets/mcp-read-only-research.yaml) with [MCP boundaries](references/mcp.md), or [external CLI contracts](references/external-cli-contracts.md) for wrapper details.
