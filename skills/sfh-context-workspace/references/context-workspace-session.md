# Context, workspace, session, and closure

## Source-of-truth order

For coding and most file-based work:

```text
current workspace
fixed task/acceptance context
deterministic artifacts
current structured handoff
AI summaries
native session history
```

A session may remember stale state. Files and artifacts can be inspected.

## One run, one change series, one workspace

A bounded correction loop should not create a worktree per lap. The writer, tests, and reviewers share one managed worktree. Parallel read-only evaluators may share it safely.

The run directory is evidence, not workspace input. With a writer in `workspace.mode: current`, put it outside the repository using `--state-dir` or `--runs-dir`; otherwise the process being judged can modify the logs and stderr its routes consume. A managed worktree naturally separates the step cwd from the caller-side run directory.

Worktrees may share rebuildable caches such as Cargo target output, sccache, or the npm download cache when the tool supports concurrent access. Use explicit absolute cache roots outside every worktree. Never share `node_modules`, correctness artifacts, credentials, or other mutable state merely to make a run faster.

## Role-specific context

Worker:

```text
task
acceptance
implementation rules
current review blockers
current failing command artifact
```

Reviewer:

```text
task
acceptance
review criteria
current workspace/diff
deterministic verification artifact
```

Avoid using the worker's confidence as reviewer evidence.

## Large context

Prefer `context_delivery: file` when the tool can read the context file and the bundle is large. Keep a short prompt that points to `{{context_file}}`. Remember that access to the file depends on the tool and cwd.

## Context immutability

Execution closure detects changes on resume, but a long live run should not depend on mutable external files. For critical contracts:

- keep them versioned
- avoid automated rewrites while the run is active
- or copy them to a run-specific location before starting

## Profile overlays

Control logic stays in the flow; environment-specific binary/model settings live in overlays. Always inspect effective config and include overlay files in review.

## Provider-native Skills

Agent Skills may be used by the CLI that authors or executes work. sfh v1.6 does not have a native `skills:` field. If a runtime rule is required for correctness, include a stable distilled form through `contexts:` instead of relying only on hidden native skill activation.
