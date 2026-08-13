#!/usr/bin/env python3
"""Render version-bound distribution metadata and installers."""

from __future__ import annotations

import argparse
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
APPLE_TEAM_ID_RE = re.compile(r"^[A-Z0-9]{10}$")
PLACEHOLDER_RE = re.compile(r"\{\{[A-Z0-9_]+\}\}")

CHECKSUM_ASSETS = {
    "MACOS_ARM64_SHA256": "sfh-macos-arm64.tar.gz",
    "MACOS_X64_SHA256": "sfh-macos-x64.tar.gz",
    "LINUX_ARM64_SHA256": "sfh-linux-arm64.tar.gz",
    "LINUX_X64_SHA256": "sfh-linux-x64.tar.gz",
    "WINDOWS_X64_SHA256": "sfh-windows-x64.zip",
}


def read_checksum(directory: Path, asset: str) -> str:
    sidecar = directory / f"{asset}.sha256"
    try:
        fields = sidecar.read_text(encoding="ascii").split()
    except FileNotFoundError as error:
        raise SystemExit(f"missing checksum file: {sidecar}") from error

    if len(fields) < 2 or fields[1].lstrip("*") != asset:
        raise SystemExit(f"{sidecar} must contain '<sha256>  {asset}'")
    checksum = fields[0]
    if not SHA256_RE.fullmatch(checksum):
        raise SystemExit(f"invalid SHA-256 in {sidecar}: {checksum!r}")
    return checksum.lower()


def render(template: Path, destination: Path, values: dict[str, str]) -> None:
    content = template.read_text(encoding="utf-8")
    for key, value in values.items():
        content = content.replace(f"{{{{{key}}}}}", value)
    unresolved = sorted(set(PLACEHOLDER_RE.findall(content)))
    if unresolved:
        raise SystemExit(
            f"unresolved placeholders in {template}: {', '.join(unresolved)}"
        )
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(content, encoding="utf-8", newline="\n")
    print(destination)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Render Homebrew metadata for an sfh release."
    )
    parser.add_argument("--version", required=True, help="release version without v")
    parser.add_argument(
        "--checksums-dir",
        required=True,
        type=Path,
        help="directory containing release asset .sha256 files",
    )
    parser.add_argument(
        "--output-dir",
        required=True,
        type=Path,
        help="directory to receive the homebrew/ tree",
    )
    parser.add_argument(
        "--release-assets-dir",
        type=Path,
        help="also stage sfh.rb for GitHub Releases",
    )
    parser.add_argument(
        "--apple-team-id",
        default="",
        help="Apple Developer Program Team ID embedded in the shell installer",
    )
    parser.add_argument(
        "--windows-codesign-cert-sha256",
        default="",
        help="SHA-256 fingerprint embedded in the PowerShell installer",
    )
    args = parser.parse_args()

    if not VERSION_RE.fullmatch(args.version):
        raise SystemExit(f"invalid release version: {args.version!r}")
    if args.apple_team_id and not APPLE_TEAM_ID_RE.fullmatch(args.apple_team_id):
        raise SystemExit(f"invalid Apple Team ID: {args.apple_team_id!r}")
    if args.windows_codesign_cert_sha256 and not SHA256_RE.fullmatch(
        args.windows_codesign_cert_sha256
    ):
        raise SystemExit("invalid Windows code-signing certificate SHA-256")

    values = {
        "VERSION": args.version,
        "APPLE_TEAM_ID": args.apple_team_id or "UNSIGNED",
        "WINDOWS_CODESIGN_CERT_SHA256": (
            args.windows_codesign_cert_sha256.lower() or "UNSIGNED"
        ),
    }
    values.update(
        {
            placeholder: read_checksum(args.checksums_dir, asset)
            for placeholder, asset in CHECKSUM_ASSETS.items()
        }
    )

    formula = args.output_dir / "homebrew/Formula/sfh.rb"
    render(
        ROOT / "packaging/homebrew/sfh.rb.template",
        formula,
        values,
    )

    if args.release_assets_dir:
        args.release_assets_dir.mkdir(parents=True, exist_ok=True)
        render(
            ROOT / "installers/sfh-installer.sh",
            args.release_assets_dir / "sfh-installer.sh",
            values,
        )
        render(
            ROOT / "installers/sfh-installer.ps1",
            args.release_assets_dir / "sfh-installer.ps1",
            values,
        )
        (args.release_assets_dir / "sfh.rb").write_bytes(formula.read_bytes())


if __name__ == "__main__":
    main()
