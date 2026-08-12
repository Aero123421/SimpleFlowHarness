# Validation report

Checked against the `sfh` binary built from this repository at v1.6.0.

The pack originally shipped as a standalone zip whose validation report could
only claim YAML/schema checks: the environment that generated it had no `sfh`
executable. Bringing it into this repository is what made a real check possible,
and the real check found things the schema pass could not.

## Continuously verified in CI

- 20/20 flow files pass `sfh validate` on Linux, macOS, and Windows.
- Every referenced context file resolves and stays under the pack directory.
- Route, `on_error`, `on_max_visits`, and `on_budget` targets resolve to a
  top-level step or a terminal.
- Parallel child steps use no forbidden child `goto` error action.
- Writer `foreach` uses `max_parallel: 1`.
- Review fan-outs use `when_members`, so a reviewer that failed to run cannot
  vote PASS merely because some other step printed the word.
- `profiles.*.example.yaml` are overlays, not flows. CI applies
  `profiles.codex-pi.example.yaml` to a flow rather than validating it as one.

## Found by the first real run of `sfh validate`

Three flows routed `prove_failure` with `{when_exit: 0, goto: stuck}` and no
catch-all, so every other exit code fell through **implicitly to whichever step
came next in the file**. That is the intended target, but only by accident of
ordering: reordering the steps would have silently rerouted the flow. Each now
writes `{goto: implement}` explicitly, which `validate --strict` had been asking
for and the schema pass could never have seen.

## Accepted `--strict` findings

`validate --strict` is an advisory pass, not a gate — only 4 of this
repository's own 18 top-level examples are strict-clean either. What remains
here is deliberate:

- **17 flows: `workspace.mode: auto` resolved to one managed git worktree.**
  `auto` is the idiom `sfh guide` teaches and the repository's own
  `managed-loop.yaml` and `workspace-smoke.yaml` use. Strict reports the
  resolution so it is visible in a plan, not because it is wrong. The resolution
  it reports — exactly one worktree per run, no matter how many writer steps or
  loop visits — is what this pack's README promises.

## Not claimed

`validate` proves the flow is well-formed. It does not prove the environment.
These flows call `cargo`, `python -m pytest`, `npm`, and migration tooling that
CI here does not have, and they name AI CLIs (`codex`, `pi`, `claude`) that are
not installed on the runners. Before spending money on a flow:

```bash
sfh validate <flow.yaml> --strict --profiles <profiles.yaml>
sfh preflight <flow.yaml> --profiles <profiles.yaml> --json
sfh plan <flow.yaml> --profiles <profiles.yaml> --json --save
```

`preflight` is the step that checks the CLIs and programs actually exist, and
it is the one CI cannot run for you.
