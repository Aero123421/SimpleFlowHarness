# Sources and design grounding

This pack is an independent set of Agent Skills for designing SimpleFlowHarness YAML. It does not copy Ponytail and does not add a new `skills:` key to the sfh flow format.

## SimpleFlowHarness

- Repository: https://github.com/Aero123421/SimpleFlowHarness
- Target release: https://github.com/Aero123421/SimpleFlowHarness/tree/v2.0.0
- Flow schema: https://github.com/Aero123421/SimpleFlowHarness/blob/v2.0.0/schema/flow.schema.json
- Built-in guide: `sfh guide`

The skills are written against sfh v2.0 concepts: routes, bounded visits, retry/fallback, replay, outcomes and labels, parallel/foreach, managed workspace, named context, execution closure, budgets, detach/status/wait/stop, preflight, doctor, plan, and machine JSON.

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

- v2 contract re-review: [CLI evidence and migration](../docs/harness-refresh-2026-10.md). All nine skills retain their authoring scope; MCP, session, protocol and cost guidance is updated for the new adapters.

## Skill simplification review — retrieved 2026-10-29

- OpenAI [current skill creation guidance](https://learn.chatgpt.com/docs/build-skills) (unversioned living page; redirected from `/codex/skills/`).
- OpenAI [Rethinking skills and prompts for GPT-6 Astra](https://developers.openai.com/blog/rethinking-skills-and-prompts-for-gpt-6-astra), published 2026-09-11: narrow descriptions and task-dependent reference use informed this revision.
- OpenAI [prompt engineering](https://developers.openai.com/api/docs/guides/prompt-engineering) (living page): evaluate changes for the chosen model/task.
- OpenAI [public skill-creator snapshot](https://github.com/openai/skills/blob/4ab6e0fd99c6667163bc34173e3ed3a3fed75ebc/skills/.system/skill-creator/SKILL.md) and Anthropic [public skill-creator snapshot](https://github.com/anthropics/skills/blob/b0cbd3df1533b396d281a6886d5132f623393a9c/skills/skill-creator/SKILL.md). These are public repository revisions, not proof of the version bundled in every client.
- Anthropic [skill authoring](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices) and [prompting](https://platform.claude.com/docs/en/build-with-claude/prompt-engineering/claude-prompting-best-practices) (living pages): retain task-specific constraints, use clear instructions and test actual outcomes.

The pack keeps its nine public names and existing assets. Shorter entrypoints and optional references reduce required reading; this is not a measured model-performance improvement. No new runtime feature or model-specific syntax is implied.
