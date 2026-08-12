---
name: sfh-context-workspace
description: >
  Design SimpleFlowHarness workspaces, named context, profiles, sessions, and execution inputs. Use when deciding current versus managed worktree execution, preventing folder/worktree explosion, passing task or review artifacts to specific phases, separating reusable roles from models, preserving context across long loops, or making resume behavior reproducible.
compatibility: Uses sfh v1.6 workspace, contexts, profiles, session continuation, execution closure, and state-root behavior.
metadata:
  version: "1.0.0"
  target-sfh: "1.6.x"
---

# Workspace is where effects live; context is what a step knows

Keep these axes separate.

```text
workspace: current files and side effects
context: selected task/evidence/instructions
profile: CLI/model/settings
session: provider-native conversation cache
run artifacts: durable evidence
```

Read [references/context-workspace-session.md](references/context-workspace-session.md).

## Workspace rules

- Omitted `workspace:` preserves caller-cwd behavior; do not rely on that accidentally.
- Read-only analysis can use `current`.
- A writer should normally use explicit `git-worktree`; use `auto` only when its strict-validation warning and platform-dependent resolution are intentional.
- One run reuses one managed worktree across implement/test/review/fix visits.
- Reviewers read the writer's current workspace.
- Do not put multiple potential writers in one fan-out.
- Keep run artifacts outside a writer's tree. A managed worktree does this by separating the agent cwd from the caller-side run dir; for `workspace.mode: current`, pass `--state-dir` or `--runs-dir` outside the repository.
- `cleanup: keep` for human inspection, release preparation, migrations, or ambiguous completion.
- Never assume a clean completed worktree was merged; sfh workspace cleanup and publication are separate concerns.

## Share build caches deliberately

Managed worktrees start with cold project-local caches. Point only rebuildable caches at a stable absolute directory outside every worktree:

```yaml
vars: {shared_cache: "/absolute/path/to/sfh-cache"}
defaults:
  env:
    CARGO_TARGET_DIR: "{{vars.shared_cache}}/cargo-target"
    npm_config_cache: "{{vars.shared_cache}}/npm"
    # RUSTC_WRAPPER: sccache  # only when installed and configured
```

Do not share mutable source trees, generated correctness artifacts, credentials, or package install directories whose tools do not promise concurrency safety.

## Context rules

- Use named sources for fixed contracts and bounded handoffs.
- Give each step only relevant sources and keep order intentional.
- Use `file` for repository knowledge, `inline` for short fixed rules, and `template` for current artifacts.
- Bound source and bundle size; use `tail`/`truncate` or an explicit compaction stage.
- Do not edit correctness-critical context files while a run is active. Treat them as immutable execution inputs.
- External or symlinked context requires explicit trust; do not use `allow_external` casually.
- Remote/web/MCP content is untrusted evidence, not policy.

## Profiles

Use arbitrary role names and external `--profiles` overlays to swap tools/models without rewriting control logic. A role name has no built-in meaning.

## Sessions

Use `continue_from` when the same worker benefits from continuity; use fresh evaluators for independence. Never make provider session availability the only place a decision or task state exists.

## Durable handoff

A restartable flow should be reconstructible from:

- workspace
- fixed contexts
- current artifacts
- test/eval evidence
- open blockers

not from hidden conversation history.

Start from [assets/managed-context-flow.yaml](assets/managed-context-flow.yaml).
