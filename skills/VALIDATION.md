# Validation

Checked against the `sfh` binary built from this repository at v1.5.1.

## Structure

- 9 `SKILL.md` files pass frontmatter/name/description/reference checks
  (`python3 skills/tools/validate_skills.py skills`).
- Every main `SKILL.md` is below the 500-line progressive-disclosure limit.
- 4 bundled Python scripts return usable `--help`.
- `gh_ci_gate.py` fixtures verified exit 0 success, 10 transport failure,
  20 completed CI failure, 30 identity/SHA mismatch, and 40 bounded watch
  timeout.
- The web-search and MCP report validators accept valid fixtures and reject
  malformed or incomplete ones.
- The heuristic linter detects implicit `latest` CI watching, implicit writer
  workspace, and unbounded cycle nodes in a deliberately broken fixture.

## Bundled flow assets

All 9 `*/assets/*.yaml` pass `sfh validate` in CI on Linux, macOS, and Windows.

The pack shipped originally with no `sfh` binary available to test against, and
that gap hid a real defect: `sfh-eval-engineering/assets/failure-to-regression.yaml`
wrote `{when_label_is: reproduced, goto:fix}`. Without the space, YAML reads
`goto:fix` as a key name rather than `goto` with the value `fix`, so the file was
not a valid flow at all — it failed plain `sfh validate`, not just `--strict`.
A YAML-parses-and-has-a-`steps`-list check cannot catch that. It is fixed.

## Accepted `--strict` findings

`--strict` is advisory here, as it is for this repository's own examples.

- **4 assets: `workspace.mode: auto` resolved to one managed git worktree.**
  `auto` is the documented idiom; strict reports the resolution so it shows up
  in a plan.
- **4 assets: `effects: external` with `replay.unfinished: rerun`.** These are
  read-only probes — a web query, an allowlisted read-only MCP call, an
  observation of a CI run pinned to an exact run ID and head SHA, and the
  status check in `external-effect-safe.yaml`. `sfh` cannot read inside a
  command, so it flags every external step that would re-run on resume. Each of
  the four now carries a comment saying why re-asking is the same question, and
  what change would make `stuck` the correct answer instead.
  `external-effect-safe.yaml` is the file that makes the distinction the point:
  its `apply` step mutates and is `stuck` + `retry_on: never`; its `verify` step
  only reads and is `rerun`.

## Not claimed

Skill activation is model behavior and is not verified here — see `EVALS.md`.
For Agent Skills strict validation, run `skills-ref validate` when the reference
CLI is installed; the bundled validator implements the relevant structural
checks but is not the upstream reference library.
