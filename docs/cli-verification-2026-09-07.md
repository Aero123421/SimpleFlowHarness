# CLI verification: 2026-09-07

This is a dated measurement, not a promise about future CLI releases. The
baseline was the reviewed PR #27 tree (`11df471`), followed by the fixes
described below. Probes ran on Linux in an isolated working directory with
explicit time limits. They used `access: read` and a fixed short response.
No project implementation, credentials, or provider transcripts are included
in this report.

## Versions and observed behavior

| Adapter | Existing installation | New version checked | Result |
|---|---|---|---|
| Codex | 0.151.0-alpha.7.1 | [0.153.4](https://github.com/openai/codex/releases/tag/rust-v0.153.4) | Fresh, resume, and fork completed; resume retained the session ID and fork returned a different ID. |
| Claude Code | 2.1.241 | [2.1.263](https://github.com/anthropics/claude-code/releases/tag/v2.1.263) | Required flags present. Both versions returned a structured authentication failure; a successful model turn and session continuity remain unverified in this environment. |
| OpenCode | 1.18.21 | [1.18.29](https://github.com/anomalyco/opencode/releases/tag/v1.18.29) | Fresh, resume, and fork completed with the expected session relationships. |
| Pi | 0.84.4 | [0.85.1](https://github.com/earendil-works/pi/releases/tag/v0.85.1) | Fresh, resume, and fork completed. Resume retained both session ID and timestamp; fork returned a new ID. |
| Grok | 1.0.5 | 1.0.13 | Required flags checked. The official stable pointer selected 1.0.13, while that binary identified itself as `[alpha]`. Both versions required login, so successful provider output remains unverified. |
| Antigravity | 1.1.25 | [1.1.25](https://antigravity.google/docs/cli/install/) | Fresh and resume completed with the same conversation ID. Headless fork remains unsupported. |
| Cursor Agent | Absent | Official installer advertised 2026.09.02-c22c1a3 | The package download returned HTTP 403. No current binary or successful turn was verified. |

The four GitHub release archives were checked against their release asset
SHA-256 digests before execution. They were unpacked into a separate test
directory; the existing installations were retained. Grok was obtained from
the versioned URL selected by its [official installer](https://x.ai/cli/install.sh).
Its reported channel discrepancy is recorded above rather than relabeled.

The real session flows completed 11 steps: three each for Codex, OpenCode,
and Pi, and two for Antigravity. All 11 step records reported a valid protocol
and exit zero. Accounting fields were parsed where supplied; this does not
establish billing accuracy or imply that an unreported cost is zero.

## Bugs exposed by the verification

- Preflight inspected top-level help for Codex and OpenCode, then looked for
  flags published only by `exec --help` or `run --help`. It incorrectly
  blocked working installations. Adapter metadata now selects the appropriate
  help command. A missing flag still blocks the adapter.
  Matching uses complete option tokens, and command facts require a usage
  header. Failed or interrupted help commands are reported as unreadable.
- Codex fork detection accepted the interactive `fork` command in root help
  as proof of headless `exec fork`. The probe now checks the exec command's
  own help and requires its fork subcommand.
- Codex's attached short option values bypassed the access guard. The 0.153.4
  binary accepted all three tested attached sandbox/config forms with `--help`,
  while rejecting an unknown-option control. Those forms now receive the same
  access checks as separated arguments.
- The Claude parser treated a missing or non-boolean `is_error` as success.
  It now requires the boolean verdict defined by the
  [official SDK parser](https://github.com/anthropics/claude-agent-sdk-python/blob/main/src/claude_agent_sdk/_internal/message_parser.py).
- Fresh Claude calls now require the returned session ID to match the ID sfh
  supplied. Missing or different IDs fail with `SFH_SESSION_UNVERIFIED` and are
  not recorded as successful sessions. An optional durable `failure_code`
  preserves that classification when routing resumes after a crash. This rule
  is limited to Claude; other adapters need their own identity-contract review
  before it is generalized (issue #28).
- Doctor could accept text without certified terminal evidence, or a nonzero
  exit from a CLI whose exit code is trustworthy. It now checks both. Agy's
  documented exit-code exception still requires certified success. Repeating
  doctor against the new versions confirmed Codex, OpenCode, Pi and Agy pass;
  Claude and Grok now show their in-band authentication diagnosis.
- Claude's variadic tool lists, `--allowed-tools` alias, `default` selector,
  and newer shell/code tools could evade the extra-argument access guard.
  The guard now inspects every list value and recognizes the documented
  command-execution tools. The
  [Claude tools reference](https://code.claude.com/docs/en/tools-reference)
  supplies the tool behavior; 2.1.263 help supplies the argument grammar.
- Pi's `powershell` tool was absent from its access guard. Its
  [official registry](https://github.com/earendil-works/pi/blob/v0.85.1/packages/coding-agent/src/core/tools/index.ts)
  lists it as a built-in shell tool. Explicit requests now require full access
  or the existing deliberate override, including rendered argument values.
- The guard also recognizes Cursor's documented `-f` permission alias, Grok's
  `--allowedTools` alias, and Claude's `auto` mode, which can approve shell
  commands through a classifier. Sources: [Cursor parameters](https://cursor.com/docs/cli/reference/parameters),
  [Grok reference](https://docs.x.ai/build/cli/reference), and
  [Claude auto mode](https://code.claude.com/docs/en/auto-mode-config).
- Cursor's parser accepted missing success verdicts and ignored a contradictory
  `is_error` field. It now checks both fields in the
  [documented success envelope](https://cursor.com/docs/cli/reference/output-format).
  The shared single-envelope parser also refuses duplicate terminal records;
  an earlier error cannot be replaced by a later success record. These changes
  were checked with synthetic fixtures, not a live Cursor turn.
- PR #27 originally removed all `CLAUDE_*` variables, including documented
  authentication and provider settings. Its merged version preserves operator
  configuration while removing inherited host/session state. Synthetic
  environment probes verified both preservation and removal without using
  credentials.
- A historical `stuck` decision continued to authorize changed variables
  after the next attempt had started and crashed. The merged fix consumes
  that exception at `run_start`; a new `stuck` decision can authorize another
  correction.

The latest Claude authentication response had `type: result`,
`subtype: success`, and `is_error: true`. The subtype alone must therefore
never be used as proof of success. The boolean error verdict remains
authoritative.

## Scope and remaining limits

These probes verify invocation, protocol parsing, and the observed session
relationships. They do not prove that write/full access, every provider,
custom hooks, MCP servers, or all user configurations behave identically.
The existing access enforcement classifications and documented gaps remain
in force. Windows and macOS regression CI uses fake tools; the real CLI
session calls reported here ran on Linux.

Cursor's documented executable `agent` is not a reason to rename this
adapter. Its [official installer](https://cursor.com/install) creates both
`agent` and `cursor-agent`, and other products also install an `agent`
command. The explicit `cursor-agent` default remains appropriate.

Re-run `sfh preflight` after installing or updating a CLI, and `sfh doctor`
when authenticated real-call verification is needed. A successful preflight
does not establish authentication or successful provider communication.
