#!/bin/sh

set -eu
unset SFH_STATE_DIR

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
INSTALLER_HOME="$TEST_ROOT/installer-home"
mkdir "$INSTALLER_HOME"
HOME="$INSTALLER_HOME"
export HOME
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
DATA_DIR="$TEST_ROOT/data"
mkdir -p "$PACKAGE_DIR" "$ASSET_DIR"
python3 "$REPO_ROOT/scripts/release_assets.py" package \
  --binary "$BINARY" \
  --asset "$ASSET_DIR/$ASSET"
tar xzf "$ASSET_DIR/$ASSET" -C "$PACKAGE_DIR"

EXPECTED_VERSION="$("$BINARY" --version)"
grep -F "EXPECTED_APPLE_TEAM_ID='{{APPLE_TEAM_ID}}'" \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F "INSTALLER_VERSION='{{VERSION}}'" \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
for checksum_placeholder in \
  LINUX_ARM64_SHA256 LINUX_X64_SHA256 MACOS_ARM64_SHA256 MACOS_X64_SHA256; do
  grep -F "{{${checksum_placeholder}}}" \
    "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
done
grep -F 'codesign --verify --strict "$STAGED_PATH"' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F 'spctl --assess --type execute "$STAGED_PATH"' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F 'TeamIdentifier=' "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F '"$STAGED_PATH" __release-manifest >"$EMBEDDED_INVENTORY"' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F 'cmp -s "$EMBEDDED_INVENTORY" "$STAGED_INVENTORY"' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F 'lock_path="$HOME/.sfh-installer.lock"' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F 'EMBEDDED_SHA256="$(embedded_sha256_for_asset "$ASSET")"' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F '[ "$EXPECTED" = "$EMBEDDED_SHA256" ]' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F '[ "$ARCHIVE_SIZE" -le 52428800 ]' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F '[ "$ARCHIVE_MEMBER_COUNT" -le 2000 ]' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
[ "$(grep -Fxc '  ! (trap - EXIT; validate_resource_inventory "$DATA_TRANSACTION/previous"); then' \
  "$REPO_ROOT/installers/sfh-installer.sh")" -eq 1 ]
grep -F 'validate_resource_inventory "$transaction_previous"' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
grep -F 'delete_inventoried_resource_tree "$DATA_TRANSACTION/previous"' \
  "$REPO_ROOT/installers/sfh-installer.sh" >/dev/null
[ "$(grep -Fxc 'validate_activated_resources "$DATA_DIR" "$STAGED_INVENTORY" "$EXPECTED_MANIFEST"' \
  "$REPO_ROOT/installers/sfh-installer.sh")" -eq 2 ]

STALE_LOCK="$HOME/.sfh-installer.lock"
mkdir "$STALE_LOCK"
printf '2147483647\ndead process\nstale-test\n' >"$STALE_LOCK/owner"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/stale-lock-install" \
   SFH_DATA_DIR="$TEST_ROOT/stale-lock-data" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/stale-lock-error.log"; then
  echo "installer automatically removed a stale global lock" >&2
  exit 1
fi
grep -F "stale sfh installer lock requires inspection and manual removal: $STALE_LOCK" \
  "$TEST_ROOT/stale-lock-error.log" >/dev/null
[ -f "$STALE_LOCK/owner" ]
[ ! -e "$TEST_ROOT/stale-lock-install/sfh" ]
[ ! -e "$TEST_ROOT/stale-lock-data" ]
rm "$STALE_LOCK/owner"
rmdir "$STALE_LOCK"
INSTALL_OUTPUT="$(
  SFH_ASSET_DIR="$ASSET_DIR" \
  SFH_INSTALL_DIR="$INSTALL_DIR" \
  SFH_DATA_DIR="$DATA_DIR" \
  SFH_NO_MODIFY_PATH=1 \
    sh "$REPO_ROOT/installers/sfh-installer.sh"
)"
printf '%s\n' "$INSTALL_OUTPUT"
printf '%s\n' "$INSTALL_OUTPUT" | grep -F "Installed resources to $DATA_DIR" >/dev/null

ACTUAL_VERSION="$("$INSTALL_DIR/sfh" --version)"
[ "$ACTUAL_VERSION" = "$EXPECTED_VERSION" ] || {
  echo "installed version mismatch: $ACTUAL_VERSION != $EXPECTED_VERSION" >&2
  exit 1
}

verify_resources() {
  data="$1"
  cmp "$PACKAGE_DIR/release-resources.txt" "$data/release-resources.txt"
  while IFS= read -r resource || [ -n "$resource" ]; do
    case "$resource" in
      */) diff -r "$PACKAGE_DIR/${resource%/}" "$data/${resource%/}" >/dev/null ;;
      *) cmp "$PACKAGE_DIR/$resource" "$data/$resource" ;;
    esac
  done <"$PACKAGE_DIR/release-resources.txt"
}

verify_resources "$DATA_DIR"
[ "$(cat "$DATA_DIR/.sfh-installer-owned")" = "sfh installer resource directory v1" ]
[ -f "$DATA_DIR/.sfh-installer-inventory" ]
sed 's/^[^ ]* [^ ]* //' "$DATA_DIR/.sfh-installer-inventory" \
  >"$TEST_ROOT/inventory-paths.txt"
LC_ALL=C sort -c "$TEST_ROOT/inventory-paths.txt"
if grep -E ' \.sfh-installer-(owned|inventory)/?$' \
  "$DATA_DIR/.sfh-installer-inventory" >/dev/null; then
  echo "installer inventory includes its private metadata" >&2
  exit 1
fi

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

COLLIDING_STATE_DIR="$DATA_DIR/runtime-state"
COLLIDING_STATE_SENTINEL="$COLLIDING_STATE_DIR/runs/keep.txt"
mkdir -p "$(dirname "$COLLIDING_STATE_SENTINEL")"
printf 'keep runtime state\n' >"$COLLIDING_STATE_SENTINEL"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$INSTALL_DIR" \
   SFH_DATA_DIR="$DATA_DIR" \
   SFH_STATE_DIR="$COLLIDING_STATE_DIR" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/state-overlap-error.log"; then
  echo "installer accepted an explicit state directory inside its resource destination" >&2
  exit 1
fi
grep -F "state directory must not overlap" "$TEST_ROOT/state-overlap-error.log" >/dev/null
[ "$(cat "$COLLIDING_STATE_SENTINEL")" = "keep runtime state" ]
rm -rf "$COLLIDING_STATE_DIR"

PLATFORM_STATE_HOME="$TEST_ROOT/platform-state-home"
PLATFORM_STATE_DIR="$PLATFORM_STATE_HOME/.local/state/sfh"
mkdir -p "$PLATFORM_STATE_DIR/workspaces"
printf 'keep platform state\n' >"$PLATFORM_STATE_DIR/workspaces/keep.txt"
if HOME="$PLATFORM_STATE_HOME" \
   XDG_STATE_HOME="$PLATFORM_STATE_HOME/.local/state" \
   SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/platform-state-install" \
   SFH_DATA_DIR="$PLATFORM_STATE_DIR" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/platform-state-overlap-error.log"; then
  echo "installer accepted the platform state directory as its resource destination" >&2
  exit 1
fi
grep -F "state directory must not overlap" "$TEST_ROOT/platform-state-overlap-error.log" >/dev/null
[ "$(cat "$PLATFORM_STATE_DIR/workspaces/keep.txt")" = "keep platform state" ]
[ ! -e "$TEST_ROOT/platform-state-install" ]

OVERLAP_ROOT="$TEST_ROOT/overlap"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$OVERLAP_ROOT/bin" \
   SFH_DATA_DIR="$OVERLAP_ROOT/missing/../bin/sfh/resources" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/overlap-error.log"; then
  echo "installer accepted overlapping binary and resource destinations" >&2
  exit 1
fi
grep -F "must not overlap" "$TEST_ROOT/overlap-error.log" >/dev/null
[ ! -e "$OVERLAP_ROOT" ]

if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$OVERLAP_ROOT/parent/child/bin" \
   SFH_DATA_DIR="$OVERLAP_ROOT/parent/missing/.." \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/overlap-reverse-error.log"; then
  echo "installer accepted a resource destination containing the install directory" >&2
  exit 1
fi
grep -F "must not overlap" "$TEST_ROOT/overlap-reverse-error.log" >/dev/null
[ ! -e "$OVERLAP_ROOT" ]

mkdir -p "$OVERLAP_ROOT/bin"
ln -s "$OVERLAP_ROOT/bin" "$TEST_ROOT/overlap-link"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$OVERLAP_ROOT/bin" \
   SFH_DATA_DIR="$TEST_ROOT/overlap-link/resources" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/overlap-link-error.log"; then
  echo "installer accepted overlapping destinations through a symbolic link" >&2
  exit 1
fi
grep -F "must not overlap" "$TEST_ROOT/overlap-link-error.log" >/dev/null
[ ! -e "$OVERLAP_ROOT/resources" ]
[ ! -e "$OVERLAP_ROOT/bin/resources" ]

MALFORMED_INSTALL_DIR="$TEST_ROOT/malformed-install"
MALFORMED_DATA_DIR="$TEST_ROOT/malformed-data"
mkdir -p "$MALFORMED_INSTALL_DIR/sfh"
cp -R "$DATA_DIR" "$MALFORMED_DATA_DIR"
printf 'previous\n' >"$MALFORMED_DATA_DIR/previous.txt"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$MALFORMED_INSTALL_DIR" \
   SFH_DATA_DIR="$MALFORMED_DATA_DIR" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/malformed-error.log"; then
  echo "installer accepted a directory at the binary destination" >&2
  exit 1
fi
grep -F "binary destination is a directory" "$TEST_ROOT/malformed-error.log" >/dev/null
grep -F "previous" "$MALFORMED_DATA_DIR/previous.txt" >/dev/null

DUPLICATE_ASSET_DIR="$TEST_ROOT/duplicate-assets"
mkdir "$DUPLICATE_ASSET_DIR"
tar czf "$DUPLICATE_ASSET_DIR/$ASSET" \
  -C "$PACKAGE_DIR" \
  sfh release-resources.txt \
  AGENTS.md CHANGELOG.md CONTRIBUTING.md LICENSE README.ja.md README.md README.md \
  SECURITY.md SUPPORT.md docs examples schema skills tests
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$DUPLICATE_ASSET_DIR" && sha256sum "$ASSET" >"$ASSET.sha256")
else
  (cd "$DUPLICATE_ASSET_DIR" && shasum -a 256 "$ASSET" >"$ASSET.sha256")
fi
if SFH_ASSET_DIR="$DUPLICATE_ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/duplicate-install" \
   SFH_DATA_DIR="$TEST_ROOT/duplicate-data" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/duplicate-error.log"; then
  echo "installer accepted a duplicate archive member" >&2
  exit 1
fi
grep -F "duplicate member: README.md" "$TEST_ROOT/duplicate-error.log" >/dev/null
[ ! -e "$TEST_ROOT/duplicate-install/sfh" ]
[ ! -e "$TEST_ROOT/duplicate-data" ]

UNKNOWN_RESOURCE_DIR="$DATA_DIR/examples/.runtime-state/runs"
UNKNOWN_RESOURCE_SENTINEL="$UNKNOWN_RESOURCE_DIR/keep.txt"
mkdir -p "$UNKNOWN_RESOURCE_DIR"
printf 'keep unknown runtime state\n' >"$UNKNOWN_RESOURCE_SENTINEL"
cp "$DATA_DIR/.sfh-installer-inventory" "$TEST_ROOT/inventory-before-unknown.txt"
BINARY_BEFORE_UNKNOWN="$(file_sha256 "$INSTALL_DIR/sfh")"
README_BEFORE_UNKNOWN="$(file_sha256 "$DATA_DIR/README.md")"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$INSTALL_DIR" \
   SFH_DATA_DIR="$DATA_DIR" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/unknown-resource-error.log"; then
  echo "installer replaced a resource tree containing an unknown nested entry" >&2
  exit 1
fi
grep -F "does not match its installer inventory" "$TEST_ROOT/unknown-resource-error.log" >/dev/null
[ "$(cat "$UNKNOWN_RESOURCE_SENTINEL")" = "keep unknown runtime state" ]
cmp "$TEST_ROOT/inventory-before-unknown.txt" "$DATA_DIR/.sfh-installer-inventory"
[ "$(file_sha256 "$INSTALL_DIR/sfh")" = "$BINARY_BEFORE_UNKNOWN" ]
[ "$(file_sha256 "$DATA_DIR/README.md")" = "$README_BEFORE_UNKNOWN" ]
rm -rf "$DATA_DIR/examples/.runtime-state"

MISSING_INVENTORY_DATA="$TEST_ROOT/missing-inventory-data"
MISSING_INVENTORY_INSTALL="$TEST_ROOT/missing-inventory-install"
cp -R "$DATA_DIR" "$MISSING_INVENTORY_DATA"
mkdir "$MISSING_INVENTORY_INSTALL"
cp "$INSTALL_DIR/sfh" "$MISSING_INVENTORY_INSTALL/sfh"
rm "$MISSING_INVENTORY_DATA/README.md"
MISSING_BINARY_BEFORE="$(file_sha256 "$MISSING_INVENTORY_INSTALL/sfh")"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$MISSING_INVENTORY_INSTALL" \
   SFH_DATA_DIR="$MISSING_INVENTORY_DATA" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/missing-inventory-error.log"; then
  echo "installer replaced a resource tree with an inventoried file missing" >&2
  exit 1
fi
grep -F "does not match its installer inventory" "$TEST_ROOT/missing-inventory-error.log" >/dev/null
[ ! -e "$MISSING_INVENTORY_DATA/README.md" ]
[ "$(file_sha256 "$MISSING_INVENTORY_INSTALL/sfh")" = "$MISSING_BINARY_BEFORE" ]

TYPE_CHANGE_DATA="$TEST_ROOT/type-change-data"
TYPE_CHANGE_INSTALL="$TEST_ROOT/type-change-install"
cp -R "$DATA_DIR" "$TYPE_CHANGE_DATA"
mkdir "$TYPE_CHANGE_INSTALL"
cp "$INSTALL_DIR/sfh" "$TYPE_CHANGE_INSTALL/sfh"
rm "$TYPE_CHANGE_DATA/SUPPORT.md"
mkdir "$TYPE_CHANGE_DATA/SUPPORT.md"
printf 'keep type change\n' >"$TYPE_CHANGE_DATA/SUPPORT.md/keep.txt"
TYPE_BINARY_BEFORE="$(file_sha256 "$TYPE_CHANGE_INSTALL/sfh")"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$TYPE_CHANGE_INSTALL" \
   SFH_DATA_DIR="$TYPE_CHANGE_DATA" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/type-change-error.log"; then
  echo "installer replaced a resource tree with a file/directory type change" >&2
  exit 1
fi
grep -F "does not match its installer inventory" "$TEST_ROOT/type-change-error.log" >/dev/null
[ "$(cat "$TYPE_CHANGE_DATA/SUPPORT.md/keep.txt")" = "keep type change" ]
[ "$(file_sha256 "$TYPE_CHANGE_INSTALL/sfh")" = "$TYPE_BINARY_BEFORE" ]

CONTENT_CHANGE_DATA="$TEST_ROOT/content-change-data"
CONTENT_CHANGE_INSTALL="$TEST_ROOT/content-change-install"
cp -R "$DATA_DIR" "$CONTENT_CHANGE_DATA"
mkdir "$CONTENT_CHANGE_INSTALL"
cp "$INSTALL_DIR/sfh" "$CONTENT_CHANGE_INSTALL/sfh"
printf 'locally edited resource\n' >"$CONTENT_CHANGE_DATA/README.md"
CONTENT_BINARY_BEFORE="$(file_sha256 "$CONTENT_CHANGE_INSTALL/sfh")"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$CONTENT_CHANGE_INSTALL" \
   SFH_DATA_DIR="$CONTENT_CHANGE_DATA" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/content-change-error.log"; then
  echo "installer overwrote a locally modified resource" >&2
  exit 1
fi
grep -F "does not match its installer inventory" "$TEST_ROOT/content-change-error.log" >/dev/null
[ "$(cat "$CONTENT_CHANGE_DATA/README.md")" = "locally edited resource" ]
[ "$(file_sha256 "$CONTENT_CHANGE_INSTALL/sfh")" = "$CONTENT_BINARY_BEFORE" ]

LINK_DATA="$TEST_ROOT/link-data"
LINK_INSTALL="$TEST_ROOT/link-install"
cp -R "$DATA_DIR" "$LINK_DATA"
mkdir "$LINK_INSTALL"
cp "$INSTALL_DIR/sfh" "$LINK_INSTALL/sfh"
ln -s "$TEST_ROOT" "$LINK_DATA/examples/runtime-link"
LINK_BINARY_BEFORE="$(file_sha256 "$LINK_INSTALL/sfh")"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$LINK_INSTALL" \
   SFH_DATA_DIR="$LINK_DATA" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/link-error.log"; then
  echo "installer replaced a resource tree containing a symbolic link" >&2
  exit 1
fi
grep -F "link or special file" "$TEST_ROOT/link-error.log" >/dev/null
[ -L "$LINK_DATA/examples/runtime-link" ]
[ "$(file_sha256 "$LINK_INSTALL/sfh")" = "$LINK_BINARY_BEFORE" ]

SFH_ASSET_DIR="$ASSET_DIR" \
SFH_INSTALL_DIR="$INSTALL_DIR" \
SFH_DATA_DIR="$DATA_DIR" \
SFH_NO_MODIFY_PATH=1 \
  sh "$REPO_ROOT/installers/sfh-installer.sh"
verify_resources "$DATA_DIR"

CONCURRENT_ROOT="$TEST_ROOT/concurrent"
CONCURRENT_INSTALL="$CONCURRENT_ROOT/install"
CONCURRENT_DATA="$CONCURRENT_ROOT/data"
COPY_WRAPPER_DIR="$TEST_ROOT/copy-wrapper"
COPY_MARKER="$TEST_ROOT/first-installer-copying"
COPY_CLAIM="$TEST_ROOT/first-installer-copying.claim"
mkdir -p "$CONCURRENT_ROOT" "$COPY_WRAPPER_DIR"
cat >"$COPY_WRAPPER_DIR/cp" <<'EOF'
#!/bin/sh
if [ -n "${SFH_TEST_CP_MARKER:-}" ] &&
  (umask 077 && set -C && : >"$SFH_TEST_CP_CLAIM") 2>/dev/null; then
  : >"$SFH_TEST_CP_MARKER"
  sleep 2
fi
exec "$SFH_TEST_REAL_CP" "$@"
EOF
chmod 0755 "$COPY_WRAPPER_DIR/cp"
SFH_TEST_REAL_CP="$(command -v cp)"
PATH="$COPY_WRAPPER_DIR:$PATH" \
SFH_TEST_REAL_CP="$SFH_TEST_REAL_CP" \
SFH_TEST_CP_MARKER="$COPY_MARKER" \
SFH_TEST_CP_CLAIM="$COPY_CLAIM" \
SFH_ASSET_DIR="$ASSET_DIR" \
SFH_INSTALL_DIR="$CONCURRENT_INSTALL" \
SFH_DATA_DIR="$CONCURRENT_DATA" \
SFH_NO_MODIFY_PATH=1 \
  sh "$REPO_ROOT/installers/sfh-installer.sh" \
  >"$TEST_ROOT/concurrent-first.out" 2>"$TEST_ROOT/concurrent-first.err" &
FIRST_INSTALLER_PID=$!
COPY_WAIT_COUNT=0
while [ ! -e "$COPY_MARKER" ]; do
  if ! kill -0 "$FIRST_INSTALLER_PID" 2>/dev/null; then
    wait "$FIRST_INSTALLER_PID" || true
    cat "$TEST_ROOT/concurrent-first.err" >&2
    echo "first concurrent installer exited before holding both destination locks" >&2
    exit 1
  fi
  COPY_WAIT_COUNT=$((COPY_WAIT_COUNT + 1))
  if [ "$COPY_WAIT_COUNT" -gt 500 ]; then
    kill "$FIRST_INSTALLER_PID" 2>/dev/null || true
    wait "$FIRST_INSTALLER_PID" || true
    echo "timed out waiting for the first concurrent installer" >&2
    exit 1
  fi
  sleep 0.01
done
if PATH="$COPY_WRAPPER_DIR:$PATH" \
   SFH_TEST_REAL_CP="$SFH_TEST_REAL_CP" \
   SFH_TEST_CP_MARKER="$COPY_MARKER" \
   SFH_TEST_CP_CLAIM="$COPY_CLAIM" \
   SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$CONCURRENT_ROOT/other-install" \
   SFH_DATA_DIR="$CONCURRENT_INSTALL" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" \
     >"$TEST_ROOT/concurrent-second.out" 2>"$TEST_ROOT/concurrent-second.err"; then
  echo "two installers concurrently mutated the same destinations" >&2
  exit 1
fi
grep -F "another sfh installer is active" \
  "$TEST_ROOT/concurrent-second.err" >/dev/null
if ! wait "$FIRST_INSTALLER_PID"; then
  cat "$TEST_ROOT/concurrent-first.err" >&2
  echo "the lock-owning installer did not complete" >&2
  exit 1
fi
verify_resources "$CONCURRENT_DATA"
[ "$("$CONCURRENT_INSTALL/sfh" --version)" = "$EXPECTED_VERSION" ]
[ -f "$CONCURRENT_DATA/.sfh-installer-inventory" ]
if find "$CONCURRENT_ROOT" \
  \( -name '.sfh-installer-lock-*' -o -name '.sfh-data.*' -o -name '.sfh.new.*' \) \
  -print -quit | grep . >/dev/null; then
  echo "concurrent install left a lock or transaction artifact" >&2
  exit 1
fi

PROFILE_HOME="$TEST_ROOT/home"
mkdir "$PROFILE_HOME"
HOME="$PROFILE_HOME" \
SHELL=/bin/bash \
XDG_DATA_HOME="$PROFILE_HOME/.local/share" \
SFH_ASSET_DIR="$ASSET_DIR" \
  sh "$REPO_ROOT/installers/sfh-installer.sh"
HOME="$PROFILE_HOME" \
SHELL=/bin/bash \
XDG_DATA_HOME="$PROFILE_HOME/.local/share" \
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
verify_resources "$PROFILE_HOME/.local/share/sfh"

OVERSIZED_ASSET_DIR="$TEST_ROOT/oversized-assets"
mkdir "$OVERSIZED_ASSET_DIR"
dd if=/dev/zero of="$OVERSIZED_ASSET_DIR/$ASSET" bs=1048576 count=51 2>/dev/null
printf '%s  %s\n' "$(file_sha256 "$OVERSIZED_ASSET_DIR/$ASSET")" "$ASSET" \
  >"$OVERSIZED_ASSET_DIR/$ASSET.sha256"
if SFH_ASSET_DIR="$OVERSIZED_ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/oversized-install" \
   SFH_DATA_DIR="$TEST_ROOT/oversized-data" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" \
     2>"$TEST_ROOT/oversized-error.log"; then
  echo "installer accepted an archive over the compressed-size limit" >&2
  exit 1
fi
grep -F "exceeds the 50 MiB download-size limit" "$TEST_ROOT/oversized-error.log" >/dev/null
[ ! -e "$TEST_ROOT/oversized-install/sfh" ]
[ ! -e "$TEST_ROOT/oversized-data" ]

MANY_PACKAGE_DIR="$TEST_ROOT/many-package"
MANY_ASSET_DIR="$TEST_ROOT/many-assets"
cp -R "$PACKAGE_DIR" "$MANY_PACKAGE_DIR"
mkdir -p "$MANY_PACKAGE_DIR/tests/archive-member-limit"
many_index=1
while [ "$many_index" -le 2001 ]; do
  : >"$MANY_PACKAGE_DIR/tests/archive-member-limit/member-$many_index"
  many_index=$((many_index + 1))
done
mkdir "$MANY_ASSET_DIR"
tar czf "$MANY_ASSET_DIR/$ASSET" \
  -C "$MANY_PACKAGE_DIR" \
  sfh release-resources.txt \
  AGENTS.md CHANGELOG.md CONTRIBUTING.md LICENSE README.ja.md README.md SECURITY.md \
  SUPPORT.md docs examples schema skills tests
printf '%s  %s\n' "$(file_sha256 "$MANY_ASSET_DIR/$ASSET")" "$ASSET" \
  >"$MANY_ASSET_DIR/$ASSET.sha256"
if SFH_ASSET_DIR="$MANY_ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/many-install" \
   SFH_DATA_DIR="$TEST_ROOT/many-data" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" \
     2>"$TEST_ROOT/many-error.log"; then
  echo "installer accepted an archive over the member-count limit" >&2
  exit 1
fi
grep -F "exceeds the 2000-member limit" "$TEST_ROOT/many-error.log" >/dev/null
[ ! -e "$TEST_ROOT/many-install/sfh" ]
[ ! -e "$TEST_ROOT/many-data" ]

MISSING_PACKAGE_DIR="$TEST_ROOT/missing-package"
MISSING_ASSET_DIR="$TEST_ROOT/missing-assets"
mkdir "$MISSING_ASSET_DIR"
cp -R "$PACKAGE_DIR" "$MISSING_PACKAGE_DIR"
rm -rf "$MISSING_PACKAGE_DIR/schema"
  tar czf "$MISSING_ASSET_DIR/$ASSET" \
  -C "$MISSING_PACKAGE_DIR" \
  sfh release-resources.txt \
  AGENTS.md CHANGELOG.md CONTRIBUTING.md LICENSE README.ja.md README.md SECURITY.md \
  SUPPORT.md docs examples skills tests
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$MISSING_ASSET_DIR" && sha256sum "$ASSET" >"$ASSET.sha256")
else
  (cd "$MISSING_ASSET_DIR" && shasum -a 256 "$ASSET" >"$ASSET.sha256")
fi
if SFH_ASSET_DIR="$MISSING_ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/missing-install" \
   SFH_DATA_DIR="$TEST_ROOT/missing-data" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/missing-error.log"; then
  echo "installer accepted an archive with a missing resource directory" >&2
  exit 1
fi
grep -F "required resource directory: schema/" "$TEST_ROOT/missing-error.log" >/dev/null
[ ! -e "$TEST_ROOT/missing-install/sfh" ]
[ ! -e "$TEST_ROOT/missing-data" ]

UNOWNED_DATA_DIR="$TEST_ROOT/unowned-data"
mkdir "$UNOWNED_DATA_DIR"
cp "$PACKAGE_DIR/release-resources.txt" "$UNOWNED_DATA_DIR/"
printf 'keep\n' >"$UNOWNED_DATA_DIR/personal.txt"
if SFH_ASSET_DIR="$ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/unowned-install" \
   SFH_DATA_DIR="$UNOWNED_DATA_DIR" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/unowned-error.log"; then
  echo "installer replaced an unowned resource directory" >&2
  exit 1
fi
grep -F "not owned by the sfh installer" "$TEST_ROOT/unowned-error.log" >/dev/null
grep -F "keep" "$UNOWNED_DATA_DIR/personal.txt" >/dev/null
[ ! -e "$TEST_ROOT/unowned-install/sfh" ]

BAD_ASSET_DIR="$TEST_ROOT/bad-assets"
mkdir "$BAD_ASSET_DIR"
cp "$ASSET_DIR/$ASSET" "$ASSET_DIR/$ASSET.sha256" "$BAD_ASSET_DIR/"
printf 'corrupt' >>"$BAD_ASSET_DIR/$ASSET"

if SFH_ASSET_DIR="$BAD_ASSET_DIR" \
   SFH_INSTALL_DIR="$TEST_ROOT/rejected" \
   SFH_DATA_DIR="$TEST_ROOT/rejected-data" \
   SFH_NO_MODIFY_PATH=1 \
     sh "$REPO_ROOT/installers/sfh-installer.sh" 2>"$TEST_ROOT/error.log"; then
  echo "installer accepted a corrupted archive" >&2
  exit 1
fi
grep -F "SHA-256 mismatch" "$TEST_ROOT/error.log" >/dev/null
[ ! -e "$TEST_ROOT/rejected/sfh" ]
[ ! -e "$TEST_ROOT/rejected-data" ]

echo "Unix installer checks passed ($EXPECTED_VERSION, $ASSET)"
