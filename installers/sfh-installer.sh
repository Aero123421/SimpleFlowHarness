#!/bin/sh

set -eu

REPOSITORY="Aero123421/SimpleFlowHarness"
DEFAULT_INSTALL_DIR="${HOME:?HOME is required}/.local/bin"
REQUESTED_VERSION="${SFH_VERSION:-latest}"
INSTALL_DIR="${SFH_INSTALL_DIR:-$DEFAULT_INSTALL_DIR}"
ASSET_DIR="${SFH_ASSET_DIR:-}"
BASE_URL_OVERRIDE="${SFH_BASE_URL:-}"

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
  case "$REQUESTED_VERSION" in
    latest)
      printf '%s\n' "latest"
      ;;
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

fetch_asset() {
  name="$1"
  destination="$2"

  if [ -n "$ASSET_DIR" ]; then
    [ -d "$ASSET_DIR" ] || fail "SFH_ASSET_DIR is not a directory: $ASSET_DIR"
    [ -f "$ASSET_DIR/$name" ] || fail "asset not found in SFH_ASSET_DIR: $name"
    cp "$ASSET_DIR/$name" "$destination"
    return
  fi

  url="$BASE_URL/$name"
  if command_exists curl; then
    curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
      --retry 3 --output "$destination" "$url"
  elif command_exists wget; then
    wget --https-only --quiet --tries=3 --output-document="$destination" "$url"
  else
    fail "curl or wget is required"
  fi
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
elif [ "$VERSION" = "latest" ]; then
  BASE_URL="https://github.com/$REPOSITORY/releases/latest/download"
else
  BASE_URL="https://github.com/$REPOSITORY/releases/download/$VERSION"
fi

command_exists tar || fail "tar is required"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/sfh-install.XXXXXX")"
STAGED_PATH="$INSTALL_DIR/.sfh.new.$$"
cleanup() {
  rm -rf "$TMP_DIR"
  rm -f "$STAGED_PATH"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

ARCHIVE="$TMP_DIR/$ASSET"
CHECKSUM="$TMP_DIR/$ASSET.sha256"
EXTRACT_DIR="$TMP_DIR/extract"

say "Downloading $ASSET..."
fetch_asset "$ASSET" "$ARCHIVE"
fetch_asset "$ASSET.sha256" "$CHECKSUM"

EXPECTED="$(awk 'NR == 1 {print $1}' "$CHECKSUM" | tr 'A-F' 'a-f')"
if ! printf '%s\n' "$EXPECTED" | grep -Eq '^[0-9a-f]{64}$'; then
  fail "invalid SHA-256 file for $ASSET"
fi
ACTUAL="$(sha256_file "$ARCHIVE" | tr 'A-F' 'a-f')"
[ "$ACTUAL" = "$EXPECTED" ] ||
  fail "SHA-256 mismatch for $ASSET (expected $EXPECTED, got $ACTUAL)"

mkdir -p "$EXTRACT_DIR"
tar xzf "$ARCHIVE" -C "$EXTRACT_DIR" sfh
[ -f "$EXTRACT_DIR/sfh" ] || fail "archive does not contain sfh"

mkdir -p "$INSTALL_DIR"
cp "$EXTRACT_DIR/sfh" "$STAGED_PATH"
chmod 0755 "$STAGED_PATH"
mv -f "$STAGED_PATH" "$INSTALL_DIR/sfh"

maybe_add_to_path

INSTALLED_VERSION="$("$INSTALL_DIR/sfh" --version 2>/dev/null)" ||
  fail "installed binary did not start"
say "Installed $INSTALLED_VERSION to $INSTALL_DIR/sfh"
