# Installation and use

## Project-level installation

Copy each skill directory directly under `.agents/skills/`:

```text
<repo>/.agents/skills/
  sfh-flow-design/SKILL.md
  sfh-flow-review/SKILL.md
  ...
```

Project-level skills are appropriate when the sfh design rules should travel with the repository.

## User-level installation

Copy the same directories under `~/.agents/skills/` for reuse across repositories.

## Native client locations

Some clients also scan their own directories such as `.claude/skills/`. Prefer `.agents/skills/` when sharing across clients, then add a native copy or symlink only when your client requires it.

## Trust

A project skill is repository content and can contain instructions or scripts. Do not auto-activate skills from an untrusted checkout. Inspect `SKILL.md` and scripts first.

## sfh runtime boundary

Agent Skills guide the AI that authors the flow. Current sfh v1.5 does not parse a `skills:` flow key.

When a runtime AI step must receive stable operating rules, use:

```yaml
contexts:
  implementation_rules:
    file: ./prompts/implementation.md
```

and list that context on the step. This makes the exact text a durable run input. A provider-native Skill may still be used, but activation and hidden resources are provider behavior and should not be the only source of a correctness-critical rule.
