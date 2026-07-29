## Problem and contract

Describe the failure mode or user problem and the behavior this change promises.

## Compatibility

Describe effects on YAML, logs/status, exit codes, existing runs, and all three supported operating systems.

## Verification

- [ ] Added a regression test that fails without the change
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --release --locked --all-targets -- -D warnings`
- [ ] `cargo test --release --locked`
- [ ] `cargo deny check`
- [ ] Relevant shell suites and example validation
- [ ] Updated schema/docs/changelog when public behavior changed
