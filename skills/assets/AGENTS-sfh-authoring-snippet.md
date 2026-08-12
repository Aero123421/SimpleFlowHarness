# sfh authoring instructions

When creating or editing a SimpleFlowHarness YAML flow:

1. Load `sfh-flow-design` before writing YAML.
2. Load `sfh-loop-engineering` for any route that can revisit a step.
3. Load `sfh-deterministic-gates` when tests, validators, verdicts, votes, or exit codes control routing.
4. Load `sfh-ci-monitoring` for GitHub Actions or another remote CI system.
5. Load `sfh-tool-integration` for web-search CLIs, APIs, or MCP-enabled agents.
6. Load `sfh-failure-recovery` when retry, fallback, resume, replay, or external effects appear.
7. Load `sfh-flow-review` after drafting.

Never invent sfh fields. Before proposing execution, run the local linter, `sfh validate --strict`, `sfh preflight --json`, and `sfh plan --json --save` when those commands are available.
