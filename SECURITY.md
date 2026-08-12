# Security policy

## Supported versions

The latest stable v1.x release receives security fixes. Older releases may be
used to reproduce a report but are not maintained.

## Reporting a vulnerability

Please do not open a public issue for a vulnerability that could expose local
files, execute unintended commands, escape a declared permission boundary, or
tamper with resume/run artifacts.

Use GitHub's private vulnerability reporting from the repository's
**Security → Advisories → Report a vulnerability** page. Include:

- the affected version and operating system;
- a minimal flow and commands needed to reproduce it;
- the expected and actual trust boundary;
- whether symlinks, junctions, detached processes, or a resumed run are
  involved; and
- any proposed mitigation.

Do not include live credentials, session tokens, or private provider output.
You should receive an acknowledgement within seven days. Fix timing depends on
severity and reproducibility; coordinated disclosure is preferred.

## Boundary reminder

sfh launches tools with the current user's OS rights. Its `access` setting maps
to provider CLI flags and is **not** an operating-system sandbox. Only codex has
a real sandbox behind it; for every other preset, `access` is a request to that
CLI's own permission system, and the CLI's user or project configuration can
widen it. `sfh preflight` reports, per tool and per level, whether sfh believes
enforcement is `sandboxed`, `enforced`, `best-effort` or `unsupported`, along
with the gaps it knows about. Treat anything it cannot see - MCP servers, hooks,
skills, plugins, auto-update - as `unknown` rather than as absent.

Escape hatches intentionally weaken normal checks, and each is recorded as an
unsafe override in the plan, the execution closure and the run's metadata:

- `--force-resume` waives the flow and execution-closure comparison. It does
  **not** adopt a changed workspace, and it does not restore a resumed session's
  recorded access level - every restored level drops to unknown, which the
  per-step guard fails closed on.
- `--adopt-workspace` accepts a workspace that changed underneath a resume. It
  waives only that, and only for the run it is given to.
- `sfh workspaces remove --discard` is the one command that drops uncommitted
  work. Nothing sfh does automatically ever will.
- `unsafe_shell_template`, `allow_access_override` and
  `allow_dynamic_exec_paths` behave as they have since v1.0.
- `contexts.<name>.allow_external` permits reading a context file outside the
  flow directory and the workspace, including through a symlink.
- `workspace.allow_concurrent_writers` permits two potential writers in one
  workspace at once, which sfh cannot then tell apart.

## What is not a secret

`--var` values are not secrets: they are recorded in the run's `meta.json` and
may be rendered into prompts. sfh still has no first-class secret input or
provider.

Prompts are treated as sensitive in the durable command log: an adapter that
delivers the prompt through argv records it as `<prompt chars=N sha256=...>`
rather than as text. The prompt itself is still written to the run directory
(`<step>.prompt.txt`), which is created 0700 and gitignored, because a step
cannot be reproduced without it. Environment values are redacted from
`sfh config show` unless `--show-secrets` is passed.
