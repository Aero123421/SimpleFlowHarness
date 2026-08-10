# Examples

All top-level YAML files are validated on Windows, Linux, and macOS CI.

All top-level `.yaml` files here are FLOWS. `profiles/` holds profile overlay
files, which are `--profiles` input rather than flows, and are not validated as
flows by CI.

Start with:

- `research.yaml` — multi-source research with parallel review and synthesis;
- `hypotheses.yaml` — generate, fan out, verify, and consolidate;
- `parallel-ideas.yaml` — compact parallel brainstorming;
- `smoke.yaml` / `smoke-parallel.yaml` — deterministic commands used by CI;
- `mini-check.yaml` — a small live-provider health check (it makes real AI
  calls and therefore is not part of CI).

v1.2 concepts:

- `workspace-smoke.yaml` — a managed workspace end to end with **no AI and no
  cost**. Run it inside any Git repository and check the result yourself:

  ```bash
  sfh run examples/workspace-smoke.yaml --state-dir /tmp/sfh-state
  sfh workspaces list --state-dir /tmp/sfh-state
  git worktree list        # one worktree, on an sfh/... branch
  git status               # your own checkout is untouched
  ```

- `managed-loop.yaml` — the same concepts with real tools: one workspace for the
  whole run, named context, declared `effects:`, and profiles that can be
  replaced from outside with `--profiles examples/profiles/local.yaml`. Its step
  ids and profile names are the flow author's words; sfh attaches no meaning to
  them.

Maintainer scenarios:

- `cross-os-gate.yaml` demonstrates environment-specific release gates. Its WSL
  distribution and path are intentionally placeholders to customize.
- `selfhost-*`, `v1-harden*`, `v1-review.yaml`, and `v1-council.yaml` preserve
  larger self-hosting/review workflows. They use `{{flow_dir}}` instead of a
  contributor's machine path, but still assume the sfh repository and relevant
  provider CLIs are available.

## `ponytail/` — flows for real repository work

`ponytail/` is a self-contained pack of 20 flows aimed at an existing codebase
rather than at demonstrating a primitive: regression-first bugfixes, dependency
elimination, independent review councils, chunked long builds, migration dry
runs with a human gate, and release-readiness loops. Each carries its own
`inputs/` and `prompts/` as named context, so the flows work without installing
anything else.

They are the exception to the "top-level only" rule above: CI validates every
flow in `ponytail/` by name, and validates each one against both
`profiles.*.example.yaml` overlays, because applying an overlay is a separate
claim from parsing one.

Run them from the target project, not from here — see
[ponytail/README.md](ponytail/README.md).

Every new flow should begin with:

```yaml
api_version: 1
```

Run `sfh validate example.yaml --strict`, then `sfh plan example.yaml`, before a
flow that invokes real AI tools.
