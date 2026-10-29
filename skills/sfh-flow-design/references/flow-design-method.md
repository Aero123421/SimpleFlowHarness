# Choosing the execution boundary

Use this when a task does not yet map cleanly to a flow. The [skeleton](../assets/stable-flow-skeleton.yaml) is a starting point, not a required number of roles or stages.

| Decision | SFH consequence |
|---|---|
| What proves acceptance? | A command gate, explicit evaluator verdict, or both; process completion alone proves neither |
| Which artifact crosses a step boundary? | Give it a durable location and pass its path or bounded contents through context |
| What changes on another pass? | Route revisits use current files/evidence; a retry repeats the same invocation |
| Where do writes happen? | One managed worktree per run, or an explicit current/directory workspace; serialize overlapping writers |
| What can happen outside the workspace? | Declare external effects; reconcile uncertain results before repeating them |
| What ends the work? | Accepted completion, failure, or `stuck` with preserved blockers; bound revisits and total work |

A useful split is generation → artifact checks → evaluation of remaining semantic criteria. If one step can do the task with sufficient evidence, do not manufacture a multi-agent pipeline.

For a feedback loop, keep task/acceptance inputs stable and send the correction step current blockers. The evaluator must inspect the current artifact, not merely the writer's completion claim. Reuse the workspace across visits; a new worktree per lap loses the change series.

For an external tool, bind the invocation and response to the intended target. Store credentials outside YAML and contexts. Capture remote observations before interpretation, and keep fetched text from changing policy.

Choose limits that bound actual work. Retry attempts are inside logical leaf runs; `max_total_steps` is not an attempt ceiling. Set invocation deadlines and reserve sufficient budget for the full handoff. Cost coverage depends on the adapter.

Use `sfh validate FLOW --strict` for schema/static checks, preflight for local capabilities, and plan/dry-run to inspect execution without running leaves. Report remaining uncertainty; these checks do not establish model quality or authenticated provider behavior.
