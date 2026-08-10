# Research sources

Accessed: 2026-08-09

## Ponytail

- Dietrich Gebert, Ponytail repository  
  https://github.com/DietrichGebert/ponytail
- Ponytail core Skill  
  https://github.com/DietrichGebert/ponytail/blob/main/skills/ponytail/SKILL.md
- Ponytail review Skill  
  https://github.com/DietrichGebert/ponytail/blob/main/skills/ponytail-review/SKILL.md
- Ponytail audit Skill  
  https://github.com/DietrichGebert/ponytail/blob/main/skills/ponytail-audit/SKILL.md

Applied ideas: understand before minimizing; YAGNI; reuse/stdlib/native/dependency ladder;
root-cause fixes; deletion over abstraction; minimal runnable checks; security and accessibility boundaries;
complexity review separated from correctness/security review.

## Recent loop and harness engineering

- OpenAI, “Harness engineering: leveraging Codex in an agent-first world”  
  https://openai.com/index/harness-engineering/
- Anthropic, “Harness design for long-running application development”  
  https://www.anthropic.com/engineering/harness-design-long-running-apps
- Anthropic, “Demystifying evals for AI agents”  
  https://www.anthropic.com/engineering/demystifying-evals-for-ai-agents
- OpenAI, “Building self-improving tax agents with Codex”  
  https://openai.com/index/building-self-improving-tax-agents-with-codex/
- Anthropic, “How we contain Claude across products”  
  https://www.anthropic.com/engineering/how-we-contain-claude
- OpenAI, “From model to agent: Equipping the Responses API with a computer environment”  
  https://openai.com/index/equip-responses-api-computer-environment/

Applied ideas: depth-first task decomposition; repository knowledge as durable context; one worktree per change;
application and evidence legibility; planner/generator/evaluator separation; structured handoffs across sessions;
review/fix loops with explicit stop conditions; regression evals derived from real failures; continuous technical-debt
garbage collection; containment by limiting environment and tools; Skills as reusable working procedures.
