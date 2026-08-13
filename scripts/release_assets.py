#!/usr/bin/env python3
"""Build and verify the complete, cross-platform sfh release packet."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import tarfile
import tempfile
import zipfile
from datetime import datetime, timezone
from functools import lru_cache
from bisect import bisect_left
from pathlib import Path, PurePosixPath
from urllib.parse import unquote


ROOT = Path(__file__).resolve().parents[1]
RESOURCE_CONTRACT = ROOT / "release-resources.txt"
CONTENT_MANIFEST = ROOT / "release-content-manifest.txt"
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
MARKDOWN_LINK_RE = re.compile(r"\[[^\]]+\]\(([^)]+)\)")
PLATFORM_ASSETS = (
    "sfh-linux-arm64.tar.gz",
    "sfh-linux-x64.tar.gz",
    "sfh-macos-arm64.tar.gz",
    "sfh-macos-x64.tar.gz",
    "sfh-windows-x64.zip",
)
DISTRIBUTION_ASSETS = ("sfh-installer.ps1", "sfh-installer.sh", "sfh.rb")


def fail(message: str) -> SystemExit:
    return SystemExit(message)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def read_resource_contract() -> list[str]:
    try:
        raw = RESOURCE_CONTRACT.read_text(encoding="ascii")
    except FileNotFoundError as error:
        raise fail(f"missing release resource contract: {RESOURCE_CONTRACT}") from error

    entries = raw.splitlines()
    if not entries or any(not entry or entry.startswith("#") for entry in entries):
        raise fail("release-resources.txt cannot contain comments or blank lines")
    if entries != sorted(set(entries)):
        raise fail("release-resources.txt must be unique and sorted")

    for entry in entries:
        path = PurePosixPath(entry.rstrip("/"))
        if path.is_absolute() or not path.parts or ".." in path.parts:
            raise fail(f"unsafe release resource: {entry!r}")
        source = ROOT.joinpath(*path.parts)
        if source.is_symlink():
            raise fail(f"release resources cannot be symlinks: {entry}")
        if entry.endswith("/") and not source.is_dir():
            raise fail(f"release resource must be a directory: {entry}")
        if not entry.endswith("/") and not source.is_file():
            raise fail(f"release resource must be a file: {entry}")
    return entries


@lru_cache(maxsize=1)
def tracked_modes() -> dict[str, str]:
    result = subprocess.run(
        ["git", "ls-files", "--stage", "-z"],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )
    modes: dict[str, str] = {}
    for record in result.stdout.split(b"\0"):
        if not record:
            continue
        metadata, separator, raw_name = record.partition(b"\t")
        fields = metadata.split()
        if not separator or len(fields) != 3 or fields[2] != b"0":
            raise fail("git index contains an unsupported staged entry")
        name = raw_name.decode("utf-8")
        if name in modes:
            raise fail(f"git index contains a duplicate path: {name}")
        modes[name] = fields[0].decode("ascii")
    return modes


def tracked_files() -> frozenset[str]:
    return frozenset(tracked_modes())


def require_regular_git_mode(name: str) -> None:
    mode = tracked_modes().get(name)
    if mode not in ("100644", "100755"):
        raise fail(f"release resource is not a regular tracked file: {name}")


def resource_members(entries: list[str]) -> dict[str, Path]:
    tracked = tracked_files()
    tracked_sorted = sorted(tracked)

    def has_tracked_child(directory: str) -> bool:
        prefix = f"{directory}/"
        index = bisect_left(tracked_sorted, prefix)
        return index < len(tracked_sorted) and tracked_sorted[index].startswith(prefix)

    members: dict[str, Path] = {"release-resources.txt": RESOURCE_CONTRACT}
    for entry in entries:
        relative = PurePosixPath(entry.rstrip("/"))
        source = ROOT.joinpath(*relative.parts)
        if source.is_dir():
            members[f"{relative.as_posix()}/"] = source
            prefix = f"{relative.as_posix()}/"
            tracked_children = {name for name in tracked if name.startswith(prefix)}
            for child in sorted(source.rglob("*")):
                child_relative = child.relative_to(ROOT).as_posix()
                tracked_node = child_relative in tracked
                tracked_below = has_tracked_child(child_relative)
                if not tracked_node and not tracked_below:
                    continue
                if child.is_symlink():
                    raise fail(
                        "release resource trees cannot contain symlinks: "
                        f"{child.relative_to(ROOT)}"
                    )
                if tracked_node:
                    require_regular_git_mode(child_relative)
                    if not child.is_file():
                        raise fail(
                            "tracked release resource is not a regular file: "
                            f"{child_relative}"
                        )
                elif not child.is_dir():
                    raise fail(
                        "release resource parent is not a directory: "
                        f"{child_relative}"
                    )
                members[child_relative + ("/" if child.is_dir() else "")] = child
            missing = sorted(tracked_children - members.keys())
            if missing:
                raise fail(
                    "tracked release resource is missing or unsupported: " + missing[0]
                )
        else:
            if relative.as_posix() not in tracked:
                raise fail(f"release resource is not tracked by git: {entry}")
            require_regular_git_mode(relative.as_posix())
            members[relative.as_posix()] = source
    folded: dict[str, str] = {}
    for name in members:
        key = name.casefold()
        previous = folded.get(key)
        if previous is not None and previous != name:
            raise fail(
                "release resource paths collide on a case-insensitive filesystem: "
                f"{previous!r} and {name!r}"
            )
        folded[key] = name
        if any(ord(character) < 0x20 or ord(character) > 0x7E for character in name):
            raise fail(f"release resource path is not printable ASCII: {name!r}")
    return members


def indexed_bytes(name: str) -> bytes:
    require_regular_git_mode(name)
    result = subprocess.run(
        ["git", "show", f":{name}"],
        cwd=ROOT,
        check=True,
        capture_output=True,
    )
    return result.stdout


def content_manifest_text() -> str:
    members = resource_members(read_resource_contract())
    records: list[str] = []
    for name in sorted(members):
        if name.endswith("/"):
            records.append(f"d - {name}\n")
        else:
            digest = hashlib.sha256(indexed_bytes(name)).hexdigest()
            records.append(f"f {digest} {name}\n")
    return "".join(records)


def write_or_check_content_manifest(output: Path, check: bool) -> None:
    expected = content_manifest_text().encode("ascii")
    if check:
        try:
            actual = output.read_bytes()
        except FileNotFoundError as error:
            raise fail(f"missing release content manifest: {output}") from error
        if actual != expected:
            raise fail(
                "release-content-manifest.txt is stale; regenerate it with "
                "scripts/release_assets.py content-manifest"
            )
        return
    output.write_bytes(expected)


def normalized_member(name: str, is_directory: bool, directory_suffix: bool) -> str:
    normalized = name.rstrip("/")
    path = PurePosixPath(normalized)
    canonical = normalized + ("/" if is_directory and directory_suffix else "")
    if (
        not normalized
        or name != canonical
        or "\\" in name
        or ":" in normalized
        or path.is_absolute()
        or path.as_posix() != normalized
        or any(part in ("", ".", "..") for part in path.parts)
    ):
        raise fail(f"unsafe archive member: {name!r}")
    return normalized + ("/" if is_directory else "")


def zip_member_is_directory(member: zipfile.ZipInfo) -> bool:
    unix_type = (member.external_attr >> 16) & 0xF000
    dos_directory = bool(member.external_attr & 0x10)
    return member.is_dir() or unix_type == 0x4000 or dos_directory


def verify_zip_member_type(member: zipfile.ZipInfo, is_directory: bool) -> None:
    unix_type = (member.external_attr >> 16) & 0xF000
    if unix_type not in (0, 0x4000, 0x8000):
        raise fail(f"unsupported archive member type: {member.filename}")
    if member.external_attr & 0x400:
        raise fail(f"unsupported archive reparse point: {member.filename}")
    if is_directory != member.filename.endswith("/"):
        raise fail(f"archive member type disagrees with its name: {member.filename}")


def archive_contents(archive_path: Path) -> tuple[dict[str, bytes], set[str]]:
    files: dict[str, bytes] = {}
    directories: set[str] = set()
    if archive_path.name.endswith(".tar.gz"):
        with tarfile.open(archive_path, "r:gz") as archive:
            for member in archive.getmembers():
                if not (member.isfile() or member.isdir()):
                    raise fail(f"unsupported archive member type: {member.name}")
                name = normalized_member(member.name, member.isdir(), False)
                if name in files or name in directories:
                    raise fail(f"duplicate archive member: {name}")
                if member.isdir():
                    directories.add(name)
                else:
                    stream = archive.extractfile(member)
                    if stream is None:
                        raise fail(f"cannot read archive member: {name}")
                    files[name] = stream.read()
    elif archive_path.suffix == ".zip":
        with zipfile.ZipFile(archive_path) as archive:
            for member in archive.infolist():
                is_directory = zip_member_is_directory(member)
                verify_zip_member_type(member, is_directory)
                name = normalized_member(member.filename, is_directory, True)
                if name in files or name in directories:
                    raise fail(f"duplicate archive member: {name}")
                if is_directory:
                    directories.add(name)
                else:
                    files[name] = archive.read(member)
    else:
        raise fail(f"unsupported release archive: {archive_path}")
    return files, directories


def resolve_link(base: str, target: str) -> str | None:
    target = target.strip().strip("<>")
    if not target or target.startswith("#"):
        return None
    if re.match(r"^[A-Za-z][A-Za-z0-9+.-]*:", target):
        return None
    path_text = unquote(target.split("#", 1)[0].split("?", 1)[0])
    combined = PurePosixPath(base).parent / PurePosixPath(path_text)
    parts: list[str] = []
    for part in combined.parts:
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                raise fail(f"Markdown link escapes the archive: {target}")
            parts.pop()
        else:
            parts.append(part)
    resolved = "/".join(parts)
    return resolved + ("/" if path_text.endswith("/") else "")


def verify_markdown_links(files: dict[str, bytes], directories: set[str]) -> None:
    for readme in sorted(name for name in files if name.lower().endswith(".md")):
        try:
            text = files[readme].decode("utf-8")
        except KeyError as error:
            raise fail(f"archive is missing {readme}") from error
        for target in MARKDOWN_LINK_RE.findall(text):
            resolved = resolve_link(readme, target)
            if resolved is None:
                continue
            if resolved.endswith("/"):
                if resolved not in directories:
                    raise fail(f"broken archive link in {readme}: {target}")
            elif resolved not in files:
                raise fail(f"broken archive link in {readme}: {target}")


def expected_members(binary_name: str) -> tuple[set[str], set[str]]:
    members = resource_members(read_resource_contract())
    files = {name for name in members if not name.endswith("/")}
    directories = {name for name in members if name.endswith("/")}
    files.add(binary_name)
    return files, directories


def verify_archive(archive_path: Path, binary_name: str, binary: Path | None = None) -> None:
    files, directories = archive_contents(archive_path)
    expected_files, expected_directories = expected_members(binary_name)
    missing = sorted((expected_files - files.keys()) | (expected_directories - directories))
    unexpected = sorted((files.keys() - expected_files) | (directories - expected_directories))
    if missing or unexpected:
        details = []
        if missing:
            details.append(f"missing: {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected: {', '.join(unexpected)}")
        raise fail("release archive does not match release-resources.txt (" + "; ".join(details) + ")")

    verify_markdown_links(files, directories)
    expected_resources = resource_members(read_resource_contract())
    for name, source in expected_resources.items():
        if name.endswith("/"):
            continue
        if files[name] != source.read_bytes():
            raise fail(f"archive resource differs from the tagged checkout: {name}")

    contract = files["release-resources.txt"].decode("ascii").splitlines()
    if contract != read_resource_contract():
        raise fail("archive release-resources.txt differs from the repository contract")
    if binary is not None and files[binary_name] != binary.read_bytes():
        raise fail("packaged binary differs from the signed build output")


def add_tar_member(archive: tarfile.TarFile, source: Path, name: str) -> None:
    archive.add(source, arcname=name.rstrip("/"), recursive=False)


def write_tar(binary: Path, asset: Path, members: dict[str, Path]) -> None:
    with tarfile.open(asset, "w:gz", format=tarfile.PAX_FORMAT) as archive:
        add_tar_member(archive, binary, binary.name)
        for name, source in sorted(members.items()):
            add_tar_member(archive, source, name)


def write_zip(binary: Path, asset: Path, members: dict[str, Path]) -> None:
    with zipfile.ZipFile(asset, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        archive.write(binary, binary.name)
        for name, source in sorted(members.items()):
            archive.write(source, name)


def package(binary: Path, asset: Path) -> None:
    if binary.name not in ("sfh", "sfh.exe") or not binary.is_file():
        raise fail("--binary must name an existing sfh or sfh.exe")
    members = resource_members(read_resource_contract())
    asset.parent.mkdir(parents=True, exist_ok=True)
    if asset.name.endswith(".tar.gz"):
        write_tar(binary, asset, members)
    elif asset.suffix == ".zip":
        write_zip(binary, asset, members)
    else:
        raise fail("--asset must end in .tar.gz or .zip")
    verify_archive(asset, binary.name, binary)
    sidecar = asset.with_name(f"{asset.name}.sha256")
    sidecar.write_text(f"{sha256(asset)}  {asset.name}\n", encoding="ascii", newline="\n")


def direct_files(directory: Path, excluded: set[str] | None = None) -> list[Path]:
    excluded = excluded or set()
    nested = [path for path in directory.rglob("*") if path.is_file() and path.parent != directory]
    if nested:
        raise fail(f"release asset directory must be flat: {nested[0]}")
    return sorted(
        path for path in directory.iterdir() if path.is_file() and path.name not in excluded
    )


def verify_sidecars(directory: Path) -> None:
    for sidecar in directory.glob("*.sha256"):
        asset = directory / sidecar.name.removesuffix(".sha256")
        if not asset.is_file():
            raise fail(f"checksum sidecar has no asset: {sidecar.name}")
        fields = sidecar.read_text(encoding="ascii").split()
        if len(fields) != 2 or fields[1].lstrip("*") != asset.name:
            raise fail(f"invalid checksum sidecar: {sidecar.name}")
        if not SHA256_RE.fullmatch(fields[0].lower()) or fields[0].lower() != sha256(asset):
            raise fail(f"checksum mismatch: {sidecar.name}")


def expected_release_assets(version: str, include_metadata: bool) -> set[str]:
    if not VERSION_RE.fullmatch(version):
        raise fail(f"invalid release version: {version!r}")
    primary = {
        *PLATFORM_ASSETS,
        *DISTRIBUTION_ASSETS,
        f"sfh-{version}-source.tar.gz",
    }
    expected = primary | {f"{name}.sha256" for name in primary}
    if include_metadata:
        expected |= {"provenance.json", "SHA256SUMS"}
    return expected


def verify_release_set(
    directory: Path, version: str, *, include_metadata: bool, excluded: set[str] | None = None
) -> list[Path]:
    expected = expected_release_assets(version, include_metadata)
    excluded = excluded or set()
    expected -= excluded
    assets = direct_files(directory, excluded)
    actual = {path.name for path in assets}
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing or unexpected:
        details = []
        if missing:
            details.append(f"missing: {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected: {', '.join(unexpected)}")
        raise fail("release asset set is incomplete (" + "; ".join(details) + ")")
    return assets


def write_provenance(
    directory: Path, output: Path, version: str, tag: str, commit: str
) -> None:
    if not VERSION_RE.fullmatch(version) or tag != f"v{version}":
        raise fail("provenance version and tag must agree")
    if not COMMIT_RE.fullmatch(commit):
        raise fail("provenance commit must be a full lowercase Git SHA")
    verify_sidecars(directory)
    excluded = {output.name, "SHA256SUMS"}
    assets = {
        path.name: sha256(path)
        for path in verify_release_set(
            directory, version, include_metadata=False, excluded=excluded
        )
    }
    source_archive = f"sfh-{version}-source.tar.gz"
    if source_archive not in assets:
        raise fail(f"missing source archive: {source_archive}")
    document = {
        "schema_version": 1,
        "version": version,
        "tag": tag,
        "commit": commit,
        "source_archive": source_archive,
        "archive_sha256": assets[source_archive],
        "generated_utc": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "assets": assets,
    }
    output.write_text(
        json.dumps(document, ensure_ascii=True, indent=2) + "\n",
        encoding="ascii",
        newline="\n",
    )


def write_manifest(directory: Path, output: Path, version: str) -> None:
    verify_sidecars(directory)
    assets = verify_release_set(
        directory,
        version,
        include_metadata=True,
        excluded={output.name},
    )
    if not assets:
        raise fail("cannot write an empty release manifest")
    output.write_text(
        "".join(f"{sha256(path)}  {path.name}\n" for path in assets),
        encoding="ascii",
        newline="\n",
    )


def verify_manifest(directory: Path, manifest: Path, version: str) -> None:
    records: dict[str, str] = {}
    for line in manifest.read_text(encoding="ascii").splitlines():
        fields = line.split()
        if len(fields) != 2 or not SHA256_RE.fullmatch(fields[0].lower()):
            raise fail(f"invalid release manifest line: {line!r}")
        name = fields[1].lstrip("*")
        if name in records or Path(name).name != name:
            raise fail(f"invalid or duplicate release manifest name: {name!r}")
        records[name] = fields[0].lower()
    expected = {
        path.name
        for path in verify_release_set(
            directory,
            version,
            include_metadata=True,
            excluded={manifest.name},
        )
    }
    if set(records) != expected:
        raise fail("SHA256SUMS does not list the complete release asset set")
    for name, digest in records.items():
        if sha256(directory / name) != digest:
            raise fail(f"SHA256SUMS mismatch: {name}")


def verify_remote_assets(
    directory: Path, manifest: Path, remote_assets: Path, version: str
) -> None:
    verify_manifest(directory, manifest, version)
    expected = {
        path.name: sha256(path)
        for path in verify_release_set(directory, version, include_metadata=True)
    }
    try:
        document = json.loads(remote_assets.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise fail(f"invalid remote release asset JSON: {error}") from error
    if not isinstance(document, list):
        raise fail("remote release asset JSON must be an array")

    remote: dict[str, str] = {}
    for item in document:
        if not isinstance(item, dict):
            raise fail("remote release asset record must be an object")
        name = item.get("name")
        digest = item.get("digest")
        state = item.get("state")
        digest_text = digest.removeprefix("sha256:") if isinstance(digest, str) else ""
        if (
            not isinstance(name, str)
            or Path(name).name != name
            or name in remote
            or not isinstance(digest, str)
            or not digest.startswith("sha256:")
            or not SHA256_RE.fullmatch(digest_text.lower())
            or state != "uploaded"
        ):
            raise fail(f"invalid or duplicate remote release asset record: {item!r}")
        remote[name] = digest_text.lower()
    if remote != expected:
        missing = sorted(expected.keys() - remote.keys())
        unexpected = sorted(remote.keys() - expected.keys())
        mismatched = sorted(
            name for name in expected.keys() & remote.keys() if expected[name] != remote[name]
        )
        details = []
        if missing:
            details.append(f"missing: {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected: {', '.join(unexpected)}")
        if mismatched:
            details.append(f"digest mismatch: {', '.join(mismatched)}")
        raise fail("remote draft asset set differs from SHA256SUMS (" + "; ".join(details) + ")")


def changelog_section(changelog: Path, tag: str) -> str:
    version = tag.removeprefix("v")
    if tag != f"v{version}" or not VERSION_RE.fullmatch(version):
        raise fail(f"invalid release tag: {tag!r}")
    text = changelog.read_text(encoding="utf-8")
    headings = list(re.finditer(r"^## v([^\s]+)(?:\s.*)?$", text, re.MULTILINE))
    if not headings or headings[0].group(1) != version:
        raise fail(f"top CHANGELOG release heading must be {tag}")
    start = headings[0].end()
    end = headings[1].start() if len(headings) > 1 else len(text)
    body = text[start:end].strip()
    if not body:
        raise fail(f"CHANGELOG section for {tag} is empty")
    return body + "\n"


def extract_and_run(archive: Path, binary_name: str) -> None:
    with tempfile.TemporaryDirectory() as temporary:
        destination = Path(temporary)
        if archive.name.endswith(".tar.gz"):
            with tarfile.open(archive, "r:gz") as packet:
                member = packet.getmember(binary_name)
                stream = packet.extractfile(member)
                if stream is None:
                    raise fail(f"cannot read packaged binary: {binary_name}")
                (destination / binary_name).write_bytes(stream.read())
        else:
            with zipfile.ZipFile(archive) as packet:
                packet.extract(binary_name, destination)
        binary = destination / binary_name
        if os.name != "nt":
            binary.chmod(binary.stat().st_mode | 0o111)
        subprocess.run([str(binary), "--version"], check=True)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    package_parser = commands.add_parser("package")
    package_parser.add_argument("--binary", type=Path, required=True)
    package_parser.add_argument("--asset", type=Path, required=True)
    package_parser.add_argument("--run", action="store_true", help="run the packaged binary")

    verify_parser = commands.add_parser("verify-archive")
    verify_parser.add_argument("--archive", type=Path, required=True)
    verify_parser.add_argument("--binary-name", choices=("sfh", "sfh.exe"), required=True)
    verify_parser.add_argument("--expected-binary", type=Path)

    run_parser = commands.add_parser("run-archive")
    run_parser.add_argument("--archive", type=Path, required=True)
    run_parser.add_argument("--binary-name", choices=("sfh", "sfh.exe"), required=True)

    provenance_parser = commands.add_parser("provenance")
    provenance_parser.add_argument("--directory", type=Path, required=True)
    provenance_parser.add_argument("--output", type=Path, required=True)
    provenance_parser.add_argument("--version", required=True)
    provenance_parser.add_argument("--tag", required=True)
    provenance_parser.add_argument("--commit", required=True)

    manifest_parser = commands.add_parser("manifest")
    manifest_parser.add_argument("--directory", type=Path, required=True)
    manifest_parser.add_argument("--output", type=Path, required=True)
    manifest_parser.add_argument("--version", required=True)

    check_parser = commands.add_parser("verify-manifest")
    check_parser.add_argument("--directory", type=Path, required=True)
    check_parser.add_argument("--manifest", type=Path, required=True)
    check_parser.add_argument("--version", required=True)

    remote_parser = commands.add_parser("verify-remote-assets")
    remote_parser.add_argument("--directory", type=Path, required=True)
    remote_parser.add_argument("--manifest", type=Path, required=True)
    remote_parser.add_argument("--remote-assets", type=Path, required=True)
    remote_parser.add_argument("--version", required=True)

    notes_parser = commands.add_parser("release-notes")
    notes_parser.add_argument("--changelog", type=Path, required=True)
    notes_parser.add_argument("--tag", required=True)
    notes_parser.add_argument("--output", type=Path, required=True)

    content_parser = commands.add_parser("content-manifest")
    content_parser.add_argument("--output", type=Path, default=CONTENT_MANIFEST)
    content_parser.add_argument("--check", action="store_true")
    return parser


def main() -> None:
    args = build_parser().parse_args()
    if args.command == "package":
        package(args.binary, args.asset)
        if args.run:
            extract_and_run(args.asset, args.binary.name)
    elif args.command == "verify-archive":
        verify_archive(args.archive, args.binary_name, args.expected_binary)
    elif args.command == "run-archive":
        verify_archive(args.archive, args.binary_name)
        extract_and_run(args.archive, args.binary_name)
    elif args.command == "provenance":
        write_provenance(args.directory, args.output, args.version, args.tag, args.commit)
    elif args.command == "manifest":
        write_manifest(args.directory, args.output, args.version)
    elif args.command == "verify-manifest":
        verify_manifest(args.directory, args.manifest, args.version)
    elif args.command == "verify-remote-assets":
        verify_remote_assets(
            args.directory, args.manifest, args.remote_assets, args.version
        )
    elif args.command == "release-notes":
        args.output.write_text(
            changelog_section(args.changelog, args.tag),
            encoding="utf-8",
            newline="\n",
        )
    elif args.command == "content-manifest":
        write_or_check_content_manifest(args.output, args.check)


if __name__ == "__main__":
    main()
