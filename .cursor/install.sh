#!/usr/bin/env bash
# Cloud Agent install script for sfh (SimpleFlowHarness).
#
# Idempotent, non-interactive dependency refresh and build. Safe to run
# repeatedly against cached or partially prepared state. sfh is a single-binary
# Rust CLI with no runtime services, so there is no `start`/`terminals` phase.
set -euo pipefail

cd "$(dirname "$0")/.."

# cargo-deny powers the dependency audit in CONTRIBUTING.md ("cargo deny check")
# and CI. The pinned project toolchain (Rust 1.85) is too old to compile a
# current cargo-deny, and CVSS 4.0 advisories require >= 0.20, so install a
# prebuilt binary instead of building it from source. Best-effort: a download
# failure must not block the core build below.
CARGO_DENY_VERSION="0.20.2"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
install_cargo_deny() {
  if command -v cargo-deny >/dev/null 2>&1 &&
    cargo-deny --version 2>/dev/null | grep -q "${CARGO_DENY_VERSION}"; then
    echo "cargo-deny ${CARGO_DENY_VERSION} already present"
    return 0
  fi
  local arch tarball url tmp
  case "$(uname -m)" in
    x86_64) arch="x86_64-unknown-linux-musl" ;;
    aarch64 | arm64) arch="aarch64-unknown-linux-musl" ;;
    *) echo "cargo-deny: unsupported arch $(uname -m); skipping" >&2; return 0 ;;
  esac
  tarball="cargo-deny-${CARGO_DENY_VERSION}-${arch}.tar.gz"
  url="https://github.com/EmbarkStudios/cargo-deny/releases/download/${CARGO_DENY_VERSION}/${tarball}"
  tmp="$(mktemp -d)"
  if curl --proto '=https' --tlsv1.2 -sSfL "$url" -o "$tmp/cd.tgz" &&
    tar -xzf "$tmp/cd.tgz" -C "$tmp" --strip-components=1 &&
    install -m 0755 "$tmp/cargo-deny" "${CARGO_BIN}/cargo-deny"; then
    echo "installed cargo-deny ${CARGO_DENY_VERSION}"
  else
    echo "cargo-deny: install failed; continuing without it" >&2
  fi
  rm -rf "$tmp"
}
install_cargo_deny

# Fetch dependencies and build the release binary the smoke/engine test suites
# and example flows consume (./target/release/sfh).
cargo build --release --locked

echo "sfh install complete: $(./target/release/sfh --version)"
