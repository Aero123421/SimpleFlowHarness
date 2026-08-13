#!/bin/sh

set -eu

REPOSITORY="Aero123421/SimpleFlowHarness"
EXPECTED_APPLE_TEAM_ID='{{APPLE_TEAM_ID}}'
INSTALLER_VERSION='{{VERSION}}'
EXPECTED_LINUX_ARM64_SHA256='{{LINUX_ARM64_SHA256}}'
EXPECTED_LINUX_X64_SHA256='{{LINUX_X64_SHA256}}'
EXPECTED_MACOS_ARM64_SHA256='{{MACOS_ARM64_SHA256}}'
EXPECTED_MACOS_X64_SHA256='{{MACOS_X64_SHA256}}'
DEFAULT_INSTALL_DIR="${HOME:?HOME is required}/.local/bin"
DEFAULT_DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/sfh"
REQUESTED_VERSION="${SFH_VERSION:-latest}"
INSTALL_DIR="${SFH_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
DATA_DIR="${SFH_DATA_DIR:-$DEFAULT_DATA_DIR}"
ASSET_DIR="${SFH_ASSET_DIR:-}"
BASE_URL_OVERRIDE="${SFH_BASE_URL:-}"
if [ -n "${SFH_STATE_DIR:-}" ]; then
  STATE_DIR="$SFH_STATE_DIR"
elif [ -n "${XDG_STATE_HOME:-}" ] && [ "${XDG_STATE_HOME#/}" != "$XDG_STATE_HOME" ]; then
  STATE_DIR="$XDG_STATE_HOME/sfh"
elif [ "${HOME#/}" != "$HOME" ]; then
  STATE_DIR="$HOME/.local/state/sfh"
else
  STATE_DIR=""
fi

EXPECTED_RESOURCE_PATHS='AGENTS.md
CHANGELOG.md
CONTRIBUTING.md
LICENSE
README.ja.md
README.md
SECURITY.md
SUPPORT.md
docs/
examples/
schema/
skills/
tests/'

OWNERSHIP_MARKER_NAME='.sfh-installer-owned'
OWNERSHIP_MARKER_CONTENT='sfh installer resource directory v1'
INVENTORY_NAME='.sfh-installer-inventory'

say() {
  printf '%s\n' "$*"
}

fail() {
  printf 'sfh installer: %s\n' "$*" >&2
  exit 1
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

normalize_version() {
  if [ -n "$ASSET_DIR" ]; then
    case "$REQUESTED_VERSION" in
      latest) printf '%s\n' "latest" ;;
      *)
        if ! printf '%s\n' "$REQUESTED_VERSION" |
          grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$'; then
          fail "invalid SFH_VERSION '$REQUESTED_VERSION' (expected latest, 1.2.3, or v1.2.3)"
        fi
        case "$REQUESTED_VERSION" in
          v*) printf '%s\n' "$REQUESTED_VERSION" ;;
          *) printf 'v%s\n' "$REQUESTED_VERSION" ;;
        esac
        ;;
    esac
    return
  fi

  if ! printf '%s\n' "$INSTALLER_VERSION" |
    grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$'; then
    fail "official installer version is not configured"
  fi
  case "$REQUESTED_VERSION" in
    latest) printf 'v%s\n' "$INSTALLER_VERSION" ;;
    *)
      if ! printf '%s\n' "$REQUESTED_VERSION" |
        grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+([-.][0-9A-Za-z.-]+)?$'; then
        fail "invalid SFH_VERSION '$REQUESTED_VERSION' (expected latest, 1.2.3, or v1.2.3)"
      fi
      requested_without_v="${REQUESTED_VERSION#v}"
      [ "$requested_without_v" = "$INSTALLER_VERSION" ] ||
        fail "this installer is bound to sfh $INSTALLER_VERSION"
      printf 'v%s\n' "$INSTALLER_VERSION"
      ;;
  esac
}

select_asset() {
  os="$(uname -s 2>/dev/null || true)"
  arch="$(uname -m 2>/dev/null || true)"

  case "$arch" in
    x86_64 | amd64) suffix="x64" ;;
    arm64 | aarch64) suffix="arm64" ;;
    *) fail "unsupported CPU architecture '$arch'" ;;
  esac

  case "$os" in
    Linux) printf 'sfh-linux-%s.tar.gz\n' "$suffix" ;;
    Darwin) printf 'sfh-macos-%s.tar.gz\n' "$suffix" ;;
    *) fail "unsupported operating system '$os'; use sfh-installer.ps1 on Windows" ;;
  esac
}

embedded_sha256_for_asset() {
  case "$1" in
    sfh-linux-arm64.tar.gz) printf '%s\n' "$EXPECTED_LINUX_ARM64_SHA256" ;;
    sfh-linux-x64.tar.gz) printf '%s\n' "$EXPECTED_LINUX_X64_SHA256" ;;
    sfh-macos-arm64.tar.gz) printf '%s\n' "$EXPECTED_MACOS_ARM64_SHA256" ;;
    sfh-macos-x64.tar.gz) printf '%s\n' "$EXPECTED_MACOS_X64_SHA256" ;;
    *) fail "no embedded SHA-256 is configured for $1" ;;
  esac
}

fetch_asset() {
  name="$1"
  destination="$2"
  case "$name" in
    *.sha256) max_bytes=4096 ;;
    *) max_bytes=52428800 ;;
  esac

  if [ -n "$ASSET_DIR" ]; then
    [ -d "$ASSET_DIR" ] || fail "SFH_ASSET_DIR is not a directory: $ASSET_DIR"
    [ -f "$ASSET_DIR/$name" ] || fail "asset not found in SFH_ASSET_DIR: $name"
    cp "$ASSET_DIR/$name" "$destination"
    return
  fi

  url="$BASE_URL/$name"
  fetch_fifo="$TMP_DIR/fetch-fifo"
  rm -f "$fetch_fifo"
  mkfifo "$fetch_fifo"
  if command_exists curl; then
    curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
      --retry 3 --max-filesize "$max_bytes" "$url" >"$fetch_fifo" &
  elif command_exists wget; then
    wget --https-only --quiet --tries=3 --output-document=- "$url" >"$fetch_fifo" &
  else
    rm -f "$fetch_fifo"
    fail "curl or wget is required"
  fi
  FETCH_PID=$!
  if ! dd if="$fetch_fifo" of="$destination" bs=4096 count=$((max_bytes / 4096)) 2>/dev/null; then
    kill "$FETCH_PID" 2>/dev/null || true
    wait "$FETCH_PID" 2>/dev/null || true
    FETCH_PID=""
    rm -f "$fetch_fifo"
    fail "could not download $name"
  fi
  if ! wait "$FETCH_PID"; then
    FETCH_PID=""
    rm -f "$fetch_fifo"
    fail "$name download failed or exceeded its size limit"
  fi
  FETCH_PID=""
  rm -f "$fetch_fifo"
  downloaded_size="$(wc -c <"$destination" | tr -d '[:space:]')"
  case "$downloaded_size" in *[!0-9]* | '') fail "could not measure $name" ;; esac
  [ "$downloaded_size" -le "$max_bytes" ] || fail "$name exceeds its download-size limit"
}

sha256_file() {
  file="$1"
  if command_exists sha256sum; then
    sha256sum "$file" | awk '{print $1}'
  elif command_exists shasum; then
    shasum -a 256 "$file" | awk '{print $1}'
  elif command_exists openssl; then
    openssl dgst -sha256 "$file" | sed 's/^.*= //'
  else
    fail "sha256sum, shasum, or openssl is required to verify the download"
  fi
}

acquire_install_lock() {
  [ ! -L "$HOME" ] && [ -d "$HOME" ] ||
    fail "HOME must be a regular private directory for the installer lock: $HOME"
  lock_path="$HOME/.sfh-installer.lock"
  if ! (umask 077 && mkdir "$lock_path") 2>/dev/null; then
    if [ -L "$lock_path" ] || [ ! -d "$lock_path" ]; then
      fail "global installer lock is not a private directory: $lock_path"
    fi
    owner_file="$lock_path/owner"
    if [ ! -L "$owner_file" ] && [ -f "$owner_file" ]; then
      owner_pid="$(sed -n '1p' "$owner_file")"
      owner_start="$(sed -n '2p' "$owner_file")"
      case "$owner_pid" in *[!0-9]* | '') owner_pid='' ;; esac
      if [ -n "$owner_pid" ]; then
        live_start="$(ps -p "$owner_pid" -o lstart= 2>/dev/null |
          sed -n '1s/^[[:space:]]*//p')"
        if [ -n "$live_start" ] && [ "$live_start" = "$owner_start" ]; then
          fail "another sfh installer is active (global lock: $lock_path)"
        fi
      fi
    fi
    fail "stale sfh installer lock requires inspection and manual removal: $lock_path"
  fi
  GLOBAL_LOCK_DIR="$lock_path"
  GLOBAL_LOCK_CREATED=1
  [ ! -L "$lock_path" ] && [ -d "$lock_path" ] ||
    fail "installer lock is not a private directory: $lock_path"
  chmod 0700 "$lock_path"
  GLOBAL_LOCK_START="$(ps -p "$$" -o lstart= 2>/dev/null |
    sed -n '1s/^[[:space:]]*//p' || true)"
  [ -n "$GLOBAL_LOCK_START" ] || GLOBAL_LOCK_START='unknown'
  GLOBAL_LOCK_NONCE="$$-$(date +%s)-$(awk 'BEGIN { srand(); print int(rand() * 1000000000) }')"
  {
    printf '%s\n' "$$"
    printf '%s\n' "$GLOBAL_LOCK_START"
    printf '%s\n' "$GLOBAL_LOCK_NONCE"
  } >"$lock_path/owner"
  chmod 0600 "$lock_path/owner"
}

release_install_lock() {
  lock_path="$GLOBAL_LOCK_DIR"
  [ -n "$lock_path" ] || return 0
  [ ! -L "$lock_path" ] && [ -d "$lock_path" ] || return 1
  owner_file="$lock_path/owner"
  if [ "$GLOBAL_LOCK_CREATED" -eq 1 ] && [ ! -e "$owner_file" ] && [ ! -L "$owner_file" ]; then
    rmdir "$lock_path"
    GLOBAL_LOCK_DIR=""
    GLOBAL_LOCK_CREATED=0
    return
  fi
  [ ! -L "$owner_file" ] && [ -f "$owner_file" ] || return 1
  [ "$(sed -n '3p' "$owner_file")" = "$GLOBAL_LOCK_NONCE" ] || return 1
  rm -f "$owner_file" || return 1
  rmdir "$lock_path"
  GLOBAL_LOCK_DIR=""
  GLOBAL_LOCK_CREATED=0
}

validate_archive_member() {
  member="$1"

  [ -n "$member" ] || fail "archive contains an empty member name"
  case "$member" in
    /* | \\* | *\\* | *:*)
      fail "archive contains an unsafe member path: $member"
      ;;
  esac
  member_without_trailing_slash="${member%/}"
  [ -n "$member_without_trailing_slash" ] ||
    fail "archive contains an unsafe member path: $member"
  case "/$member_without_trailing_slash/" in
    */../* | */./* | *//*)
      fail "archive contains an unsafe member path: $member"
      ;;
  esac

  case "$member" in
    sfh | release-resources.txt | AGENTS.md | CHANGELOG.md | CONTRIBUTING.md | LICENSE | \
      README.ja.md | README.md | SECURITY.md | SUPPORT.md)
      ;;
    docs | docs/* | examples | examples/* | schema | schema/* | skills | skills/* | \
      tests | tests/*)
      ;;
    *) fail "archive contains an unexpected member: $member" ;;
  esac
}

validate_resource_manifest() {
  manifest="$1"
  expected="$2"

  [ -f "$manifest" ] || fail "archive does not contain release-resources.txt"
  cmp -s "$manifest" "$expected" ||
    fail "release-resources.txt does not match the required resource set"
}

validate_extracted_resources() {
  root="$1"

  unsafe_link="$({
    find "$root" \( -type l -o \( -type f -links +1 \) \) -print -quit
  } 2>/dev/null || true)"
  [ -z "$unsafe_link" ] || fail "archive contains a symbolic or hard link: $unsafe_link"

  for file in \
    AGENTS.md CHANGELOG.md CONTRIBUTING.md LICENSE README.ja.md README.md \
    SECURITY.md SUPPORT.md; do
    [ -f "$root/$file" ] || fail "archive does not contain required resource: $file"
  done
  for directory in docs examples schema skills tests; do
    [ -d "$root/$directory" ] ||
      fail "archive does not contain required resource directory: $directory/"
  done
}

decompress_archive_with_limit() {
  archive="$1"
  raw_archive="$2"

  command_exists gzip || fail "gzip is required"
  (
    # File-size ulimit units differ across otherwise supported shells. This
    # bounds decompression to 320-640 MiB; the exact raw-tar and extracted
    # content limits below remain platform-independent.
    ulimit -f 655360
    exec gzip -dc "$archive"
  ) >"$raw_archive" || fail "archive decompression failed or exceeded its size limit"

  raw_size="$(wc -c <"$raw_archive" | tr -d '[:space:]')"
  case "$raw_size" in *[!0-9]* | '') fail "could not measure the decompressed archive" ;; esac
  [ "$raw_size" -le 335544320 ] ||
    fail "archive exceeds the decompressed-tar size limit"
}

extract_archive_with_limits() {
  raw_archive="$1"
  destination="$2"
  extracted_files="$TMP_DIR/extracted-files.txt"

  if ! (
    # This gives a 32-64 MiB protective ceiling depending on shell units;
    # exact unnamed-stream file sizes are checked immediately afterwards.
    ulimit -f 65536
    exec tar xf "$raw_archive" -C "$destination"
  ); then
    fail "archive extraction failed or exceeded the member-size limit"
  fi

  find "$destination" -type f -print >"$extracted_files"
  extracted_total=0
  while IFS= read -r extracted_file || [ -n "$extracted_file" ]; do
    extracted_size="$(wc -c <"$extracted_file" | tr -d '[:space:]')"
    case "$extracted_size" in *[!0-9]* | '') fail "could not measure extracted archive member" ;; esac
    [ "$extracted_size" -le 33554432 ] ||
      fail "archive member exceeds the 32 MiB size limit: $extracted_file"
    extracted_total=$((extracted_total + extracted_size))
    [ "$extracted_total" -le 268435456 ] ||
      fail "archive exceeds the 256 MiB extracted-size limit"
  done <"$extracted_files"
}

validate_activated_resources() {
  root="$1"
  expected_inventory="$2"
  expected_manifest="$3"

  [ ! -L "$root/$OWNERSHIP_MARKER_NAME" ] &&
    [ -f "$root/$OWNERSHIP_MARKER_NAME" ] &&
    cmp -s "$root/$OWNERSHIP_MARKER_NAME" "$EXPECTED_OWNERSHIP_MARKER" ||
    fail "activated resource ownership marker changed before binary installation"
  [ ! -L "$root/$INVENTORY_NAME" ] && [ -f "$root/$INVENTORY_NAME" ] &&
    cmp -s "$root/$INVENTORY_NAME" "$expected_inventory" ||
    fail "activated resource inventory changed before binary installation"
  [ ! -L "$root/release-resources.txt" ] && [ -f "$root/release-resources.txt" ] &&
    cmp -s "$root/release-resources.txt" "$expected_manifest" ||
    fail "activated resource manifest changed before binary installation"
  validate_resource_inventory "$root"
}

stage_resources() {
  source_root="$1"
  staged_data="$2"

  mkdir "$staged_data"
  for file in \
    AGENTS.md CHANGELOG.md CONTRIBUTING.md LICENSE README.ja.md README.md \
    SECURITY.md SUPPORT.md release-resources.txt; do
    cp "$source_root/$file" "$staged_data/"
  done
  for directory in docs examples schema skills tests; do
    cp -R "$source_root/$directory" "$staged_data/"
  done
  cp "$EXPECTED_OWNERSHIP_MARKER" "$staged_data/$OWNERSHIP_MARKER_NAME"
}

write_resource_inventory() {
  root="$1"
  output="$2"
  unsorted="$output.unsorted"
  sorted="$output.sorted"
  tab="$(printf '\t')"

  unsafe_path="$(find "$root" -mindepth 1 \
    \( -type l -o \( -type f -links +1 \) -o \( ! -type f ! -type d \) \) \
    -print -quit 2>/dev/null || true)"
  [ -z "$unsafe_path" ] ||
    fail "resource tree contains a link or special file: $unsafe_path"

  : >"$unsorted"
  if ! LC_ALL=C find "$root" -mindepth 1 -exec sh -c '
    root="$1"
    output="$2"
    marker="$3"
    inventory="$4"
    shift 4
    newline="
"
    carriage_return="$(printf "\r")"
    tab="$(printf "\t")"
    for path do
      relative=${path#"$root"/}
      case "$relative" in
        "$marker" | "$inventory") continue ;;
        *"$newline"* | *"$carriage_return"* | *"$tab"*) exit 71 ;;
      esac
      if ! printf "%s\n" "$relative" | LC_ALL=C grep -Eq "^[ -~]+$"; then
        exit 71
      fi
      if [ -L "$path" ]; then
        exit 72
      elif [ -d "$path" ]; then
        printf "d\t%s\n" "$relative" >>"$output"
      elif [ -f "$path" ]; then
        printf "f\t%s\n" "$relative" >>"$output"
      else
        exit 72
      fi
    done
  ' sfh-inventory "$root" "$unsorted" "$OWNERSHIP_MARKER_NAME" "$INVENTORY_NAME" {} +; then
    rm -f "$unsorted"
    fail "resource tree contains a non-ASCII path, link, or special file"
  fi
  LC_ALL=C sort -t "$tab" -k2,2 "$unsorted" >"$sorted"
  : >"$output"
  while IFS="$tab" read -r kind relative || [ -n "$kind$relative" ]; do
    case "$kind" in
      d) printf 'd - %s/\n' "$relative" >>"$output" ;;
      f)
        digest="$(sha256_file "$root/$relative" | tr 'A-F' 'a-f')"
        printf 'f %s %s\n' "$digest" "$relative" >>"$output"
        ;;
      *)
        rm -f "$unsorted" "$sorted" "$output"
        fail "could not build the resource inventory"
        ;;
    esac
  done <"$sorted"
  rm -f "$unsorted" "$sorted"
}

validate_resource_inventory() {
  root="$1"
  inventory="$root/$INVENTORY_NAME"
  actual="$TMP_DIR/installed-resource-inventory.txt"

  [ ! -L "$inventory" ] && [ -f "$inventory" ] ||
    fail "resource destination has no valid installer inventory: $DATA_DIR"
  write_resource_inventory "$root" "$actual"
  cmp -s "$inventory" "$actual" ||
    fail "resource destination does not match its installer inventory: $DATA_DIR"
}

delete_inventoried_resource_tree() {
  root="$1"
  inventory="$root/$INVENTORY_NAME"
  trusted_inventory="$TMP_DIR/delete-resource-inventory.txt"
  directories="$TMP_DIR/delete-resource-directories.txt"

  [ ! -L "$inventory" ] && [ -f "$inventory" ] || return 1
  write_resource_inventory "$root" "$trusted_inventory"
  cmp -s "$inventory" "$trusted_inventory" || return 1
  : >"$directories"
  while IFS=' ' read -r kind digest relative || [ -n "$kind$digest$relative" ]; do
    case "$kind" in
      f)
        path="$root/$relative"
        [ ! -L "$path" ] && [ -f "$path" ] || return 1
        actual="$(sha256_file "$path" | tr 'A-F' 'a-f')"
        [ "$actual" = "$digest" ] || return 1
        rm -f "$path" || return 1
        [ ! -e "$path" ] && [ ! -L "$path" ] || return 1
        ;;
      d)
        [ "$digest" = "-" ] || return 1
        relative="${relative%/}"
        printf '%s\n' "$relative" >>"$directories"
        ;;
      *) return 1 ;;
    esac
  done <"$trusted_inventory"

  LC_ALL=C sort -r "$directories" | while IFS= read -r relative; do
    path="$root/$relative"
    [ ! -L "$path" ] && [ -d "$path" ] || exit 1
    rmdir "$path" || exit 1
  done || return 1

  for private_name in "$OWNERSHIP_MARKER_NAME" "$INVENTORY_NAME"; do
    path="$root/$private_name"
    [ ! -L "$path" ] && [ -f "$path" ] || return 1
    rm -f "$path" || return 1
  done
  rmdir "$root"
}

canonical_path() {
  target="$1"
  case "$target" in
    *'
'*) fail "install paths must not contain newlines" ;;
  esac
  case "$target" in
    /*) ;;
    *) target="$(pwd)/$target" ;;
  esac

  tail=""
  probe="$target"
  while [ ! -d "$probe" ]; do
    base="$(basename -- "$probe")"
    tail="/$base$tail"
    parent="$(dirname -- "$probe")"
    [ "$parent" != "$probe" ] || break
    probe="$parent"
  done
  probe="$(CDPATH='' cd -P -- "$probe" && pwd)"
  printf '%s%s\n' "$probe" "$tail" | LC_ALL=C awk -F/ '
    {
      count = 0
      for (i = 1; i <= NF; i++) {
        if ($i == "" || $i == ".") {
          continue
        }
        if ($i == "..") {
          if (count > 0) {
            count--
          }
        } else {
          components[++count] = $i
        }
      }
      output = "/"
      for (i = 1; i <= count; i++) {
        output = output (i == 1 ? "" : "/") components[i]
      }
      print output
    }
  '
}

paths_overlap() {
  left="${1%/}/"
  right="${2%/}/"
  if [ "$(uname -s 2>/dev/null || true)" = "Darwin" ]; then
    left="$(printf '%s' "$left" | tr '[:upper:]' '[:lower:]')"
    right="$(printf '%s' "$right" | tr '[:upper:]' '[:lower:]')"
  fi
  case "$left" in "$right"*) return 0 ;; esac
  case "$right" in "$left"*) return 0 ;; esac
  return 1
}

install_staged_resources() {
  staged_data="$1"
  expected_manifest="$2"

  data_parent="$(dirname -- "$DATA_DIR")"
  mkdir -p "$data_parent"
  data_transaction="$(mktemp -d "$data_parent/.sfh-data.XXXXXX")"
  DATA_TRANSACTION="$data_transaction"
  transaction_data="$data_transaction/new"
  transaction_previous="$data_transaction/previous"
  mv "$staged_data" "$transaction_data"

  if [ -L "$DATA_DIR" ]; then
    fail "resource destination must not be a symbolic link: $DATA_DIR"
  fi
  if [ -e "$DATA_DIR" ]; then
    [ -d "$DATA_DIR" ] || fail "resource destination is not a directory: $DATA_DIR"
    if [ -L "$DATA_DIR/release-resources.txt" ] ||
      [ ! -f "$DATA_DIR/release-resources.txt" ] ||
      ! cmp -s "$DATA_DIR/release-resources.txt" "$expected_manifest" ||
      [ -L "$DATA_DIR/$OWNERSHIP_MARKER_NAME" ] ||
      [ ! -f "$DATA_DIR/$OWNERSHIP_MARKER_NAME" ] ||
      ! cmp -s "$DATA_DIR/$OWNERSHIP_MARKER_NAME" "$EXPECTED_OWNERSHIP_MARKER"; then
      fail "refusing to replace a resource directory not owned by the sfh installer: $DATA_DIR"
    fi
    validate_resource_inventory "$DATA_DIR"
    trap '' HUP INT TERM
    if ! mv "$DATA_DIR" "$transaction_previous"; then
      trap 'exit 129' HUP
      trap 'exit 130' INT
      trap 'exit 143' TERM
      fail "could not stage the previous resources from $DATA_DIR"
    fi
    DATA_HAD_PREVIOUS=1
    if ! (trap - EXIT; validate_resource_inventory "$transaction_previous"); then
      if mv "$transaction_previous" "$DATA_DIR"; then
        DATA_HAD_PREVIOUS=0
      else
        DATA_INSTALLED=1
      fi
      trap 'exit 129' HUP
      trap 'exit 130' INT
      trap 'exit 143' TERM
      fail "resource destination changed while it was being staged: $DATA_DIR"
    fi
  else
    trap '' HUP INT TERM
  fi

  if ! mv "$transaction_data" "$DATA_DIR"; then
    if [ "$DATA_HAD_PREVIOUS" -eq 1 ] && [ ! -e "$DATA_DIR" ]; then
      if mv "$transaction_previous" "$DATA_DIR"; then
        DATA_HAD_PREVIOUS=0
      else
        DATA_INSTALLED=1
      fi
    elif [ -e "$DATA_DIR" ] || [ -L "$DATA_DIR" ]; then
      # Treat a destination created during activation as untrusted activated
      # data so rollback preserves it before restoring the previous tree.
      DATA_INSTALLED=1
    fi
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    fail "could not install resources to $DATA_DIR"
  fi
  DATA_INSTALLED=1
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
}

rollback_resources() {
  [ "$DATA_INSTALLED" -eq 1 ] || return

  if [ -e "$DATA_DIR" ] || [ -L "$DATA_DIR" ]; then
    if ! (trap - EXIT; delete_inventoried_resource_tree "$DATA_DIR"); then
      recovery="$DATA_TRANSACTION/activated-recovery"
      [ ! -e "$recovery" ] || return 1
      mv "$DATA_DIR" "$recovery" || return 1
      DATA_RECOVERY_PATH="$recovery"
    fi
  fi
  if [ "$DATA_HAD_PREVIOUS" -eq 1 ]; then
    mv "$DATA_TRANSACTION/previous" "$DATA_DIR" || return 1
  fi
  DATA_INSTALLED=0
  DATA_HAD_PREVIOUS=0
}

commit_resources() {
  DATA_COMMITTED=1
  if [ "$DATA_HAD_PREVIOUS" -eq 1 ]; then
    if ! (trap - EXIT; delete_inventoried_resource_tree "$DATA_TRANSACTION/previous"); then
      DATA_RECOVERY_PATH="$DATA_TRANSACTION/previous"
    fi
  fi
  if [ -z "$DATA_RECOVERY_PATH" ]; then
    if rmdir "$DATA_TRANSACTION"; then
      DATA_TRANSACTION=""
    else
      DATA_RECOVERY_PATH="$DATA_TRANSACTION"
    fi
  fi
  if [ -n "$DATA_RECOVERY_PATH" ]; then
    printf 'sfh installer: preserved unexpected resource data at %s\n' \
      "$DATA_RECOVERY_PATH" >&2
  fi
  DATA_INSTALLED=0
  DATA_HAD_PREVIOUS=0
}

maybe_add_to_path() {
  case ":${PATH:-}:" in
    *":$INSTALL_DIR:"*) return ;;
  esac

  export PATH="$INSTALL_DIR:${PATH:-}"

  case "${SFH_NO_MODIFY_PATH:-}" in
    1 | true | TRUE | yes | YES)
      say "Installed directory is not persisted on PATH because SFH_NO_MODIFY_PATH is set."
      return
      ;;
  esac

  if [ "$INSTALL_DIR" != "$DEFAULT_INSTALL_DIR" ]; then
    say "Add this directory to PATH for future shells: $INSTALL_DIR"
    return
  fi

  shell_name="$(basename "${SHELL:-sh}")"
  case "$shell_name" in
    zsh) profile="$HOME/.zshrc" ;;
    bash) profile="$HOME/.bashrc" ;;
    fish)
      if command_exists fish; then
        # fish, rather than this POSIX shell, must expand HOME.
        # shellcheck disable=SC2016
        fish -c 'fish_add_path --path "$HOME/.local/bin"'
        say "Added $DEFAULT_INSTALL_DIR to the fish user PATH."
      else
        say "Add this directory to PATH for future shells: $DEFAULT_INSTALL_DIR"
      fi
      return
      ;;
    *) profile="$HOME/.profile" ;;
  esac

  # Keep both variables dynamic for future shells.
  # shellcheck disable=SC2016
  path_line='export PATH="$HOME/.local/bin:$PATH"'
  if [ ! -f "$profile" ] || ! grep -Fqx "$path_line" "$profile"; then
    {
      printf '\n# Added by the sfh installer\n'
      printf '%s\n' "$path_line"
    } >>"$profile"
    say "Added $DEFAULT_INSTALL_DIR to PATH in $profile."
  fi
}

VERSION="$(normalize_version)"
ASSET="$(select_asset)"

if [ -n "$BASE_URL_OVERRIDE" ]; then
  BASE_URL="${BASE_URL_OVERRIDE%/}"
else
  BASE_URL="https://github.com/$REPOSITORY/releases/download/$VERSION"
fi

command_exists tar || fail "tar is required"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sfh-install.XXXXXX")"
STAGED_PATH=""
FETCH_PID=""
DATA_TRANSACTION=""
DATA_INSTALLED=0
DATA_HAD_PREVIOUS=0
DATA_COMMITTED=0
DATA_RECOVERY_PATH=""
GLOBAL_LOCK_DIR=""
GLOBAL_LOCK_NONCE=""
GLOBAL_LOCK_CREATED=0
cleanup() {
  cleanup_status="$?"
  preserve_transaction=0
  trap - EXIT
  trap '' HUP INT TERM
  if [ -n "$FETCH_PID" ]; then
    kill "$FETCH_PID" 2>/dev/null || true
    wait "$FETCH_PID" 2>/dev/null || true
    FETCH_PID=""
  fi
  if [ -n "$DATA_TRANSACTION" ]; then
    if [ "$DATA_COMMITTED" -eq 0 ]; then
      if ! rollback_resources; then
        cleanup_status=1
        preserve_transaction=1
        printf 'sfh installer: could not restore previous resources; recovery data remains at %s\n' \
          "$DATA_TRANSACTION" >&2
      fi
    fi
    transaction_new="$DATA_TRANSACTION/new"
    if [ "$preserve_transaction" -eq 0 ] && [ -z "$DATA_RECOVERY_PATH" ] &&
      { [ -e "$transaction_new" ] || [ -L "$transaction_new" ]; }; then
      if ! (trap - EXIT; delete_inventoried_resource_tree "$transaction_new"); then
        preserve_transaction=1
        DATA_RECOVERY_PATH="$transaction_new"
      fi
    fi
    if [ "$preserve_transaction" -eq 0 ] && [ -z "$DATA_RECOVERY_PATH" ]; then
      if rmdir "$DATA_TRANSACTION"; then
        DATA_TRANSACTION=""
      else
        cleanup_status=1
        preserve_transaction=1
        DATA_RECOVERY_PATH="$DATA_TRANSACTION"
      fi
    fi
    if [ "$DATA_COMMITTED" -eq 0 ] && [ -n "$DATA_RECOVERY_PATH" ]; then
      printf 'sfh installer: preserved unexpected resource data at %s\n' \
        "$DATA_RECOVERY_PATH" >&2
    fi
  fi
  if [ -n "$STAGED_PATH" ]; then
    rm -f "$STAGED_PATH" || cleanup_status=1
  fi
  rm -rf "$TMP_DIR" || cleanup_status=1
  release_install_lock || cleanup_status=1
  exit "$cleanup_status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

ARCHIVE="$TMP_DIR/$ASSET"
CHECKSUM="$TMP_DIR/$ASSET.sha256"
EXTRACT_DIR="$TMP_DIR/extract"
EXPECTED_MANIFEST="$TMP_DIR/expected-release-resources.txt"
EXPECTED_OWNERSHIP_MARKER="$TMP_DIR/expected-ownership-marker.txt"
ARCHIVE_MEMBERS="$TMP_DIR/archive-members.txt"
ARCHIVE_LISTING="$TMP_DIR/archive-listing.txt"
RAW_ARCHIVE="$TMP_DIR/archive.tar"

printf '%s\n' "$EXPECTED_RESOURCE_PATHS" >"$EXPECTED_MANIFEST"
printf '%s\n' "$OWNERSHIP_MARKER_CONTENT" >"$EXPECTED_OWNERSHIP_MARKER"

CANONICAL_INSTALL_DIR="$(canonical_path "$INSTALL_DIR")"
CANONICAL_DATA_DIR="$(canonical_path "$DATA_DIR")"
if paths_overlap "$CANONICAL_INSTALL_DIR" "$CANONICAL_DATA_DIR"; then
  fail "SFH_INSTALL_DIR and SFH_DATA_DIR must not overlap"
fi
if [ -n "$STATE_DIR" ]; then
  CANONICAL_STATE_DIR="$(canonical_path "$STATE_DIR")"
  if paths_overlap "$CANONICAL_DATA_DIR" "$CANONICAL_STATE_DIR"; then
    fail "SFH_DATA_DIR and the sfh state directory must not overlap"
  fi
fi

acquire_install_lock

say "Downloading $ASSET..."
fetch_asset "$ASSET" "$ARCHIVE"
fetch_asset "$ASSET.sha256" "$CHECKSUM"

ARCHIVE_SIZE="$(wc -c <"$ARCHIVE" | tr -d '[:space:]')"
case "$ARCHIVE_SIZE" in *[!0-9]* | '') fail "could not measure $ASSET" ;; esac
[ "$ARCHIVE_SIZE" -le 52428800 ] || fail "$ASSET exceeds the 50 MiB download-size limit"

EXPECTED="$(awk 'NR == 1 {print $1}' "$CHECKSUM" | tr 'A-F' 'a-f')"
if ! printf '%s\n' "$EXPECTED" | grep -Eq '^[0-9a-f]{64}$'; then
  fail "invalid SHA-256 file for $ASSET"
fi
ACTUAL="$(sha256_file "$ARCHIVE" | tr 'A-F' 'a-f')"
[ "$ACTUAL" = "$EXPECTED" ] ||
  fail "SHA-256 mismatch for $ASSET (expected $EXPECTED, got $ACTUAL)"
if [ -z "$ASSET_DIR" ]; then
  EMBEDDED_SHA256="$(embedded_sha256_for_asset "$ASSET")"
  if ! printf '%s\n' "$EMBEDDED_SHA256" | grep -Eq '^[0-9a-f]{64}$'; then
    fail "official archive SHA-256 is not configured in this installer"
  fi
  [ "$EXPECTED" = "$EMBEDDED_SHA256" ] ||
    fail "release sidecar SHA-256 does not match this installer"
  [ "$ACTUAL" = "$EMBEDDED_SHA256" ] ||
    fail "downloaded archive SHA-256 does not match this installer"
fi

mkdir -p "$EXTRACT_DIR"
decompress_archive_with_limit "$ARCHIVE" "$RAW_ARCHIVE"
tar tf "$RAW_ARCHIVE" >"$ARCHIVE_MEMBERS" || fail "could not list $ASSET"
ARCHIVE_MEMBER_COUNT="$(wc -l <"$ARCHIVE_MEMBERS" | tr -d '[:space:]')"
case "$ARCHIVE_MEMBER_COUNT" in *[!0-9]* | '') fail "could not count archive members" ;; esac
[ "$ARCHIVE_MEMBER_COUNT" -le 2000 ] || fail "archive exceeds the 2000-member limit"
while IFS= read -r member || [ -n "$member" ]; do
  validate_archive_member "$member"
done <"$ARCHIVE_MEMBERS"
DUPLICATE_MEMBER="$(LC_ALL=C awk '
  {
    normalized = $0
    sub(/\/$/, "", normalized)
    folded = tolower(normalized)
    if (seen[folded]++) {
      print normalized
      exit
    }
  }
' "$ARCHIVE_MEMBERS")"
[ -z "$DUPLICATE_MEMBER" ] || fail "archive contains a duplicate member: $DUPLICATE_MEMBER"

tar tvf "$RAW_ARCHIVE" >"$ARCHIVE_LISTING" || fail "could not inspect $ASSET"
while IFS= read -r listing || [ -n "$listing" ]; do
  case "$listing" in
    -* | d*) ;;
    *) fail "archive contains a link or special file" ;;
  esac
done <"$ARCHIVE_LISTING"

extract_archive_with_limits "$RAW_ARCHIVE" "$EXTRACT_DIR"
[ -f "$EXTRACT_DIR/sfh" ] || fail "archive does not contain sfh"
validate_resource_manifest "$EXTRACT_DIR/release-resources.txt" "$EXPECTED_MANIFEST"
validate_extracted_resources "$EXTRACT_DIR"

STAGED_DATA="$TMP_DIR/staged-resources"
STAGED_INVENTORY="$TMP_DIR/staged-resource-inventory.txt"
stage_resources "$EXTRACT_DIR" "$STAGED_DATA"
write_resource_inventory "$STAGED_DATA" "$STAGED_INVENTORY"
cp "$STAGED_INVENTORY" "$STAGED_DATA/$INVENTORY_NAME"

if [ -e "$INSTALL_DIR" ] || [ -L "$INSTALL_DIR" ]; then
  [ -d "$INSTALL_DIR" ] || fail "install destination is not a directory: $INSTALL_DIR"
  [ ! -L "$INSTALL_DIR" ] || fail "install destination must not be a symbolic link: $INSTALL_DIR"
else
  mkdir -p "$INSTALL_DIR"
fi
[ ! -d "$INSTALL_DIR/sfh" ] || fail "binary destination is a directory: $INSTALL_DIR/sfh"
[ ! -L "$INSTALL_DIR/sfh" ] || fail "binary destination must not be a symbolic link: $INSTALL_DIR/sfh"
STAGED_PATH="$(mktemp "$INSTALL_DIR/.sfh.new.XXXXXX")"
cp "$EXTRACT_DIR/sfh" "$STAGED_PATH"
chmod 0755 "$STAGED_PATH"
if [ -z "$ASSET_DIR" ]; then
  if [ "$(uname -s 2>/dev/null || true)" = "Darwin" ]; then
    if [ "$EXPECTED_APPLE_TEAM_ID" != "UNSIGNED" ]; then
      if ! printf '%s\n' "$EXPECTED_APPLE_TEAM_ID" |
        LC_ALL=C grep -Eq '^[A-Z0-9]{10}$'; then
        fail "official macOS signer identity is invalid in this installer"
      fi
      command_exists codesign || fail "codesign is required to verify the official macOS build"
      command_exists spctl || fail "spctl is required to assess the official macOS build"
      codesign --verify --strict "$STAGED_PATH" >/dev/null 2>&1 ||
        fail "official macOS build failed codesign verification"
      TEAM_ID="$(codesign --display --verbose=4 "$STAGED_PATH" 2>&1 |
        awk -F= '/^TeamIdentifier=/ { print $2; exit }')"
      [ "$TEAM_ID" = "$EXPECTED_APPLE_TEAM_ID" ] ||
        fail "official macOS build signer identity does not match this release channel"
      spctl --assess --type execute "$STAGED_PATH" >/dev/null 2>&1 ||
        fail "official macOS build failed Gatekeeper assessment"
    fi
  fi
  EMBEDDED_INVENTORY="$TMP_DIR/embedded-resource-inventory.txt"
  "$STAGED_PATH" __release-manifest >"$EMBEDDED_INVENTORY" 2>/dev/null ||
    fail "official build did not provide its embedded release manifest"
  cmp -s "$EMBEDDED_INVENTORY" "$STAGED_INVENTORY" ||
    fail "official build release manifest does not match the downloaded resources"
  DOWNLOADED_VERSION="$("$STAGED_PATH" --version 2>/dev/null)" ||
    fail "downloaded binary did not start"
  [ "$DOWNLOADED_VERSION" = "sfh $INSTALLER_VERSION" ] ||
    fail "downloaded binary version does not match installer version $INSTALLER_VERSION"
else
  "$STAGED_PATH" --version >/dev/null 2>&1 || fail "downloaded binary did not start"
fi

install_staged_resources "$STAGED_DATA" "$EXPECTED_MANIFEST"
validate_activated_resources "$DATA_DIR" "$STAGED_INVENTORY" "$EXPECTED_MANIFEST"
if [ "$DATA_HAD_PREVIOUS" -eq 1 ] &&
  ! (trap - EXIT; validate_resource_inventory "$DATA_TRANSACTION/previous"); then
  trap '' HUP INT TERM
  if ! rollback_resources; then
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    fail "resource destination changed before binary installation and could not be restored"
  fi
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  fail "resource destination changed before binary installation: $DATA_DIR"
fi
validate_activated_resources "$DATA_DIR" "$STAGED_INVENTORY" "$EXPECTED_MANIFEST"
trap '' HUP INT TERM
if mv -f "$STAGED_PATH" "$INSTALL_DIR/sfh"; then
  DATA_COMMITTED=1
else
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM
  rollback_resources
  fail "could not install the binary to $INSTALL_DIR/sfh"
fi
commit_resources
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

maybe_add_to_path

INSTALLED_VERSION="$("$INSTALL_DIR/sfh" --version 2>/dev/null)" ||
  fail "installed binary did not start"
say "Installed $INSTALLED_VERSION to $INSTALL_DIR/sfh"
say "Installed resources to $DATA_DIR"
