#!/usr/bin/env python3
"""Render Homebrew and WinGet metadata from one release checksum set."""

from __future__ import annotations

import argparse
import re
import shutil
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-.][0-9A-Za-z.-]+)?$")
PLACEHOLDER_RE = re.compile(r"\{\{[A-Z0-9_]+\}\}")

CHECKSUM_ASSETS = {
    "WINDOWS_X64_SHA256": "sfh-windows-x64.zip",
    "MACOS_ARM64_SHA256": "sfh-macos-arm64.tar.gz",
    "MACOS_X64_SHA256": "sfh-macos-x64.tar.gz",
    "LINUX_ARM64_SHA256": "sfh-linux-arm64.tar.gz",
    "LINUX_X64_SHA256": "sfh-linux-x64.tar.gz",
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
        description="Render Homebrew and WinGet metadata for an sfh release."
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
        help="directory to receive homebrew/ and winget/ trees",
    )
    parser.add_argument(
        "--release-assets-dir",
        type=Path,
        help="also stage sfh.rb and a WinGet manifest zip for GitHub Releases",
    )
    args = parser.parse_args()

    if not VERSION_RE.fullmatch(args.version):
        raise SystemExit(f"invalid release version: {args.version!r}")

    values = {"VERSION": args.version}
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

    winget_root = args.output_dir / "winget"
    winget_destination = (
        winget_root
        / "manifests/a/Aero123421/SimpleFlowHarness"
        / args.version
    )
    for template in sorted((ROOT / "packaging/winget").glob("*.template")):
        render(
            template,
            winget_destination / template.name.removesuffix(".template"),
            values,
        )

    if args.release_assets_dir:
        args.release_assets_dir.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(formula, args.release_assets_dir / "sfh.rb")
        archive = args.release_assets_dir / "sfh-winget-manifests.zip"
        with zipfile.ZipFile(
            archive, mode="w", compression=zipfile.ZIP_DEFLATED
        ) as bundle:
            for manifest in sorted(winget_root.rglob("*.yaml")):
                bundle.write(manifest, manifest.relative_to(winget_root))
        print(archive)


if __name__ == "__main__":
    main()
