# sfh review status and remaining boundaries

This file replaces the raw, machine-local review transcript that originally
lived here. It keeps the useful decision record without stale absolute paths
or line numbers.

## Closed hardening findings

The v1.0–v1.1 review rounds produced adversarial cases for path containment,
resume trust, process ownership, permission mapping, cross-platform behavior,
and fan-out recovery. The maintained regression suites are the source of truth:

- [`tests/engine_behaviour.sh`](../tests/engine_behaviour.sh)
- [`tests/independent_checks.sh`](../tests/independent_checks.sh)
- unit tests next to the implementation

The following classes are now enforced and tested:

- run artifacts and resume inputs are contained; symlink/reparse traversal and
  malformed management files fail closed;
- `status`, `wait`, and `stop` authenticate the owning run using nonce, PID,
  process identity, and process start time;
- process-tree termination covers children and grandchildren on all supported
  operating systems;
- access-changing CLI arguments and session access escalation are rejected
  unless an explicit escape hatch is present;
- run-derived values cannot select executables, working directories, or shell
  script text by accident;
- legacy run compatibility is selected only for genuinely legacy metadata;
- missing or invalid success/session fields do not become success;
- step IDs are case-insensitively unique and terminal names are reserved;
- Linux, macOS, and Windows CI runs on every pushed branch.

## v1.1.2 complex-flow hardening

- Fan-out member completion is persisted immediately. A crash while another
  member is running no longer reruns the completed member.
- Every required event/artifact/status write is fail-closed.
- Resume pins the full effective configuration, including global profiles.
- Wall-clock usage is cumulative across resume and includes fan-out queue time.
- Session sources and mandatory step-output dependencies must dominate their
  consumers in the control-flow graph.
- Intentional branch-optional data uses `| optional` or `| default:text`.
- Strict validation reports implicit fallthrough and unreachable nodes.
- `plan`, `graph`, `config show`, `runs why`, and fan-out status expose what a
  large workflow will do and why.

## Accepted boundaries

These are deliberate product boundaries, not silently ignored defects:

1. **sfh is not an OS sandbox.** `access` maps to provider flags. The launched
   process still runs as the current OS user.
2. **A user who can modify the run directory can modify its history.** Run
   artifacts are private-by-default inputs for crash recovery, not a
   cryptographically signed audit log. Unix permissions are set to 0700/0600;
   Windows uses inherited ACLs.
3. **Reported cost is a soft guard.** sfh cannot reserve unreported in-flight
   provider spend or enforce a provider billing limit.
4. **Provider CLIs drift.** `sfh doctor <flow.yaml>` probes current command and
   output compatibility, but cannot guarantee future private storage formats.
5. **The v1 format is a routed sequence with fan-out, not an arbitrary nested
   DAG.** `parallel` and `foreach` are one-level execution primitives.
   Hierarchical subflows, typed ports, joins across independently scheduled
   DAG nodes, and distributed workers require a future API version rather than
   being smuggled into a patch release.

## Legacy review identifiers

Some maintainer examples refer to these historical labels:

- **B-12** provider silence: closed by the idle clock, `hang_after_sec`, and
  transient retry classification.
- **B-13** case-only step IDs: closed by case-insensitive uniqueness.
- **B-14** Windows run ACL strength: retained as accepted boundary 2 above.
- **B-15** session tests: closed by
  [`tests/stub/session_stub.rs`](../tests/stub/session_stub.rs).
- **B-16** fallback access reconciliation: constrained to access levels already
  declared in the fingerprinted flow; stronger cryptographic log integrity is
  outside the local-run trust model.

New unresolved defects should be filed as GitHub issues and linked here only
after they have a reproducible case and an explicit acceptance criterion.
