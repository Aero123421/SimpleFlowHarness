# Maintainer guide (for AI agents and humans)

sfh is a single-binary Rust workflow runner that chains AI coding CLIs and
shell commands into YAML-defined flows. Its one boundary rule shapes every
change: **the engine records process facts and follows declared routes; it
never judges whether an agent's work is good.** A change that makes sfh infer
quality, or that turns unprovable state into success, is wrong even if tests
pass.

Human contribution process lives in [CONTRIBUTING.md](CONTRIBUTING.md);
security boundaries in [SECURITY.md](SECURITY.md); release mechanics in
[docs/distribution.md](docs/distribution.md). This file is the map and the
invariants.

## Repository map

| Path | Role |
|---|---|
| `src/engine.rs` | Run loop: run-dir lifecycle, resume/carry-budget, budget landing, routing, dry-run/plan, validate entry |
| `src/flow.rs` | YAML schema and static validation (steps, routes, outcomes, profiles, workspace, contexts, replay) |
| `src/leaf.rs` | One step's prepare/execute: template-to-argv safety checks, per-tool output parsers, sessions, fan-out pool |
| `src/preset.rs` | All per-tool adapter facts: argv builders, `AdapterInfo` (flags, gaps, enforcement), escalation guard |
| `src/protocol.rs` | Fail-closed protocol evidence (`Plain/Valid/MissingTerminal/Invalid`) |
| `src/execute.rs` | Spawn, capture, interrupt tracking, detach, kill-tree, pid identity |
| `src/contain.rs` | Path containment: nofollow opens, private perms, atomic writes, nonces |
| `src/watch.rs` | `status`/`wait`/`stop`, owner-liveness proof (pid + start time) |
| `src/runs.rs`, `src/workspace.rs` | Run browsing (`runs why`), managed git worktrees with ownership markers |
| `src/preflight.rs`, `src/doctor.rs` | Offline capability check; real one-token probe |
| `src/context.rs`, `src/template.rs`, `src/closure.rs` | Named context bundles, `{{...}}` rendering, execution-closure pinning |
| `src/machine.rs` | `--json` envelope, stable `SFH_*` error codes |
| `src/guide.txt` | The AI-caller manual; line budget is test-enforced |
| `schema/` | Public JSON Schemas for flow, log events, status |
| `examples/`, `examples/ponytail/` | Top-level examples; 20 project-work flows with their own inputs/prompts |
| `skills/` | Authoring knowledge for AIs that *write* flows. **Not a runtime feature** — flows have no `skills:` key; never invent one |
| `tests/` | Shell/Python behaviour suites (`engine_behaviour.sh`, `independent_checks.sh`, `skills_checks.py`, `distribution_checks.py`) + fake-tool stub |
| `docs/` | Contracts and decision records; [`docs/README.md`](docs/README.md) says which files are current and which are historical |

## Gates (all enforced by CI on every pushed branch, 3 OSes)

```bash
cargo fmt --check
cargo clippy --release --locked --all-targets -- -D warnings
cargo test --release --locked
cargo deny check
cargo package --locked
python3 tests/distribution_checks.py
python3 tests/skills_checks.py
bash tests/engine_behaviour.sh ./target/release/sfh
bash tests/independent_checks.sh ./target/release/sfh
for f in examples/*.yaml; do ./target/release/sfh validate "$f" --strict; ./target/release/sfh plan "$f" >/dev/null; done
for f in examples/ponytail/[0-9][0-9]-*.yaml skills/sfh-*/assets/*.yaml; do ./target/release/sfh validate "$f" --strict; done
```

`--strict`, not plain `validate`. The README teaches `validate --strict` and CI
only ran the loose check, so 192 advisory findings accumulated across the
bundled flows while the pack's own validation report said one deliberate
resolution was all that remained. One flow is exempt by name
(`examples/ponytail/17-database-migration-dry-run.yaml`, which lands every
successful path on `stuck` so a person approves a migration); the exemption is
named in `ci.yml`, in the flow's header, and in the pack's validation report,
and `tests/skills_checks.py` checks all three still agree.

Ponytail flows are additionally validated against every bundled profile
overlay ("the overlay parses" and "the overlay applies to this flow" are
different claims). A docs-only change takes a light CI path, which still runs
`tests/distribution_checks.py`: every path in `release-resources.txt` is
hash-pinned in `release-content-manifest.txt`, so skipping that check let a
docs commit strand the manifest and fail the next unrelated pull request.
On Windows, run the shell suites from Git Bash; they make no AI calls. The
toolchain is pinned in `rust-toolchain.toml`.

## Invariants — do not break these

- **Fail closed.** Missing/invalid success, session, or protocol evidence is
  failure, never success. A parser that cannot prove the documented terminal
  record arrived must not pass raw stdout along as an answer.
- **No new dependencies** without an argued need. `cargo deny` gates; SHA-256
  is hand-rolled (`src/sha256.rs`) precisely to keep the tree small.
- **Cross-OS byte-identical decisions.** Fingerprints normalise line endings;
  anything a run dir records must compare equal across Windows/Unix checkouts.
- **`log.jsonl` is append-only and additive.** Never reinterpret an existing
  field; add a new one.
- **Stable vocabularies.** `SFH_*` error-code meanings and the JSON envelope
  are pinned to `machine::SCHEMA_VERSION`, not the release number. Exit codes
  are 0/1/2/4. Re-pointing any of these requires a schema-version bump.
- **Resume compatibility** is preserved or the migration is documented in
  `CHANGELOG.md`.
- **Adapter knowledge lives only in `src/preset.rs`.** Flags, protocols,
  version floors, probe hardening, and `known_gaps` are data on `AdapterInfo`,
  not scattered constants. `LAST_VERIFIED` moves only when someone actually
  re-verified the live CLIs — not when editing nearby code. The bar for
  claiming `Enforced` access is a CLI-guaranteed whole-class boundary; when
  in doubt, `BestEffort` (see the P0-02 doc comment on `Enforcement`).
- **Every bundled flow passes `validate --strict`**, and both READMEs' whole-flow
  examples strictly validate and dry-run. A flow or example shipped here is held
  to the checks this project tells readers to run; the single exemption above is
  the whole list.
- **`release-resources.txt` has deliberate inline duplicates, and they are
  checked.** An installer validating a downloaded archive cannot take that
  archive's own word for what belongs in it, so both installers repeat the
  contract inline. `tests/distribution_checks.py` compares every copy; adding a
  resource without updating them used to fail only the install jobs.
- **A number in a validation report is measured, never recalled.**
  `tests/skills_checks.py` recomputes the ratios those reports state. The rule
  exists because a report claimed "4 of 18 top-level examples" when there were
  17 and one was clean.
- **`src/guide.txt` has a 110-line budget** and every YAML fence in it must
  validate and dry-run — both test-enforced. Anything added there has to earn
  a line.
- **`schema/flow.schema.json` tracks every YAML format change**, and bundled
  flows pin a versioned `$schema` URL; `tests/skills_checks.py` fails on a
  minor version bump until the skills' `target-sfh` claims are re-reviewed.
  That failure is a deliberate signal, not noise.

## Culture the code expects

- Comments state constraints the code cannot show, and cite review provenance
  (`rev_break #N`, `P0-xx`, `S3-x`) where a defect was fixed. Match this
  density and style; do not narrate what the next line does.
- Test names are full behaviour sentences
  (`resume_refuses_a_visit_counter_that_cannot_advance`). A fix lands with a
  test that fails on the old behaviour.
- `tests/independent_checks.sh` is written from attack descriptions alone and
  was baselined against pre-fix binaries. Keep it independent: do not adapt it
  to match implementation details.
- Changes near resume, containment, session access, or the escalation guard
  are security-sensitive: think adversarially (tampered run dirs, symlinks,
  crafted logs) and add the adversarial test, not just the happy path.

## Where decisions live

- **[GitHub issues](https://github.com/Aero123421/SimpleFlowHarness/issues) are
  the work queue.** Start there before proposing new work. `docs/v1.6-backlog.md`
  used to be described here as the queue, and while it was, the only open piece
  of real work — a documentation gap a live run had already hit — lived in an
  issue that nobody following this file would ever see.
- `docs/v1.6-backlog.md` — boundaries deliberately left in place, and the
  continuing evaluation items. Read it *after* the issues: a proposal that lands
  on one of those boundaries has to argue the boundary first.
- `CHANGELOG.md` — user-visible changes, with reasoning (recent entries in
  Japanese; earlier in English — a language policy decision is pending, see
  the backlog).
- `docs/v1-backlog.md` — closed hardening rounds and accepted boundaries.
- `docs/machine-api.md` — the `--json` contract callers depend on.
- `docs/README.md` — index of the shipped docs tree, and which files are
  historical rather than current.

## Do not

- Weaken a fail-closed default to make a flow author's life easier; add an
  explicit opt-in instead, and record it as an escape hatch (SECURITY.md).
- Invent flow fields in docs, examples, or skills. If it is not in
  `schema/flow.schema.json`, it does not exist.
- Commit run directories, real prompts, credentials, or provider transcripts.
- Grow `sfh guide` past its budget or add a dependency casually.
- Move fast on `preset.rs` verification dates or `Enforcement` upgrades.
