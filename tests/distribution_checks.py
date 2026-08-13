#!/usr/bin/env python3
"""Independent checks for release-channel metadata generation."""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
import warnings
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RENDERER = ROOT / "scripts/render_distribution.py"
RELEASE_HELPER = ROOT / "scripts/release_assets.py"
RESOURCE_CONTRACT = ROOT / "release-resources.txt"
CONTENT_MANIFEST = ROOT / "release-content-manifest.txt"
RELEASE_SIGNERS = ROOT / "release-signers.json"
RELEASE_WORKFLOW = ROOT / ".github/workflows/release.yml"
EXPECTED_RESOURCES = [
    "AGENTS.md",
    "CHANGELOG.md",
    "CONTRIBUTING.md",
    "LICENSE",
    "README.ja.md",
    "README.md",
    "SECURITY.md",
    "SUPPORT.md",
    "docs/",
    "examples/",
    "schema/",
    "skills/",
    "tests/",
]
ASSETS = [
    "sfh-macos-arm64.tar.gz",
    "sfh-macos-x64.tar.gz",
    "sfh-linux-arm64.tar.gz",
    "sfh-linux-x64.tar.gz",
]
WINDOWS_ASSET = "sfh-windows-x64.zip"
TEST_APPLE_TEAM_ID = "A1B2C3D4E5"
TEST_WINDOWS_SIGNER_SHA256 = "ab" * 32
DISTRIBUTION_ASSETS = ["sfh-installer.ps1", "sfh-installer.sh", "sfh.rb"]


def write_checksums(directory: Path) -> dict[str, str]:
    checksums: dict[str, str] = {}
    for index, asset in enumerate([*ASSETS, WINDOWS_ASSET], start=1):
        checksum = f"{index:x}" * 64
        checksums[asset] = checksum
        (directory / f"{asset}.sha256").write_text(
            f"{checksum}  {asset}\n", encoding="ascii"
        )
    return checksums


def write_complete_release_set(directory: Path, version: str = "9.8.7") -> set[str]:
    primary = {
        *ASSETS,
        WINDOWS_ASSET,
        *DISTRIBUTION_ASSETS,
        f"sfh-{version}-source.tar.gz",
    }
    for index, name in enumerate(sorted(primary), start=1):
        path = directory / name
        content = f"release asset {index}: {name}\n".encode("ascii")
        path.write_bytes(content)
        (directory / f"{name}.sha256").write_text(
            f"{hashlib.sha256(content).hexdigest()}  {name}\n",
            encoding="ascii",
        )
    return primary | {f"{name}.sha256" for name in primary}


def write_zip_member(
    archive: zipfile.ZipFile, source: zipfile.ZipFile, member: zipfile.ZipInfo
) -> None:
    content = source.read(member) if not member.is_dir() else b""
    clone = zipfile.ZipInfo(member.filename, date_time=member.date_time)
    clone.external_attr = member.external_attr
    clone.create_system = member.create_system
    archive.writestr(clone, content)


class DistributionChecks(unittest.TestCase):
    def run_renderer(
        self,
        checksums: Path,
        output: Path,
        release_assets: Path,
        version: str = "9.8.7",
        native_signing: bool = True,
    ) -> subprocess.CompletedProcess[str]:
        arguments = [
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
        ]
        if native_signing:
            arguments.extend(
                [
                    "--apple-team-id",
                    TEST_APPLE_TEAM_ID,
                    "--windows-codesign-cert-sha256",
                    TEST_WINDOWS_SIGNER_SHA256,
                ]
            )
        return subprocess.run(
            arguments,
            cwd=ROOT,
            check=False,
            text=True,
            capture_output=True,
        )

    def run_release_helper(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(RELEASE_HELPER), *arguments],
            cwd=ROOT,
            check=False,
            text=True,
            capture_output=True,
        )

    def test_renders_complete_homebrew_set(self) -> None:
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
            for asset in ASSETS:
                self.assertIn(checksums[asset], formula_text)
            self.assertNotIn("{{", formula_text)
            self.assertIn('pkgshare.install "release-resources.txt"', formula_text)
            for resource in EXPECTED_RESOURCES:
                self.assertIn(f'"{resource.rstrip("/")}"', formula_text)

            ruby = shutil.which("ruby")
            if ruby:
                syntax = subprocess.run(
                    [ruby, "-c", str(formula)],
                    check=False,
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(syntax.returncode, 0, syntax.stderr)

            self.assertEqual(
                (release_assets / "sfh.rb").read_text(encoding="utf-8"),
                formula_text,
            )
            shell_installer = (release_assets / "sfh-installer.sh").read_text(
                encoding="utf-8"
            )
            powershell_installer = (
                release_assets / "sfh-installer.ps1"
            ).read_text(encoding="utf-8")
            self.assertIn(TEST_APPLE_TEAM_ID, shell_installer)
            self.assertIn(TEST_WINDOWS_SIGNER_SHA256, powershell_installer)
            for asset in ASSETS:
                self.assertIn(checksums[asset], shell_installer)
            self.assertIn(checksums[WINDOWS_ASSET], powershell_installer)
            self.assertNotIn("{{APPLE_TEAM_ID}}", shell_installer)
            self.assertNotIn(
                "{{WINDOWS_CODESIGN_CERT_SHA256}}", powershell_installer
            )
            self.assertEqual(
                {path.name for path in release_assets.iterdir()},
                {"sfh-installer.ps1", "sfh-installer.sh", "sfh.rb"},
            )

    def test_renders_explicit_unsigned_installers_without_publisher_pins(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            checksums_dir = work / "checksums"
            checksums_dir.mkdir()
            write_checksums(checksums_dir)

            release_assets = work / "release"
            result = self.run_renderer(
                checksums_dir,
                work / "generated",
                release_assets,
                native_signing=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            shell_installer = (release_assets / "sfh-installer.sh").read_text(
                encoding="utf-8"
            )
            powershell_installer = (
                release_assets / "sfh-installer.ps1"
            ).read_text(encoding="utf-8")
            self.assertIn("EXPECTED_APPLE_TEAM_ID='UNSIGNED'", shell_installer)
            self.assertIn(
                '$ExpectedSignerCertificateSha256 = "UNSIGNED"',
                powershell_installer,
            )
            self.assertIn(
                'if [ "$EXPECTED_APPLE_TEAM_ID" != "UNSIGNED" ]',
                shell_installer,
            )
            self.assertIn(
                'if ($ExpectedSignerCertificateSha256 -cne "UNSIGNED")',
                powershell_installer,
            )

    def test_rejects_mismatched_checksum_filename(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            checksums_dir = work / "checksums"
            checksums_dir.mkdir()
            write_checksums(checksums_dir)
            bad = checksums_dir / "sfh-macos-arm64.tar.gz.sha256"
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

            prerelease = self.run_renderer(
                checksums_dir,
                work / "generated-prerelease",
                work / "release-prerelease",
                version="1.6.1-rc.1",
            )
            self.assertNotEqual(prerelease.returncode, 0)
            self.assertIn("invalid release version", prerelease.stderr)

    def test_rejects_invalid_native_signer_pins(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            checksums_dir = work / "checksums"
            checksums_dir.mkdir()
            write_checksums(checksums_dir)
            base = [
                sys.executable,
                str(RENDERER),
                "--version",
                "9.8.7",
                "--checksums-dir",
                str(checksums_dir),
                "--output-dir",
                str(work / "generated"),
                "--release-assets-dir",
                str(work / "release"),
            ]
            invalid_team = subprocess.run(
                [
                    *base,
                    "--apple-team-id",
                    "../../bad",
                    "--windows-codesign-cert-sha256",
                    TEST_WINDOWS_SIGNER_SHA256,
                ],
                cwd=ROOT,
                check=False,
                text=True,
                capture_output=True,
            )
            self.assertNotEqual(invalid_team.returncode, 0)
            self.assertIn("invalid Apple Team ID", invalid_team.stderr)

            invalid_windows_pin = subprocess.run(
                [
                    *base,
                    "--apple-team-id",
                    TEST_APPLE_TEAM_ID,
                    "--windows-codesign-cert-sha256",
                    "not-a-fingerprint",
                ],
                cwd=ROOT,
                check=False,
                text=True,
                capture_output=True,
            )
            self.assertNotEqual(invalid_windows_pin.returncode, 0)
            self.assertIn(
                "invalid Windows code-signing certificate SHA-256",
                invalid_windows_pin.stderr,
            )

    def test_release_resource_contract_is_exact_and_sorted(self) -> None:
        self.assertEqual(
            RESOURCE_CONTRACT.read_text(encoding="ascii").splitlines(),
            EXPECTED_RESOURCES,
        )

    def test_release_content_manifest_is_current_and_canonical(self) -> None:
        result = self.run_release_helper(
            "content-manifest", "--output", str(CONTENT_MANIFEST), "--check"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        records = CONTENT_MANIFEST.read_text(encoding="ascii").splitlines()
        self.assertTrue(records)
        paths = [record.split(" ", 2)[2].rstrip("/") for record in records]
        self.assertEqual(paths, sorted(paths))
        self.assertEqual(len(paths), len({path.casefold() for path in paths}))
        for record in records:
            self.assertRegex(record, r"^(?:d - .+|f [0-9a-f]{64} .+)$")

    def test_release_signer_pins_are_reviewed_repository_data(self) -> None:
        document = json.loads(RELEASE_SIGNERS.read_text(encoding="ascii"))
        self.assertEqual(
            set(document),
            {"schema_version", "apple_team_id", "windows_codesign_cert_sha256"},
        )
        self.assertEqual(document["schema_version"], 1)
        if document["apple_team_id"] is not None:
            self.assertRegex(document["apple_team_id"], r"^[A-Z0-9]{10}$")
        if document["windows_codesign_cert_sha256"] is not None:
            self.assertRegex(
                document["windows_codesign_cert_sha256"], r"^[0-9a-f]{64}$"
            )

    def test_readme_install_pins_match_the_package_version(self) -> None:
        with (ROOT / "Cargo.toml").open("rb") as manifest:
            version = tomllib.load(manifest)["package"]["version"]
        for readme in ("README.md", "README.ja.md"):
            text = (ROOT / readme).read_text(encoding="utf-8")
            pins = re.findall(r"`SFH_VERSION=([^`]+)`", text)
            self.assertEqual(pins, [version], f"stale install pin in {readme}")

    def test_shared_packager_builds_complete_tar_and_zip_packets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            for binary_name, asset_name in (
                ("sfh", "sfh-test.tar.gz"),
                ("sfh.exe", "sfh-test.zip"),
            ):
                binary = work / binary_name
                binary.write_bytes(b"test binary")
                asset = work / asset_name
                result = self.run_release_helper(
                    "package",
                    "--binary",
                    str(binary),
                    "--asset",
                    str(asset),
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue(asset.is_file())
                sidecar = asset.with_name(f"{asset.name}.sha256")
                self.assertEqual(
                    sidecar.read_text(encoding="ascii"),
                    f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n",
                )
                verify = self.run_release_helper(
                    "verify-archive",
                    "--archive",
                    str(asset),
                    "--binary-name",
                    binary_name,
                )
                self.assertEqual(verify.returncode, 0, verify.stderr)
                wrong_binary = work / f"wrong-{binary_name}"
                wrong_binary.write_bytes(b"different binary")
                verify = self.run_release_helper(
                    "verify-archive",
                    "--archive",
                    str(asset),
                    "--binary-name",
                    binary_name,
                    "--expected-binary",
                    str(wrong_binary),
                )
                self.assertNotEqual(verify.returncode, 0)
                self.assertIn("packaged binary differs", verify.stderr)

    def test_packager_excludes_untracked_files_from_resource_directories(self) -> None:
        sentinel = ROOT / "tests" / f"untracked-release-sentinel-{os.getpid()}.txt"
        try:
            sentinel.write_text("must not ship\n", encoding="ascii")
            self.assertNotIn(
                sentinel.relative_to(ROOT).as_posix(),
                subprocess.run(
                    ["git", "ls-files"],
                    cwd=ROOT,
                    check=True,
                    text=True,
                    capture_output=True,
                ).stdout.splitlines(),
            )
            with tempfile.TemporaryDirectory() as temporary:
                work = Path(temporary)
                binary = work / "sfh.exe"
                binary.write_bytes(b"test binary")
                packet = work / "packet.zip"
                result = self.run_release_helper(
                    "package", "--binary", str(binary), "--asset", str(packet)
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                with zipfile.ZipFile(packet) as archive:
                    self.assertNotIn(sentinel.relative_to(ROOT).as_posix(), archive.namelist())
        finally:
            sentinel.unlink(missing_ok=True)

    def test_every_packaged_resource_is_a_regular_git_entry(self) -> None:
        records = subprocess.run(
            ["git", "ls-files", "--stage", "-z"],
            cwd=ROOT,
            check=True,
            capture_output=True,
        ).stdout.split(b"\0")
        modes = {}
        for record in records:
            if not record:
                continue
            metadata, raw_name = record.split(b"\t", 1)
            modes[raw_name.decode("utf-8")] = metadata.split()[0].decode("ascii")
        for resource in EXPECTED_RESOURCES:
            prefix = resource if resource.endswith("/") else None
            names = (
                [name for name in modes if name.startswith(prefix)]
                if prefix
                else [resource]
            )
            self.assertTrue(names, resource)
            for name in names:
                self.assertIn(modes[name], {"100644", "100755"}, name)

    def test_archive_verifier_rejects_noncanonical_and_special_members(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            binary = work / "sfh.exe"
            binary.write_bytes(b"test binary")
            packet = work / "packet.zip"
            result = self.run_release_helper(
                "package", "--binary", str(binary), "--asset", str(packet)
            )
            self.assertEqual(result.returncode, 0, result.stderr)

            for kind in ("backslash", "traversal", "symlink", "duplicate"):
                rewritten = work / f"{kind}.zip"
                with zipfile.ZipFile(packet) as source, zipfile.ZipFile(
                    rewritten, "w", compression=zipfile.ZIP_DEFLATED
                ) as destination:
                    for original in source.infolist():
                        member = zipfile.ZipInfo(
                            original.filename,
                            date_time=original.date_time,
                        )
                        member.external_attr = original.external_attr
                        member.create_system = original.create_system
                        content = source.read(original) if not original.is_dir() else b""
                        if original.filename == "LICENSE":
                            if kind == "backslash":
                                member.filename = ".\\LICENSE"
                            elif kind == "traversal":
                                member.filename = "../LICENSE"
                            elif kind == "symlink":
                                member.create_system = 3
                                member.external_attr = (stat.S_IFLNK | 0o777) << 16
                        destination.writestr(member, content)
                        if kind == "duplicate" and original.filename == "LICENSE":
                            with warnings.catch_warnings():
                                warnings.simplefilter("ignore", UserWarning)
                                destination.writestr(member, content)

                verify = self.run_release_helper(
                    "verify-archive",
                    "--archive",
                    str(rewritten),
                    "--binary-name",
                    "sfh.exe",
                )
                self.assertNotEqual(verify.returncode, 0, kind)
                expected_error = {
                    "backslash": "unsafe archive member",
                    "traversal": "unsafe archive member",
                    "symlink": "unsupported archive member type",
                    "duplicate": "duplicate archive member",
                }[kind]
                self.assertIn(expected_error, verify.stderr)

            for kind in ("traversal", "symlink", "duplicate"):
                packet = work / f"{kind}.tar.gz"
                with tarfile.open(packet, "w:gz") as archive:
                    first = tarfile.TarInfo("../sfh" if kind == "traversal" else "sfh")
                    first.size = 0
                    if kind == "symlink":
                        first.type = tarfile.SYMTYPE
                        first.linkname = "target"
                    archive.addfile(first)
                    if kind == "duplicate":
                        archive.addfile(tarfile.TarInfo("sfh"))
                verify = self.run_release_helper(
                    "verify-archive",
                    "--archive",
                    str(packet),
                    "--binary-name",
                    "sfh",
                )
                self.assertNotEqual(verify.returncode, 0, kind)
                self.assertIn(
                    {
                        "traversal": "unsafe archive member",
                        "symlink": "unsupported archive member type",
                        "duplicate": "duplicate archive member",
                    }[kind],
                    verify.stderr,
                )

    def test_archive_verifier_rejects_a_broken_link_in_any_markdown(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            binary = work / "sfh.exe"
            binary.write_bytes(b"test binary")
            packet = work / "packet.zip"
            result = self.run_release_helper(
                "package", "--binary", str(binary), "--asset", str(packet)
            )
            self.assertEqual(result.returncode, 0, result.stderr)

            rewritten = work / "broken.zip"
            with zipfile.ZipFile(packet) as source, zipfile.ZipFile(
                rewritten, "w", compression=zipfile.ZIP_DEFLATED
            ) as destination:
                for member in source.infolist():
                    if member.filename == "CONTRIBUTING.md":
                        content = source.read(member) + b"\n[missing](not-released.md)\n"
                        clone = zipfile.ZipInfo(member.filename, date_time=member.date_time)
                        clone.external_attr = member.external_attr
                        clone.create_system = member.create_system
                        destination.writestr(clone, content)
                    else:
                        write_zip_member(destination, source, member)

            verify = self.run_release_helper(
                "verify-archive",
                "--archive",
                str(rewritten),
                "--binary-name",
                "sfh.exe",
            )
            self.assertNotEqual(verify.returncode, 0)
            self.assertIn("broken archive link", verify.stderr)

    def test_archive_verifier_rejects_stale_resource_bytes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            binary = work / "sfh.exe"
            binary.write_bytes(b"test binary")
            packet = work / "packet.zip"
            result = self.run_release_helper(
                "package", "--binary", str(binary), "--asset", str(packet)
            )
            self.assertEqual(result.returncode, 0, result.stderr)

            rewritten = work / "stale.zip"
            with zipfile.ZipFile(packet) as source, zipfile.ZipFile(
                rewritten, "w", compression=zipfile.ZIP_DEFLATED
            ) as destination:
                for member in source.infolist():
                    if member.filename == "LICENSE":
                        content = b"stale release license\n"
                        clone = zipfile.ZipInfo(member.filename, date_time=member.date_time)
                        clone.external_attr = member.external_attr
                        clone.create_system = member.create_system
                        destination.writestr(clone, content)
                    else:
                        write_zip_member(destination, source, member)

            verify = self.run_release_helper(
                "verify-archive",
                "--archive",
                str(rewritten),
                "--binary-name",
                "sfh.exe",
            )
            self.assertNotEqual(verify.returncode, 0)
            self.assertIn("differs from the tagged checkout: LICENSE", verify.stderr)

    def test_manifest_and_provenance_bind_the_complete_asset_set(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            dist = work / "dist"
            dist.mkdir()
            staged = write_complete_release_set(dist)
            binary = dist / "sfh-linux-x64.tar.gz"

            provenance = dist / "provenance.json"
            result = self.run_release_helper(
                "provenance",
                "--directory",
                str(dist),
                "--output",
                str(provenance),
                "--version",
                "9.8.7",
                "--tag",
                "v9.8.7",
                "--commit",
                "a" * 40,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            document = json.loads(provenance.read_text(encoding="ascii"))
            self.assertEqual(document["commit"], "a" * 40)
            self.assertEqual(
                set(document["assets"]),
                staged,
            )

            manifest = dist / "SHA256SUMS"
            result = self.run_release_helper(
                "manifest",
                "--directory",
                str(dist),
                "--output",
                str(manifest),
                "--version",
                "9.8.7",
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            verify = self.run_release_helper(
                "verify-manifest",
                "--directory",
                str(dist),
                "--manifest",
                str(manifest),
                "--version",
                "9.8.7",
            )
            self.assertEqual(verify.returncode, 0, verify.stderr)
            listed = {line.split()[1] for line in manifest.read_text().splitlines()}
            self.assertEqual(listed, {path.name for path in dist.iterdir()} - {manifest.name})

            binary.write_bytes(b"tampered")
            verify = self.run_release_helper(
                "verify-manifest",
                "--directory",
                str(dist),
                "--manifest",
                str(manifest),
                "--version",
                "9.8.7",
            )
            self.assertNotEqual(verify.returncode, 0)
            self.assertIn("SHA256SUMS mismatch", verify.stderr)

    def test_provenance_rejects_missing_and_unexpected_assets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            write_complete_release_set(dist)
            (dist / "sfh-windows-x64.zip").unlink()
            (dist / "sfh-windows-x64.zip.sha256").unlink()
            (dist / "unsigned-extra.zip").write_bytes(b"extra")
            result = self.run_release_helper(
                "provenance",
                "--directory",
                str(dist),
                "--output",
                str(dist / "provenance.json"),
                "--version",
                "9.8.7",
                "--tag",
                "v9.8.7",
                "--commit",
                "a" * 40,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing: sfh-windows-x64.zip", result.stderr)
            self.assertIn("unexpected: unsigned-extra.zip", result.stderr)

    def test_remote_draft_assets_must_exactly_match_the_local_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            dist = work / "dist"
            dist.mkdir()
            write_complete_release_set(dist)
            provenance = dist / "provenance.json"
            result = self.run_release_helper(
                "provenance",
                "--directory",
                str(dist),
                "--output",
                str(provenance),
                "--version",
                "9.8.7",
                "--tag",
                "v9.8.7",
                "--commit",
                "a" * 40,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = dist / "SHA256SUMS"
            result = self.run_release_helper(
                "manifest",
                "--directory",
                str(dist),
                "--output",
                str(manifest),
                "--version",
                "9.8.7",
            )
            self.assertEqual(result.returncode, 0, result.stderr)

            remote = work / "remote-assets.json"
            records = [
                {
                    "name": path.name,
                    "digest": f"sha256:{hashlib.sha256(path.read_bytes()).hexdigest()}",
                    "state": "uploaded",
                }
                for path in sorted(dist.iterdir())
            ]
            remote.write_text(json.dumps(records), encoding="utf-8")
            arguments = (
                "verify-remote-assets",
                "--directory",
                str(dist),
                "--manifest",
                str(manifest),
                "--remote-assets",
                str(remote),
                "--version",
                "9.8.7",
            )
            result = self.run_release_helper(*arguments)
            self.assertEqual(result.returncode, 0, result.stderr)

            records[0]["digest"] = f"sha256:{'f' * 64}"
            records.append(
                {
                    "name": "unlisted.bin",
                    "digest": f"sha256:{'e' * 64}",
                    "state": "uploaded",
                }
            )
            remote.write_text(json.dumps(records), encoding="utf-8")
            result = self.run_release_helper(*arguments)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unexpected: unlisted.bin", result.stderr)
            self.assertIn("digest mismatch", result.stderr)

    def test_release_notes_are_the_current_changelog_section(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            changelog = work / "CHANGELOG.md"
            output = work / "notes.md"
            changelog.write_text(
                "# Changelog\n\n## v2.0.0 - today\n\nCurrent notes.\n\n"
                "## v1.0.0 - yesterday\n\nOld notes.\n",
                encoding="utf-8",
            )
            result = self.run_release_helper(
                "release-notes",
                "--changelog",
                str(changelog),
                "--tag",
                "v2.0.0",
                "--output",
                str(output),
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output.read_text(encoding="utf-8"), "Current notes.\n")

            stale = self.run_release_helper(
                "release-notes",
                "--changelog",
                str(changelog),
                "--tag",
                "v1.0.0",
                "--output",
                str(output),
            )
            self.assertNotEqual(stale.returncode, 0)
            self.assertIn("top CHANGELOG", stale.stderr)

    def test_release_workflow_is_pinned_and_publishes_a_completed_draft(self) -> None:
        workflow = RELEASE_WORKFLOW.read_text(encoding="utf-8")
        remote_actions = re.findall(r"uses:\s+([^\s#]+)", workflow)
        for action in remote_actions:
            if action.startswith("./"):
                continue
            reference = action.rsplit("@", 1)[-1]
            self.assertRegex(reference, r"^[0-9a-f]{40}$", action)

        self.assertEqual(workflow.count("release_assets.py package"), 1)
        self.assertIn("actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6", workflow)
        contract = workflow.index("  release-contract:")
        signing = workflow.index("  signing-contract:")
        build = workflow.index("  build:")
        publish = workflow.index("  publish:")
        preflight = workflow.index(
            "Prove the remote release boundary before creating a draft", publish
        )
        draft = workflow.index("Create draft release and upload", preflight)
        remote_verify = workflow.index("release_assets.py verify-remote-assets", draft)
        attest = workflow.index("actions/attest@", remote_verify)
        immutable_gate = workflow.index("immutable-releases", attest)
        final_remote_verify = workflow.index(
            "final-remote-assets.json", immutable_gate
        )
        finalize = workflow.index("Publish the completed draft release", attest)
        cleanup = workflow.index("Remove this run's incomplete mutable release", finalize)
        self.assertLess(preflight, draft)
        self.assertIn("immutable-releases", workflow[preflight:draft])
        self.assertIn("A release already exists", workflow[preflight:draft])
        self.assertNotIn("softprops/action-gh-release", workflow)
        self.assertIn("--method POST", workflow[draft:remote_verify])
        self.assertIn(".upload_url", workflow[draft:remote_verify])
        self.assertIn('"$upload_url?name=$name"', workflow[draft:remote_verify])
        self.assertNotIn("gh release upload", workflow[draft:remote_verify])
        output_id = workflow.index('echo "id=$release_id"', draft, remote_verify)
        assert_draft = workflow.index(".draft' <<< \"$release\")", draft, remote_verify)
        self.assertLess(output_id, assert_draft)
        self.assertLess(draft, remote_verify)
        self.assertLess(remote_verify, attest)
        self.assertLess(attest, immutable_gate)
        self.assertIn(".enabled", workflow[immutable_gate:finalize])
        self.assertLess(immutable_gate, final_remote_verify)
        self.assertIn(
            "release_assets.py verify-remote-assets",
            workflow[final_remote_verify:finalize],
        )
        self.assertIn("git ls-remote origin", workflow[final_remote_verify:finalize])
        self.assertIn(
            'needs.release-contract.outputs.release_commit',
            workflow[final_remote_verify:finalize],
        )
        self.assertLess(final_remote_verify, finalize)
        self.assertIn("gh api --method PATCH", workflow[finalize:])
        self.assertIn('{"draft":false,"make_latest":"true"}', workflow[finalize:])
        self.assertIn(".immutable", workflow[finalize:])
        self.assertIn("published-assets.json", workflow[finalize:])
        self.assertIn("release_assets.py verify-remote-assets", workflow[finalize:])
        self.assertIn("steps.draft.outputs.id", workflow[cleanup:])
        self.assertIn("failure() || cancelled()", workflow[cleanup:])
        self.assertIn("immutable", workflow[cleanup:])
        self.assertIn("--write-out '%{http_code}'", workflow[cleanup:])
        self.assertIn("status\" = 404", workflow[cleanup:])
        self.assertIn("group: release-${{ github.ref }}", workflow)
        self.assertIn("cancel-in-progress: false", workflow)
        self.assertLess(attest, finalize)
        self.assertIn("fetch-depth: 0", workflow[contract:signing])
        self.assertIn("git merge-base --is-ancestor", workflow[contract:signing])
        self.assertIn("Release workflow publishes stable semantic versions only", workflow[contract:signing])
        self.assertIn(
            "git ls-files --error-unmatch release-resources.txt",
            workflow[contract:signing],
        )
        self.assertIn("release_assets.py content-manifest --check", workflow[contract:signing])
        self.assertIn("release-signers.json", workflow[contract:signing])
        self.assertIn('git rev-parse "$GITHUB_SHA^{commit}"', workflow[contract:signing])
        self.assertIn("needs: [verify, release-contract]", workflow[signing:build])
        self.assertIn("Validate optional native signing credentials", workflow[signing:build])
        self.assertIn("Apple signer pin is unset", workflow[signing:build])
        self.assertIn("Windows signer pin is unset", workflow[signing:build])
        self.assertIn("Missing required Actions secret: RELEASE_ADMIN_READ_TOKEN", workflow[signing:build])
        self.assertIn(
            "needs: [verify, release-contract, signing-contract]",
            workflow[build:publish],
        )
        self.assertIn("environment: release", workflow[signing:build])
        self.assertIn("environment: release", workflow[build:publish])
        self.assertIn("environment: release", workflow[publish:])
        for permission in (
            "contents: write",
            "id-token: write",
            "attestations: write",
            "artifact-metadata: write",
        ):
            self.assertIn(permission, workflow[publish:])
        self.assertIn("--version \"$version\"", workflow[publish:draft])
        self.assertIn(".draft' <<< \"$release\")\" = true", workflow[draft:attest])
        self.assertIn(".tag_name' <<< \"$release\")\" = \"$GITHUB_REF_NAME\"", workflow[draft:attest])
        self.assertNotIn("generate_release_notes: true", workflow)
        self.assertIn("Get-AuthenticodeSignature", workflow)
        self.assertIn("TimeStamperCertificate", workflow)
        self.assertIn("SignerCertificate.RawData", workflow)
        self.assertIn("pinned publisher", workflow)
        self.assertIn("Authority=Developer ID Application:", workflow)
        self.assertIn('TeamIdentifier=$APPLE_TEAM_ID', workflow)
        self.assertIn(
            "runner.os == 'Windows' && needs.release-contract.outputs.windows_codesign_cert_sha256 != ''",
            workflow,
        )
        self.assertIn(
            "runner.os == 'macOS' && needs.release-contract.outputs.apple_team_id != ''",
            workflow,
        )
        self.assertIn("notarytool submit", workflow)
        self.assertIn('result.get("status") == "Accepted"', workflow)
        self.assertIn("^Timestamp=(none|-)$", workflow)
        self.assertIn("os: macos-15-intel", workflow)
        self.assertNotIn("cross: true", workflow)
        macos_x64 = workflow.index("target: x86_64-apple-darwin")
        self.assertIn("run_binary: true", workflow[macos_x64 : macos_x64 + 180])
        for secret in (
            "APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64",
            "APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD",
            "APPLE_SIGNING_IDENTITY",
            "APPLE_NOTARY_KEY_ID",
            "APPLE_NOTARY_ISSUER_ID",
            "APPLE_NOTARY_KEY_P8_BASE64",
            "WINDOWS_CODESIGN_PFX_BASE64",
            "WINDOWS_CODESIGN_PFX_PASSWORD",
            "WINDOWS_CODESIGN_TIMESTAMP_URL",
            "RELEASE_ADMIN_READ_TOKEN",
        ):
            self.assertIn(f"secrets.{secret}", workflow)
        self.assertNotIn("secrets.APPLE_TEAM_ID", workflow)
        self.assertNotIn("secrets.WINDOWS_CODESIGN_CERT_SHA256", workflow)
        self.assertIn("needs.release-contract.outputs.apple_team_id", workflow)
        self.assertIn(
            "needs.release-contract.outputs.windows_codesign_cert_sha256",
            workflow,
        )


if __name__ == "__main__":
    unittest.main()
