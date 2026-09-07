# Validation

Every number here was produced by running the `sfh` binary built from this
checkout, and CI re-runs the same commands on Linux, macOS, and Windows.

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

**9/9 `*/assets/*.yaml` pass `sfh validate --strict`** in CI on Linux, macOS,
and Windows. `--strict`, not plain `validate`: these assets are the flows the
skills tell readers to start from, so they are held to the checks the skills
teach readers to run.

The pack shipped originally with no `sfh` binary available to test against, and
that gap hid a real defect: `sfh-eval-engineering/assets/failure-to-regression.yaml`
wrote `{when_label_is: reproduced, goto:fix}`. Without the space, YAML reads
`goto:fix` as a key name rather than `goto` with the value `fix`, so the file was
not a valid flow at all — it failed plain `sfh validate`, not just `--strict`.
A YAML-parses-and-has-a-`steps`-list check cannot catch that. It is fixed.

An earlier version of this file listed accepted `--strict` findings — four
assets resolving `workspace.mode: auto`, four combining `effects: external`
with `replay.unfinished: rerun`. Neither survives today: all nine assets are
strict-clean, and the section was describing findings that no longer existed.
`tests/skills_checks.py` now recomputes the ratio in this file from the tree, so
a stale number fails CI instead of being read as a result.

`external-effect-safe.yaml` still carries the distinction that section was
about, in the flow itself rather than in prose: its `apply` step mutates and is
`replay.unfinished: stuck` with `retry_on: never`; its `verify` step only reads
and may rerun.

## Not claimed

Skill activation is model behavior and is not verified here — see `EVALS.md`.
For Agent Skills strict validation, run `skills-ref validate` when the reference
CLI is installed; the bundled validator implements the relevant structural
checks but is not the upstream reference library.
