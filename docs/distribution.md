# Distribution channels

GitHub Releases are the source of truth for every sfh installation channel.
Homebrew must point to immutable, version-specific Release URLs; it does not
build a different binary.

## Release outputs

Pushing a `vX.Y.Z` tag runs `.github/workflows/release.yml`. The workflow is
intentionally stable-only: prerelease package versions and tags are rejected
instead of being silently marked as the latest stable release. After the full
CI gate, it publishes:

- five platform archives and their SHA-256 sidecars, with Authenticode- or
  Developer ID-signed executables where the platform supports native signing.
  Every archive has the same resource packet alongside `sfh`/`sfh.exe`:
  `release-resources.txt` and every path it lists (`README*`, `AGENTS.md`,
  maintainer/security documents, `docs/`, `schema/`, `examples/`, `skills/`,
  and `tests/`). Installer scripts are separate rendered Release assets, not
  members of the platform packet: embedding that packet's hash in a script
  inside the same packet would create a circular checksum;
- `sfh-installer.sh` and `sfh-installer.ps1`, with sidecars;
- `sfh.rb`, rendered with all four Unix archive hashes, with a sidecar;
- a source archive (`sfh-X.Y.Z-source.tar.gz`) taken with `git archive` from
  the exact tagged commit, with a sidecar;
- `provenance.json`, which ties all staged asset hashes to the tag, commit, and
  version, and `SHA256SUMS`, which hashes every other published file including
  `provenance.json`.

`scripts/render_distribution.py` is the only code that transfers binary hashes
into package-manager metadata. Do not maintain another handwritten Formula in
this repository.

`release-resources.txt` is the authoritative list of resource roots in every
platform archive. `scripts/release_assets.py` expands tracked files beneath
that contract for tar and zip packaging, verifies the exact member set, checks
local links in every packaged Markdown file, and writes the complete release
manifest. Do not add an OS-specific copy list to the workflow.

The Unix installer puts that resource packet in
`${SFH_DATA_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/sfh}`. The PowerShell
installer uses `$env:SFH_DATA_DIR` when set and
`$env:LOCALAPPDATA\sfh-resources` otherwise, and Homebrew uses its `pkgshare`.
The separate Windows tree keeps installer-managed resources apart from the
platform state fallback under `%LOCALAPPDATA%\sfh` (ordinary runs still use
the current project's `.sfh/runs` unless a state or runs directory is chosen).
`SFH_DATA_DIR` controls installer placement only; it is not an sfh runtime
setting. Each installer reports the installed resource path, while a manually
unpacked archive exposes the same files beside the binary.

Installers are serialized per user. The shell installer uses
`$HOME/.sfh-installer.lock`; PowerShell uses
`$env:LOCALAPPDATA\sfh-installer.lock`. A live lock stops a concurrent install.
If an interrupted process leaves a stale lock, the installer reports its exact
path and refuses to remove it automatically: inspect the recorded owner, prove
that process is no longer running, then remove only that reported lock before
retrying.

The installers create a private `.sfh-installer-owned` marker and a private
`.sfh-installer-inventory` in the resource destination. The inventory records
every installed directory and every regular file's unnamed-byte-stream SHA-256.
A later install requires the marker and an exact inventory match: unknown or
missing directory entries, type changes, unnamed content changes, links, and special files all stop the upgrade
before the binary or resource tree is changed. A manually extracted archive,
source checkout, or resource tree that has acquired runtime state is therefore
never deleted as installer-owned. Canonical binary and resource destinations
must not overlap in either direction. Both installers also reject a resource
destination that overlaps the effective sfh state directory (`SFH_STATE_DIR`
when set, otherwise the platform default).

For an official network install, the PowerShell installer requires a valid,
timestamped Authenticode signature before executing the staged Windows binary.
On macOS, the shell installer requires both strict `codesign` verification and
a successful Gatekeeper `spctl` assessment. `SFH_ASSET_DIR` is a trusted local
fixture override for installer development: it skips the network-path native
signature, embedded manifest, and release-bound archive hash gates, so it must
never point at untrusted or downloaded content. Do not store user data, runtime
state, alternate data streams, extended attributes, or custom ACLs in
`SFH_DATA_DIR`; platform metadata is outside the portable inventory contract.

Every rendered installer is release-bound. It embeds its stable version and
all five platform archive hashes, and accepts only that version. To install an
older release, first download and verify the installer from that exact tag;
setting `SFH_VERSION` on a newer installer does not retarget it. Windows and
macOS additionally pin the native publisher. The binary emits its embedded
resource manifest, which the installer byte-compares to an independently built
inventory of the extracted packet before changing a destination. On Linux,
the independently verified installer and its embedded archive hash are the
authenticity boundary; an unsigned binary's embedded manifest alone would only
prove internal consistency.

After proving immutability is enabled and that no release already exists for
the tag, the workflow creates a fresh draft and uploads only to the returned
release ID. It then creates GitHub
artifact attestations for every `SHA256SUMS` subject and for `SHA256SUMS`
itself, and only then publishes the draft. Immediately before publication, the
workflow queries GitHub's `immutable-releases` API and fails closed unless its
`.enabled` value is `true`; keep **Settings > General > Releases > Enable
release immutability** on before tagging. The setting applies only to releases
published after it was enabled: v1.6.0 and older releases are not retroactively
immutable. The final transition locks both the tag and assets and GitHub also
creates a release attestation. The release body is the current top section of
`CHANGELOG.md`; a stale or empty section fails the release.

Runs for the same tag are serialized. After attestations and the immutability
setting check, the workflow fetches the draft asset list again and compares
every remote digest to the local manifest immediately before publication. A
queued duplicate run therefore cannot replace files across that boundary.
The peeled remote tag must still equal the release commit derived from the
workflow ref (lightweight and annotated tags are both accepted);
repository rules allow the owner to create `v*` tags but allow nobody to update
or delete one after creation.
The publish API response must report `draft: false`, the expected tag, and
`immutable: true`; the job then fetches and verifies the locked asset digests
once more before it can succeed.
If any step fails after draft creation, compensation targets only that run's
recorded release ID. A draft or mutable published release is deleted and a 404
is required; an immutable release is preserved for manual review. Therefore a
failed release job must always be checked in the Releases page before retrying,
even though retries never reuse or overwrite an existing release.

## Verifying a release packet

A release is only as trustworthy as a reviewer's ability to check it against
what sfh's own release job recorded - a source archive whose `Cargo.toml`
does not match the tag it was published under is a real failure mode this is
meant to catch, not a hypothetical one.

Every tagged release publishes `provenance.json` alongside the binaries. The
`assets` object covers the release files present before provenance and the
complete `SHA256SUMS` file then binds `provenance.json` as well:

```json
{
  "schema_version": 1,
  "version": "1.6.1",
  "tag": "v1.6.1",
  "commit": "<full sha>",
  "source_archive": "sfh-1.6.1-source.tar.gz",
  "archive_sha256": "<sha256 of sfh-1.6.1-source.tar.gz>",
  "generated_utc": "<timestamp>",
  "assets": {
    "sfh-linux-x64.tar.gz": "<sha256>",
    "sfh-linux-x64.tar.gz.sha256": "<sha256>",
    "sfh-1.6.1-source.tar.gz": "<sha256>"
  }
}
```

It is generated after all build jobs from the checked-out tag and downloaded
artifacts, not hand-written. The earlier `release-contract` job fails before
anything is built if the tag, the `Cargo.toml` version, and `CHANGELOG.md`'s top
heading disagree with each other.

To check a downloaded packet against it:

```bash
# 1. What the source archive itself claims.
tar xzf sfh-<version>-source.tar.gz
grep '^version' sfh-<version>/Cargo.toml

# 2. What the tag actually points to, from a real clone (the archive has no
#    .git directory of its own to ask).
git clone https://github.com/Aero123421/SimpleFlowHarness && cd SimpleFlowHarness
git checkout "v<version>"
git rev-parse HEAD

# 3. What the binary you are about to trust reports at runtime.
sfh --version
```

All three must agree with each other and with `provenance.json`'s
`version`/`commit`. Verify the complete packet and GitHub build attestations:

```bash
sha256sum -c SHA256SUMS
gh attestation verify SHA256SUMS --repo Aero123421/SimpleFlowHarness \
  --signer-workflow Aero123421/SimpleFlowHarness/.github/workflows/release.yml \
  --source-ref "refs/tags/v<version>" --deny-self-hosted-runners
gh attestation verify sfh-<platform>.tar.gz --repo Aero123421/SimpleFlowHarness \
  --signer-workflow Aero123421/SimpleFlowHarness/.github/workflows/release.yml \
  --source-ref "refs/tags/v<version>" --deny-self-hosted-runners
gh release verify "v<version>"
gh release verify-asset "v<version>" sfh-<platform>.tar.gz
```

On macOS, use `shasum -a 256 -c SHA256SUMS` if GNU `sha256sum` is not
installed. `gh release verify` and `verify-asset` require release immutability.
Any disagreement means the packet in hand does not match the release it claims
to be - report it rather than trust it.

The one-line commands in the README execute the installer before it can verify
itself. To verify that bootstrap too, download a version-fixed installer and
its sidecar first. On Linux or macOS:

```bash
(
  set -eu
  version=1.6.1
  base="https://github.com/Aero123421/SimpleFlowHarness/releases/download/v$version"
  curl --proto '=https' --tlsv1.2 -fLO "$base/sfh-installer.sh"
  curl --proto '=https' --tlsv1.2 -fLO "$base/sfh-installer.sh.sha256"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c sfh-installer.sh.sha256
  else
    shasum -a 256 -c sfh-installer.sh.sha256
  fi
  gh attestation verify sfh-installer.sh \
    --repo Aero123421/SimpleFlowHarness \
    --signer-workflow Aero123421/SimpleFlowHarness/.github/workflows/release.yml \
    --source-ref "refs/tags/v$version" --deny-self-hosted-runners
  SFH_VERSION="$version" sh ./sfh-installer.sh
)
```

On Windows PowerShell:

```powershell
$version = "1.6.1"
$base = "https://github.com/Aero123421/SimpleFlowHarness/releases/download/v$version"
Invoke-WebRequest "$base/sfh-installer.ps1" -OutFile sfh-installer.ps1
Invoke-WebRequest "$base/sfh-installer.ps1.sha256" -OutFile sfh-installer.ps1.sha256
$expected = ((Get-Content ./sfh-installer.ps1.sha256 -Raw) -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash ./sfh-installer.ps1 -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) { throw "installer SHA-256 mismatch" }
gh attestation verify ./sfh-installer.ps1 --repo Aero123421/SimpleFlowHarness `
  --signer-workflow Aero123421/SimpleFlowHarness/.github/workflows/release.yml `
  --source-ref "refs/tags/v$version" --deny-self-hosted-runners
if ($LASTEXITCODE -ne 0) { throw "installer attestation verification failed" }
$env:SFH_VERSION = $version
& ./sfh-installer.ps1
```

## Native signing prerequisites

Tagged releases fail closed before the build matrix starts unless all ten
private signing/administration settings exist as secrets in the protected
`release` environment:

- `APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64`
- `APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_NOTARY_KEY_ID`
- `APPLE_NOTARY_ISSUER_ID`
- `APPLE_NOTARY_KEY_P8_BASE64`
- `WINDOWS_CODESIGN_PFX_BASE64`
- `WINDOWS_CODESIGN_PFX_PASSWORD`
- `WINDOWS_CODESIGN_TIMESTAMP_URL`
- `RELEASE_ADMIN_READ_TOKEN`

The Apple certificate must be a **Developer ID Application** certificate
exported as password-protected PKCS#12. `APPLE_SIGNING_IDENTITY` is its full
codesign identity. The notary values are an App Store Connect API key ID,
issuer ID, and base64-encoded `.p8` key. Both macOS binaries are signed with the
hardened runtime and secure timestamp, submitted with `notarytool --wait`, and
must return `Accepted` before packaging. The Intel archive is built and
executed natively on GitHub's `macos-15-intel` runner rather than merely
cross-compiled on arm64. The ten-character Team ID reported by `codesign` is a
public trust anchor stored in reviewed `release-signers.json`, not a secret
that can be changed alongside the signing credential.

The Windows certificate must be a password-protected Authenticode code-signing
certificate in PFX form. `WINDOWS_CODESIGN_TIMESTAMP_URL` is the certificate
provider's RFC 3161 timestamp endpoint; it is stored with the other release
settings even though the URL itself is not confidential. SignTool must both
sign and pass `/pa /all /tw` verification before packaging. The official
installer independently requires `Get-AuthenticodeSignature` to report
`Valid` with a timestamp certificate before it runs the staged executable.
The lowercase SHA-256 fingerprint of the signer certificate's DER bytes is the
other public trust anchor stored in reviewed `release-signers.json`. The build
checks the real signature against that pin before the renderer embeds it in
the PowerShell installer. Certificate rotation therefore requires an explicit
pin update and a new release.

`RELEASE_ADMIN_READ_TOKEN` is a fine-grained token restricted to this
repository with **Administration: read-only** permission. GitHub's
`immutable-releases` endpoint requires that permission, while the normal
workflow `GITHUB_TOKEN` cannot be granted it. The workflow uses this token only
for the two read-only setting checks before draft creation and immediately
before publishing.

GitHub applies protection rules to each job that references the `release`
environment. Depending on repository settings, one tagged workflow can ask the
required reviewer to approve more than once as the signing contract, build
matrix, and publish job become eligible. Each approval is expected; do not
bypass or remove the environment to make the run advance.

## Before tagging

1. Update `Cargo.toml`, `Cargo.lock`, and the top `CHANGELOG.md` heading to the
   exact release version. Update schema IDs and bundled schema pins, then
   re-review every skill's `target-sfh` claim, only when the public schema or
   release minor changes; patch releases retain the compatible minor pin.
2. Confirm release immutability is enabled, the owner-only `v*` creation
   ruleset, the no-bypass `v*` update/deletion ruleset, and the
   reviewer-gated `release` environment are active, both public signer pins in
   `release-signers.json` are set to the certificates being used, and all ten
   release secrets above are configured in that environment. A normal branch/PR needs
   no signing credentials; only a tag invokes the release workflow.
3. Run:

   ```bash
   cargo fmt --check
   cargo clippy --release --locked --all-targets -- -D warnings
   cargo test --release --locked
   python3 tests/distribution_checks.py
   ```

4. Confirm both installer tests pass in CI on the release commit.

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
