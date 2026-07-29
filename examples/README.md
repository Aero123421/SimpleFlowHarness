# Examples

All top-level YAML files are validated on Windows, Linux, and macOS CI.

Start with:

- `research.yaml` — multi-source research with parallel review and synthesis;
- `hypotheses.yaml` — generate, fan out, verify, and consolidate;
- `parallel-ideas.yaml` — compact parallel brainstorming;
- `smoke.yaml` / `smoke-parallel.yaml` — deterministic commands used by CI;
- `mini-check.yaml` — a small live-provider health check (it makes real AI
  calls and therefore is not part of CI).

Maintainer scenarios:

- `cross-os-gate.yaml` demonstrates environment-specific release gates. Its WSL
  distribution and path are intentionally placeholders to customize.
- `selfhost-*`, `v1-harden*`, `v1-review.yaml`, and `v1-council.yaml` preserve
  larger self-hosting/review workflows. They use `{{flow_dir}}` instead of a
  contributor's machine path, but still assume the sfh repository and relevant
  provider CLIs are available.

Every new flow should begin with:

```yaml
api_version: 1
```

Run `sfh validate example.yaml --strict`, then `sfh plan example.yaml`, before a
flow that invokes real AI tools.
