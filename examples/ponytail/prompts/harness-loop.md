# Long-running harness contract

Use the workspace and durable artifacts as the source of truth. Native chat history is only a cache.

For long work:

- make one bounded, coherent change at a time;
- persist a concise handoff describing completed facts, remaining blockers, and the next deterministic check;
- use commands for mechanically observable facts rather than asking a model to guess;
- stop retrying when the same failure repeats without new evidence;
- preserve failed or ambiguous work for inspection;
- keep context small through explicit summaries and files, never by silently dropping requirements;
- treat a missing capability, tool, fixture, or observability surface as a harness defect to name explicitly;
- do not expand scope to make progress look larger.

A loop is complete only when the declared checks and evaluator agree, not when an agent feels finished.
