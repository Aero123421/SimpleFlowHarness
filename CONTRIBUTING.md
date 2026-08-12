# Contributing to sfh

Thank you for helping improve sfh. Bug reports, portability findings,
documentation fixes, and focused pull requests are welcome.

Maintenance sessions driven by an AI agent should start from
[AGENTS.md](AGENTS.md), which holds the repository map and the invariants this
document assumes.

## Before opening a change

For behavior changes, open an issue first when the public YAML format, durable
log/status formats, exit codes, or resume semantics could change. Small fixes
can go directly to a pull request.

Please keep sfh's boundary intact: the engine records process facts and follows
declared routes; it should not infer whether an agent's work is good.

## Development setup

The repository pins its Rust toolchain in `rust-toolchain.toml`.

```bash
cargo build --locked
cargo test --locked
cargo fmt --check
cargo clippy --release --locked --all-targets -- -D warnings
```

Before submitting:

```bash
cargo build --release --locked
cargo deny check
bash tests/engine_behaviour.sh ./target/release/sfh
bash tests/independent_checks.sh ./target/release/sfh
for f in examples/*.yaml; do
  ./target/release/sfh validate "$f" --strict
  ./target/release/sfh plan "$f"
done
cargo package --locked
python3 tests/distribution_checks.py
```

On Windows, run the shell suites from Git Bash. They contain no real AI calls;
the session behavior is exercised by a local Rust stub.
`cargo deny` is also enforced in CI; install a current release separately if
your pinned project toolchain is too old to compile the audit utility itself.

## Change requirements

- Add tests that fail on the old behavior and prove the new behavior.
- Keep Windows, Linux, and macOS behavior aligned.
- Update `schema/flow.schema.json` whenever the YAML format changes.
- Keep `log.jsonl` append-only and additive. Never silently reinterpret an
  existing field.
- Preserve resume compatibility or document a deliberate migration.
- Add user-visible changes to `CHANGELOG.md`.
- Use `api_version: 1` in new example flows.
- Do not commit real prompts, credentials, provider transcripts, or run
  directories.

## Pull requests

Keep commits reviewable and explain:

1. the failure mode or user problem;
2. the chosen contract;
3. compatibility implications; and
4. exact verification performed.

By contributing, you agree that your contribution is licensed under the MIT
license in this repository.

Maintainers cutting a release must also follow
[the distribution-channel checklist](docs/distribution.md).
