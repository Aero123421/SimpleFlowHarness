# sfh Flow Design Agent Skills

Agent Skills for designing, reviewing, and improving SimpleFlowHarness YAML workflows.

These skills teach an authoring agent how to use sfh v1.5 primitives. They do not add a `skills:` key to the sfh flow format. Runtime instructions for an AI step belong in existing named `contexts:` or in the selected CLI's native skill system.

Install at project scope:

```bash
mkdir -p .agents/skills
cp -R skills/sfh-* .agents/skills/
```

Primary entry points:

- `sfh-flow-design` for creating or rewriting a flow.
- `sfh-flow-review` for auditing an existing flow.
- Add the specialist skill matching the problem: loops, deterministic gates, context/workspace, failure recovery, CI, external tools/MCP, or eval-driven improvement.

Validate the pack with `python3 skills/tools/validate_skills.py skills`. Validate generated flows with the installed sfh binary: `sfh validate --strict`, `sfh preflight --json`, and `sfh plan --json --save`.
