# Sources and design grounding

This pack is an independent set of Agent Skills for designing SimpleFlowHarness YAML. It does not copy Ponytail and does not add a new `skills:` key to the sfh flow format.

## SimpleFlowHarness

- Repository: https://github.com/Aero123421/SimpleFlowHarness
- Target release: https://github.com/Aero123421/SimpleFlowHarness/tree/v1.6.0
- Flow schema: https://github.com/Aero123421/SimpleFlowHarness/blob/v1.6.0/schema/flow.schema.json
- Built-in guide: `sfh guide`

The skills are written against sfh v1.6 concepts: routes, bounded visits, retry/fallback, replay, outcomes and labels, parallel/foreach, managed workspace, named context, execution closure, budgets, detach/status/wait/stop, preflight, doctor, plan, and machine JSON.

## Agent Skills

- Specification: https://agentskills.io/specification
- Client integration and progressive disclosure: https://agentskills.io/client-implementation/adding-skills-support
- Description optimization: https://agentskills.io/skill-creation/optimizing-descriptions

The pack follows the standard `SKILL.md` plus optional `references/`, `scripts/`, and `assets/` layout. Main skill files stay compact; detailed material is loaded on demand.

## Harness and loop engineering

- OpenAI, Harness engineering: https://openai.com/index/harness-engineering/
- Anthropic, Harness design for long-running application development: https://www.anthropic.com/engineering/harness-design-long-running-apps

The recurring principles used here are: make the environment legible, split generation from evaluation, chunk long work, preserve structured handoffs, turn failures into reusable evaluations, and encode recurring feedback in tools and repository knowledge rather than repeatedly enlarging prompts.

## GitHub Actions / CI monitoring

- `gh run watch`: https://cli.github.com/manual/gh_run_watch
- Workflow runs API: https://docs.github.com/en/rest/actions/workflow-runs
- Workflow concurrency: https://docs.github.com/en/actions/concepts/workflows-and-actions/concurrency
- Troubleshooting workflows: https://docs.github.com/en/actions/how-tos/troubleshoot-workflows

## Model Context Protocol

- Security best practices: https://modelcontextprotocol.io/docs/tutorials/security/security_best_practices
- Client best practices: https://modelcontextprotocol.io/docs/develop/clients/client-best-practices
- Tool specification: https://modelcontextprotocol.io/specification/2025-11-25/server/tools

MCP tool annotations are treated as untrusted hints, not permission enforcement. Local MCP servers are executable code. Mutating MCP calls are external effects and should not be blindly retried.
