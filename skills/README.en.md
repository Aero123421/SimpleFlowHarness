# SFH authoring skills

Skills for agents that **write or review SFH v2.0 flows**. They are not runtime skills: YAML has no `skills:` field. Runtime instructions use `contexts:` or the selected CLI's native configuration; SFH does not prove native skills were loaded.

Start with `sfh-flow-design` for a new graph or `sfh-flow-review` for an existing one. Load a specialist only for the problem at hand:

| Skill | Question it answers |
|---|---|
| `sfh-loop-engineering` | How does a feedback loop progress and stop? |
| `sfh-deterministic-gates` | Which evidence selects a route? |
| `sfh-context-workspace` | Which files, inputs and sessions do steps share? |
| `sfh-failure-recovery` | What can safely repeat or resume? |
| `sfh-ci-monitoring` | Which exact CI run is being observed? |
| `sfh-tool-integration` | What is the external CLI/API/MCP contract? |
| `sfh-eval-engineering` | How does a recorded failure become a regression check? |

Use the linked references and assets as needed; loading the entire pack is unnecessary. SFH can implement worker/check/evaluator loops through declared routes. Quality judgments belong to those checks/evaluators, not to the engine.

Install at project scope from the distribution root:

```bash
mkdir -p .agents/skills
cp -R skills/sfh-* .agents/skills/
```

See [INSTALL.md](INSTALL.md) for client-specific placement. Validate authoring output with the installed guide/schema, `sfh validate FLOW --strict` and `sfh run FLOW --dry-run`. Before live work, preflight and plan expose local capability and execution choices. These checks do not establish provider authentication or safe repetition of an uncertain effect.

Pack checks: `python3 tests/skills_checks.py` and `python3 skills/tools/validate_pack.py skills`. [EVALS.md](EVALS.md) separates skill selection from output quality; [SOURCES.md](SOURCES.md) records authoring guidance.
