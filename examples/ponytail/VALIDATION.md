# Validation report

Every number here was produced by running the `sfh` binary built from this
checkout, and CI re-runs the same commands on Linux, macOS, and Windows. Nothing
in this file is a recollection.

The pack originally shipped as a standalone zip whose validation report could
only claim YAML/schema checks: the environment that generated it had no `sfh`
executable. Bringing it into this repository is what made a real check possible,
and the real check found things the schema pass could not.

## What CI enforces

- **20/20 flow files pass `sfh validate`.**
- **19/20 pass `sfh validate --strict`**, and the twentieth is the documented
  exception below. CI runs `--strict` per flow, so this ratio is a gate, not a
  claim — a new advisory finding in any of the 19 fails the build.
- 40/40 flow × overlay combinations validate: every flow against each of the two
  `profiles.*.example.yaml` files. "The overlay parses" and "the overlay applies
  to this flow" are different claims, and the README makes the second.
- Every referenced context file resolves and stays under the pack directory.
- Route, `on_error`, `on_max_visits`, and `on_budget` targets resolve to a
  top-level step or a terminal.
- Parallel child steps use no forbidden child `goto` error action.
- Writer `foreach` uses `max_parallel: 1`.
- Review fan-outs use `when_members`, so a reviewer that failed to run cannot
  vote PASS merely because some other step printed the word.

`profiles.*.example.yaml` are overlays, not flows, and are never validated as
flows.

## The one accepted `--strict` finding

**`17-database-migration-dry-run.yaml`: the flow entry cannot reach `end` or
`fail`.** The report is correct and the design is deliberate. A migration must
not decide for itself that it may be applied, so every successful path lands on
`stuck` (exit 4) and hands the decision to a person. CI validates this one file
without `--strict` and the flow says so in its own header. Every other flow in
the pack is under the stricter gate.

## What a real `sfh validate` found

**Implicit fall-through (52 occurrences, fixed).** Three flows routed
`prove_failure` with `{when_exit: 0, goto: stuck}` and no catch-all, so every
other exit code fell through **implicitly to whichever step came next in the
file**. That was the intended target, but only by accident of ordering:
reordering the steps would have silently rerouted the flow. Fixing those three
was the first pass; a later `--strict` audit found the same shape in 52 places
across 17 flows, all of them now carrying an explicit `route: [{goto: <next>}]`.

**Implicit workspace resolution (17 occurrences, fixed).** Those flows declared
`workspace.mode: auto` and relied on sfh resolving it to one managed git
worktree because a writer step exists. The resolution was correct, but it was a
consequence of the declared effects rather than a stated intent: adding or
removing a writer would have changed where the run's side effects went, silently.
They now declare `workspace.mode: git-worktree`, which is what `sfh guide`
itself shows and what `sfh-flow-design` tells authors to prefer for a writer
flow. The behaviour is unchanged; only the intent is now written down.

An earlier version of this file reported 4 of "18" top-level examples as
strict-clean. There have been 17 top-level examples at every tag since v1.4.0,
and the count was never measured. `tests/skills_checks.py` now recomputes the
ratios in this file from the tree, so a stale number fails CI instead of being
read as a result.

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

`preflight` is the step that checks the CLIs and programs actually exist, and it
is the one CI cannot run for you. Run it in the environment that will launch the
flow — a scheduler or service unit does not read your login shell's `PATH`.
