# Harness refresh: design and evidence, 2026-10-29

This records the v2.0 design and measured CLI contracts. Validation and release status
are recorded separately; this document does not claim publication or completion.

## Design decisions

Keep the single Rust binary, existing YAML execution model, one adapter-facts
table, bounded streaming parsers, append-only execution log, and OS-backed run
leases. They already provide the required foundation. Do not add a provider SDK,
agent framework, database, generic plugin protocol, or compatibility layer.

The changes address observed contract gaps:

1. **One deletion policy.** Manual cleanup and automatic retention must both
   prove completed (`done`) state, consistent ownership, a dead owner and absence of a
   managed workspace that still depends on the run manifest. Both acquire the
   existing deletion lease and repeat the checks before removal. Dry-run uses
   the same eligibility and lease checks and explains retained runs. Ambiguous
   or legacy evidence is kept. A lock file may be created by dry-run.
2. **Current CLI completion evidence.** Pi 1.4.0 distinguishes an assistant
   response and a low-level `agent_end` from session-level `agent_settled`.
   The adapter must require a typed, non-aborted settlement after the last
   automatic work. Token-limit truncation is not successful completion.
   Retain usage across retries and never treat absent reported cost as zero.
   OpenCode 2 is different: normal successful stdout can consist only of text
   events. Record `exit_bound`, require clean process exit, and never invent a
   terminal event. `step_finish` ends a model step, often a tool call, not the run.
   A nonzero exit cannot be excused by OpenCode's text or intermediate usage.
3. **Explicit tool surfaces.** Pi's current `--tools` can preserve MCP tools,
   accept wildcard selectors and modify defaults with `+`/`-`. Read/write
   presets must disable MCP as well as extension discovery. Explicit extension
   loading and selectors that can widen access require the existing deliberate
   override. No new access tier or blanket claim of sandbox enforcement.
4. **Scoped verification.** Adapter facts should state the tested version and
   scope of evidence. A successful help probe is not a successful model turn.
   Authenticated behavior, real CLI with a local mock provider, and synthetic
   parser fixtures must be reported separately.
5. **Owned process lifetime.** OpenCode 2 runs with `--standalone` so the
   existing process supervisor owns its private server; raw remote/shared
   server flags are refused. V2 permission configuration uses ordered
   `agents.<name>.permissions` with default deny and explicit read/write
   actions. Native plugins and managed policy remain outside this best-effort
   boundary. agy's internal timer can return partial success, so it is pinned
   to zero and SFH's external deadline is the only leaf deadline.
6. **Provable sessions only.** OpenCode 2 creates an unknown session ID and
   stdout lacks creation-marker/parent proof. Refuse `continue_from` and
   `fork_from` rather than claim continuity from an echoed ID. Durable context
   and explicit artifacts remain available; no SQLite inspection or second
   server API client is added just to reconstruct this missing contract.
7. **Consistent automation results.** Reuse the existing machine envelope for
   remaining bare-JSON commands and cleanup results. Do not invent another
   response format. Treat any changed bare response shape as an explicit
   breaking change in the new release; preserve existing run evidence.

No change to retry/replay defaults is justified: effects with an unknown result
must still stop for reconciliation. Existing per-tool concurrency limits and
run leases remain the coordination mechanism. Existing process-group and
Windows job-object cleanup remain the process boundary; escaping Unix process
groups still requires an external OS sandbox. SFH waits for its direct
children; killed orphan descendants can remain as zombies until the host's
reaper collects them. The Linux behavior test checks execution state as well
as PID existence, with a live-process negative control. In this container,
`kill -0` alone falsely called a terminated `sleep` zombie alive.

## Baseline and reproduction

Baseline main: `a527216ae4c3eb618174869875c590d7e923bd26`.
Issue [#40](https://github.com/Aero123421/SimpleFlowHarness/issues/40) is still
present at that exact SHA. Three new regression cases failed on the old
implementation: live owner despite terminal status, an exclusive lease held
by a resume, and unverifiable/unfinished evidence. They pass with the shared
deletion checks. A real-process regression also verifies dry-run and actual
clean retain a live run and remove it only after successful completion.

The September audit and PRs #29–#31 already fixed several session and parser
defects. See [the prior report](cli-verification-2026-09-07.md); those changes
are the baseline rather than new work attributed to this refresh.

## Current measurements

Official Linux x86-64 artifacts were downloaded and verified before execution.
GitHub archives were matched to release asset SHA-256 digests; the OpenCode 2
npm native package was matched to its registry SHA-512 integrity. Probes used
isolated configuration and no provider credentials. An unauthenticated failure
is not authenticated success. A local mock model tests the real CLI transport
and tool machinery, but not a real provider, model quality or billing.

| CLI | Actual version | New measurement | Support / limits |
| --- | --- | --- | --- |
| [Codex](https://github.com/openai/codex/releases/tag/rust-v0.165.0) | 0.165.0 | version, root/exec/resume/fork help; no-login; start events then test-side network timeout | Existing adapter; authenticated success and continuity untested |
| [Claude Code](https://github.com/anthropics/claude-code/releases/tag/v2.1.312) | 2.1.312 | version/help; no-login JSON exit 1, `is_error: true` despite `subtype: success` | Existing typed error check retained; authenticated success/permissions untested |
| [OpenCode](https://registry.npmjs.org/@opencode/cli/2.2.1) | 2.2.1 | version/root/run/session help; real CLI with loopback model: text-only success and denied shell request | V2 fresh standalone runs; verified native resume/fork unsupported; usage can be absent |
| [Pi](https://github.com/earendil-works/pi/releases/tag/v1.4.0) | 1.4.0 | version/help; no-key error; real CLI with loopback model, read tool, final settlement | Current JSON mode; no real provider/billing; read/write disable MCP/extensions |
| [agy](https://github.com/google-antigravity/antigravity-cli/releases/tag/1.4.10) | 1.4.10 | version/help, timeout default 0 and expanded efforts; this environment's isolated no-auth probe hit test-side 10s timeout | Current argv; authenticated turns untested; worker separately observed no-auth JSON exit 1 |
| [Grok](https://x.ai/cli) | current listing 1.0.70; no current binary here | Official installer download denied 403 in independent worker | Historical adapter retained, latest binary unverified; no fresh compatibility claim |
| [Cursor](https://cursor.com/cli) | current listing 2026.10.22-27dcf5f; no current binary here | Official installer download denied 403 in independent worker | Historical adapter retained, latest binary unverified; no fresh compatibility claim |

GitHub's OpenCode `/releases/latest` returned **v1.19.5**. That official binary
was also probed (version/help and exit-1 UnknownError), but it is not the latest
v2 product. V2's official package is `@opencode/cli`; Pi's official repository
is `earendil-works/pi` and package `@earendil-works/pi-coding-agent`. No unofficial
same-name package was installed.

### Reproducible source contracts

- [Codex non-interactive mode](https://developers.openai.com/codex/non-interactive-mode):
  start events alone cannot certify completion; the adapter requires the turn verdict.
- [Claude CLI reference](https://code.claude.com/docs/en/cli-reference): JSON
  results and session options. The measured auth failure demonstrates why
  `subtype` alone is insufficient; the prior September fix already checks the bool.
- [OpenCode v2 CLI](https://opencode.ai/v2/docs/cli),
  [permissions](https://opencode.ai/v2/docs/permissions), and
  [tagged noninteractive writer](https://github.com/anomalyco/opencode/blob/v2.2.1/packages/cli/src/run/noninteractive.ts):
  private server lifetime, v2 rule names, step events, terminal process result.
  [Tagged configuration loader](https://github.com/anomalyco/opencode/blob/v2.2.1/packages/core/src/config.ts)
  loads inline configuration after project configuration and before managed settings.
- [Pi JSON events](https://github.com/earendil-works/pi/blob/v1.4.0/packages/coding-agent/docs/json.md),
  [CLI integration](https://github.com/earendil-works/pi/blob/v1.4.0/packages/coding-agent/docs/cli-integration.md),
  [session implementation](https://github.com/earendil-works/pi/blob/v1.4.0/packages/coding-agent/src/core/agent-session.ts):
  settlement after auto-work, typed stop reason and abort state. RPC command
  acknowledgments are not task success; this release keeps the simpler JSON mode.
- [agy changelog](https://github.com/google-antigravity/antigravity-cli/blob/0281b054c762e6194fd7d76e23145373e01568ef/CHANGELOG.md):
  internal print deadline can return partial response as success; outer timeout
  avoids treating that as complete work.

### Download identities (SHA-256 of archive bytes)

| Official artifact | SHA-256 |
| --- | --- |
| Codex `codex-x86_64-unknown-linux-musl.tar.gz`, rust-v0.165.0 | `be635d3486a5bd060d0fff582ae6d30160280c31344dd85719707fa920e187f0` |
| Claude `claude-linux-x64.tar.gz`, v2.1.312 | `d9324231c4e3dfe342e853f56c752348ae060f9f221cdf4b0f90ef1dc47ee907` |
| OpenCode `@opencode/cli-linux-x64` 2.2.1 npm tarball | `42cf1dddf7d1abadcee895ac9c66489ec994b8c650b1f4aec9d3b3e7d07d9a5e` |
| Pi `pi-linux-x64.tar.gz`, v1.4.0 | `f2e4e4329603223d03260cdb8bd5033dbdc1f73ba58ab8939db2e56546e115d2` |
| agy `agy_cli_linux_x64.tar.gz`, 1.4.10 | `ed0526231e3916b6cc1b025cf356862e88cc036e27c3c5eae5a580495fc79773` |

Exact complete mock-provider stdout and hashes are committed as
[regression fixtures](../tests/fixtures/cli-2026-10/README.md). Fixture usage
and cost are synthetic, not spend. The OpenCode permission probe returned a
structured tool error for `shell` and created no sentinel file.

Independent Standard-tier Terra reproduced #40 with official SFH v1.6.1 and a
disposable live run: clean removed the run directory, then final persistence
failed (exit 2). Sol measured 44 CLI cases, including Pi provider-error exit 0,
RPC abort exit 0, and OpenCode unknown-session creation. These are independent
research results, **not an independent review of this patch**. The reported
Sol evidence ZIP (126208 bytes, SHA-256
`7a64852190b2a2356e7fc1ce881e4c80597d79e3d012dcb5d40158c6d512a9e7`)
could not be materialized in this environment using the official Library
helper, including one authorized retry. Its hash is received provenance,
not locally verified. Local fixtures above are our separate captures.

The real OpenCode 2 SFH smoke returned `LOCAL MOCK OK` with `exit_bound`.
An SFH one-second timeout returned failure, recorded `timed_out: true`, and
left no owned OpenCode process. This exposed an error-classification gap:
expected EOF after an SFH deadline was labelled missing terminal. The fix
preserves protocol facts but gives the timeout/cancellation classification
precedence; a process fixture guards timeout even after a completed text event.

### Additional interfaces considered

[Gemini CLI headless](https://geminicli.com/docs/cli/headless/) and
[ACP](https://agentclientprotocol.com/get-started/introduction) offer useful
integration surfaces. ACP's documentation currently distinguishes v1 from a
v2 draft. Neither is added as a native adapter in this release: no new live
contract was measured here, and adding a bidirectional client, provider SDK,
planner, compactor or subagent framework would not fix the observed bugs.
Explicit `cmd:` wrappers remain available with author-owned exit/artifact gates.

## Migration

1. Preserve the old binary and run directories. Complete/reconcile interrupted
   old runs with their original execution definition. Start v2 work in new run
   directories; do not blanket `--force-resume` across an adapter change.
2. Use OpenCode 2 (`@opencode/cli`) and Pi 1.4.0 for the measured contracts;
   pin versions through `require_version` where reproducibility matters.
   Set OpenCode `model: provider/model` plus `effort`, or `model#variant` alone.
   Pass prior output through `contexts`/templates instead of OpenCode native
   resume/fork. Keep `agent:` in the profile rather than overriding it in args.
3. Update JSON consumers: all commands share the schema-1 envelope;
   run count `ok` becomes `successful_steps`, the stored binary version becomes
   `run_sfh_version`, and `runs why.error` becomes `run_error`.
   See [machine API](machine-api.md). No existing log field is reinterpreted.
4. Treat `access` as the reported per-tool guarantee. Neither a successful
   model turn nor a tool allowlist proves artifact quality or an OS sandbox.
   Keep deterministic gates after agent work. Pair USD ceilings with wall-clock
   limits when coverage is incomplete, especially with OpenCode 2.
5. `runs clean --dry-run --json` explains retained evidence. Live, locked,
   contradictory, failed/stuck/stopped/dead, legacy/unverifiable and still-managed
   workspace runs remain. Only a proven `done` run is eligible.
   No force-delete mode or automatic conversion of old data is introduced.

## Release gates

The release remains pending until the final commit passes Rust fmt/clippy/tests,
behavior and independent regression suites, dependency/license checks, schema
and bundled-flow checks, cross-OS CI, package and clean-install smoke, an
independent review and fix recheck, and the existing release inventory and
provenance gates. Compare the published remote tag and every downloaded
distribution packet to that final commit. Preserve all existing releases and
run data.

## Independent patch review follow-up

Terra reproduced deletion of a resumable interrupted run and a Ctrl+C envelope
misclassified as `SFH_FLOW_INVALID` at fad1f9c8. New real-process tests failed
before the fixes and pass afterward. Cleanup now requires `done` before and
after the lease, preserving all other states without duplicating the engine's
resume diagnosis. Engine interrupt checks, including fallback checkpoints,
carry the canonical `SFH_INTERRUPTED` prefix into status and JSON envelopes.
The process regression checks SIGINT with incomplete structured output, stop,
dry-run, actual cleanup, automatic retention, successful resume, and deletion
after completion. CLI output in this regression is a synthetic fixture.
Independent re-review of the fixes remains a separate gate.

Sol also reproduced acceptance of agy's single-dash Go flag spelling and three
malformed OpenCode text parts. Fresh/resume builders now reject both dash
spellings, with separated or equals values. OpenCode requires typed text parts,
matching outer/inner session IDs and numeric start/end times in order. Mutations
of the captured valid transcript fail as protocol invalid; the unchanged real
transcript still succeeds. Unit and process fixtures cover these counterexamples;
they do not claim that the real CLI emitted the malformed records.
