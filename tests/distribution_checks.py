#!/usr/bin/env python3
"""Independent checks for release-channel metadata generation."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RENDERER = ROOT / "scripts/render_distribution.py"
ASSETS = [
    "sfh-windows-x64.zip",
    "sfh-macos-arm64.tar.gz",
    "sfh-macos-x64.tar.gz",
    "sfh-linux-arm64.tar.gz",
    "sfh-linux-x64.tar.gz",
]


def write_checksums(directory: Path) -> dict[str, str]:
    checksums: dict[str, str] = {}
    for index, asset in enumerate(ASSETS, start=1):
        checksum = f"{index:x}" * 64
        checksums[asset] = checksum
        (directory / f"{asset}.sha256").write_text(
            f"{checksum}  {asset}\n", encoding="ascii"
        )
    return checksums


class DistributionChecks(unittest.TestCase):
    def run_renderer(
        self,
        checksums: Path,
        output: Path,
        release_assets: Path,
        version: str = "9.8.7",
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                sys.executable,
                str(RENDERER),
                "--version",
                version,
                "--checksums-dir",
                str(checksums),
                "--output-dir",
                str(output),
                "--release-assets-dir",
                str(release_assets),
            ],
            cwd=ROOT,
            check=False,
            text=True,
            capture_output=True,
        )

    def test_renders_complete_homebrew_and_winget_sets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            checksums_dir = work / "checksums"
            checksums_dir.mkdir()
            checksums = write_checksums(checksums_dir)

            output = work / "generated"
            release_assets = work / "release"
            result = self.run_renderer(checksums_dir, output, release_assets)
            self.assertEqual(result.returncode, 0, result.stderr)

            formula = output / "homebrew/Formula/sfh.rb"
            formula_text = formula.read_text(encoding="utf-8")
            self.assertIn('version "9.8.7"', formula_text)
            for asset in ASSETS[1:]:
                self.assertIn(checksums[asset], formula_text)
            self.assertNotIn("{{", formula_text)

            manifest_dir = (
                output
                / "winget/manifests/a/Aero123421/SimpleFlowHarness/9.8.7"
            )
            manifests = sorted(manifest_dir.glob("*.yaml"))
            self.assertEqual(len(manifests), 3)
            for manifest in manifests:
                text = manifest.read_text(encoding="utf-8")
                self.assertIn("PackageVersion: 9.8.7", text)
                self.assertIn("ManifestVersion: 1.12.0", text)
                self.assertNotIn("{{", text)

            ruby = shutil.which("ruby")
            if ruby:
                syntax = subprocess.run(
                    [ruby, "-c", str(formula)],
                    check=False,
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(syntax.returncode, 0, syntax.stderr)
                for manifest in manifests:
                    yaml_parse = subprocess.run(
                        [
                            ruby,
                            "-e",
                            "require 'yaml'; YAML.safe_load_file(ARGV.fetch(0))",
                            str(manifest),
                        ],
                        check=False,
                        text=True,
                        capture_output=True,
                    )
                    self.assertEqual(
                        yaml_parse.returncode, 0, yaml_parse.stderr
                    )

            installer = (
                manifest_dir
                / "Aero123421.SimpleFlowHarness.installer.yaml"
            ).read_text(encoding="utf-8")
            self.assertIn(checksums["sfh-windows-x64.zip"], installer)
            self.assertIn(
                "/releases/download/v9.8.7/sfh-windows-x64.zip", installer
            )

            self.assertEqual(
                (release_assets / "sfh.rb").read_text(encoding="utf-8"),
                formula_text,
            )
            with zipfile.ZipFile(
                release_assets / "sfh-winget-manifests.zip"
            ) as bundle:
                self.assertEqual(
                    sorted(bundle.namelist()),
                    sorted(
                        str(path.relative_to(output / "winget")).replace(
                            "\\", "/"
                        )
                        for path in manifests
                    ),
                )

    def test_rejects_mismatched_checksum_filename(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            checksums_dir = work / "checksums"
            checksums_dir.mkdir()
            write_checksums(checksums_dir)
            bad = checksums_dir / "sfh-windows-x64.zip.sha256"
            bad.write_text(f"{'a' * 64}  another.zip\n", encoding="ascii")

            result = self.run_renderer(
                checksums_dir, work / "generated", work / "release"
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("must contain", result.stderr)

    def test_rejects_non_semver_release(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            checksums_dir = work / "checksums"
            checksums_dir.mkdir()
            write_checksums(checksums_dir)

            result = self.run_renderer(
                checksums_dir,
                work / "generated",
                work / "release",
                version="../escape",
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("invalid release version", result.stderr)


if __name__ == "__main__":
    unittest.main()
