# Skill activation evals

`evals/trigger-cases.json` contains positive and negative prompts. The descriptions in `SKILL.md` are the catalog-level trigger surface.

Because model activation is nondeterministic, run each case at least three times and record activation rate. A positive case passes when all relevant specialist skills are loaded before YAML is authored. A negative case passes when the sfh-specific skills stay inactive.

Also evaluate output quality separately:

- no invented sfh keys
- correct deterministic/nondeterministic separation
- explicit loop bounds
- safe replay for external effects
- exact CI/MCP/web identity and evidence
- successful `sfh validate --strict`, preflight, and plan where available
