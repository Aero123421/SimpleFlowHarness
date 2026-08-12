#!/bin/sh

set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 /path/to/sfh" >&2
  exit 2
fi

SCRIPT_DIR="$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
case "$1" in
  /*) BINARY="$1" ;;
  *) BINARY="$(pwd)/$1" ;;
esac
[ -x "$BINARY" ] || {
  echo "not an executable: $BINARY" >&2
  exit 2
}

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) ASSET="sfh-linux-x64.tar.gz" ;;
  Linux-aarch64 | Linux-arm64) ASSET="sfh-linux-arm64.tar.gz" ;;
  Darwin-x86_64) ASSET="sfh-macos-x64.tar.gz" ;;
  Darwin-arm64 | Darwin-aarch64) ASSET="sfh-macos-arm64.tar.gz" ;;
  *)
    echo "unsupported test host: $(uname -s)-$(uname -m)" >&2
    exit 2
    ;;
esac

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/sfh-installer-test.XXXXXX")"
cleanup() {
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

PACKAGE_DIR="$TEST_ROOT/package"
ASSET_DIR="$TEST_ROOT/assets"
INSTALL_DIR="$TEST_ROOT/installed"
mkdir -p "$PACKAGE_DIR" "$ASSET_DIR"
cp "$BINARY" \
  "$REPO_ROOT/README.md" \
  "$REPO_ROOT/README.ja.md" \
  "$REPO_ROOT/LICENSE" \
  "$PACKAGE_DIR/"

tar czf "$ASSET_DIR/$ASSET" \
  -C "$PACKAGE_DIR" sfh README.md README.ja.md LICENSE
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$ASSET_DIR" && sha256sum "$ASSET" >"$ASSET.sha256")
else
  (cd "$ASSET_DIR" && shasum -a 256 "$ASSET" >"$ASSET.sha256")
fi

EXPECTED_VERSION="$("$BINARY" --version)"
SFH_ASSET_DIR="$ASSET_DIR" \
SFH_INSTALL_DIR="$INSTALL_DIR" \
SFH_NO_MODIFY_PATH=1 \
  sh "$REPO_ROOT/installers/sfh-installer.sh"

ACTUAL_VERSION="$("$INSTALL_DIR/sfh" --version)"
[ "$ACTUAL_VERSION" = "$EXPECTED_VERSION" ] || {
  echo "installed version mismatch: $ACTUAL_VERSION != $EXPECTED_VERSION" >&2
  exit 1
}

PROFILE_HOME="$TEST_ROOT/home"
mkdir "$PROFILE_HOME"
HOME="$PROFILE_HOME" \
SHELL=/bin/bash \
SFH_ASSET_DIR="$ASSET_DIR" \
  sh "$REPO_ROOT/installers/sfh-installer.sh"
HOME="$PROFILE_HOME" \
SHELL=/bin/bash \
SFH_ASSET_DIR="$ASSET_DIR" \
  sh "$REPO_ROOT/installers/sfh-installer.sh"
[ "$("$PROFILE_HOME/.local/bin/sfh" --version)" = "$EXPECTED_VERSION" ]
PROFILE_VERSION="$(
  HOME="$PROFILE_HOME" /bin/bash -c \
    '. "$HOME/.bashrc"; sfh --version'
)"
[ "$PROFILE_VERSION" = "$EXPECTED_VERSION" ]
# The profile must contain variables for the future shell to expand.
# shellcheck disable=SC2016
[ "$(grep -Fxc 'export PATH="$HOME/.local/bin:$PATH"' "$PROFILE_HOME/.bashrc")" -eq 1 ]

BAD_ASSET_DIR="$TEST_ROOT/bad-assets"
mkdir "$BAD_ASSET_DIR"
cp "$ASSET_DIR/$ASSET" "$ASSET_DIR/$ASSET.sha256" "$BAD_ASSET_DIR/"
printf 'corrupt' >>"$BAD_ASSET_DIR/$ASSET"

if SFH_ASSET_DIR="$BAD_ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/rejected" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/error.log"; then
  echo "installer accepted a corrupted archive" >&2
  exit 1
fi
grep -F "SHA-256 mismatch" "$TEST_ROOT/error.log" >/dev/null

echo "Unix installer checks passed ($EXPECTED_VERSION, $ASSET)"
