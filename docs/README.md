# Documentation index

Everything in this directory ships inside every release archive, so this index
exists to say what each file is *for* — and, just as importantly, which files
are historical records rather than current documentation.

Start elsewhere if you are new: [`../README.md`](../README.md) is the user
guide, `sfh guide` is the compact manual for an AI driving sfh, and
[`../schema/flow.schema.json`](../schema/flow.schema.json) is the authority on
the flow format. If a document here disagrees with the schema, the schema wins.

## Current

| File | What it is |
|---|---|
| [`machine-api.md`](machine-api.md) | The `--json` contract: envelope shape, stable `SFH_*` error codes, exit codes. Read this before driving sfh from a program. |
| [`distribution.md`](distribution.md) | How releases are built, signed, attested, and installed, and what each installation channel does and does not verify. |
| [`v1.6-backlog.md`](v1.6-backlog.md) | Boundaries deliberately left in place, and where the live work queue is. |

## Historical

Kept because the reasoning is worth preserving, not because they describe
current behaviour. Each carries a header saying what has changed since.

| File | What it is |
|---|---|
| [`v1-backlog.md`](v1-backlog.md) | Closed v1.0–v1.1 hardening rounds and the boundaries they settled. The regression suites, not this file, are the source of truth for what is enforced. |
| [`v1.1-spec.md`](v1.1-spec.md) | The v1.1 feature-proposal specification from 2026-07. Four of its Phase 3 items were never implemented; the file's header lists the measured status of each. Do not copy field names out of it. |

## Related, outside this directory

- [`../AGENTS.md`](../AGENTS.md) — maintainer guide: repository map, CI gates, invariants.
- [`../CONTRIBUTING.md`](../CONTRIBUTING.md) — how to propose a change.
- [`../SECURITY.md`](../SECURITY.md) — the trust boundaries sfh does and does not enforce.
- [`../CHANGELOG.md`](../CHANGELOG.md) — user-visible changes with reasoning.
- [`../skills/`](../skills/) — authoring knowledge for AIs that *write* flows. Not a runtime feature; flows have no `skills:` key.
