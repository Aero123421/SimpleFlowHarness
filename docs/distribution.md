# Distribution channels

GitHub Releases are the source of truth for every sfh installation channel.
Homebrew must point to immutable, version-specific Release URLs; it does not
build a different binary.

## Release outputs

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`. After the full CI
gate, it publishes:

- five platform archives and their SHA-256 sidecars;
- `sfh-installer.sh` and `sfh-installer.ps1`, with sidecars;
- `sfh.rb`, rendered with all four Unix archive hashes, with a sidecar.

`scripts/render_distribution.py` is the only code that transfers binary hashes
into package-manager metadata. Do not maintain another handwritten Formula in
this repository.

## Before tagging

1. Update `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, schema IDs, examples, and
   pinned documentation to the same version.
2. Run:

   ```bash
   cargo fmt --check
   cargo clippy --release --locked --all-targets -- -D warnings
   cargo test --release --locked
   python3 tests/distribution_checks.py
   ```

3. Confirm both installer tests pass in CI on the release commit.

The Release workflow refuses a tag that differs from the Cargo package or
changelog version.

## Homebrew Tap

The public Tap is `Aero123421/homebrew-tap`. After a Release succeeds, download
its `sfh.rb`, replace `Formula/sfh.rb` in the Tap, and run:

```bash
brew audit --strict --online Aero123421/tap/sfh
brew install --build-from-source Aero123421/tap/sfh
brew test Aero123421/tap/sfh
```

Commit and push only after the Formula installs the released version on each
available macOS/Linux architecture. The Tap may automate copying `sfh.rb` from
the latest Release, but its test must still execute before committing.
