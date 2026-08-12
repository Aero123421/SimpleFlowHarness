#!/usr/bin/env python3
"""Structural checks for the bundled Agent Skills and their version claims.

`sfh validate` covers every YAML in skills/ and examples/ponytail/. It cannot
see the things around the YAML: whether a SKILL.md points at a reference file
that exists, whether the catalog still describes the directories on disk, or
whether a pack that says "written for sfh 1.4.x" has been carried into a 1.6
release without anyone rereading it. Those are what this file checks.

Standard library only, like the rest of tests/ - CI runs it on three OSes with
whatever Python the runner ships.
"""

from __future__ import annotations

import json
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SKILLS = ROOT / "skills"
CATALOG = SKILLS / "skill-catalog.json"

# A skill's own resources, referenced from its SKILL.md body.
REFERENCE_RE = re.compile(
    r"(?<![A-Za-z0-9_./-])((?:references|scripts|assets)/[A-Za-z0-9_.\-/]+)"
)
# The `# yaml-language-server: $schema=.../v1.6.0/schema/...` header every
# example flow carries.
SCHEMA_PIN_RE = re.compile(
    r"SimpleFlowHarness/v(\d+)\.(\d+)\.(\d+)/schema/flow\.schema\.json"
)
FRONTMATTER_FIELD_RE = re.compile(r"^([a-z][a-z0-9-]*):\s*(.*)$")

MAX_SKILL_LINES = 500


def crate_version() -> tuple[int, int]:
    """The (major, minor) of the crate this checkout would publish."""
    for line in (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r'version = "(\d+)\.(\d+)\.\d+"', line.strip())
        if match:
            return int(match.group(1)), int(match.group(2))
    raise AssertionError("no version found in Cargo.toml")


def skill_dirs() -> list[Path]:
    return sorted(p for p in SKILLS.glob("sfh-*") if p.is_dir())


def frontmatter(path: Path) -> tuple[dict[str, str], str]:
    """Split a SKILL.md into its top-level frontmatter fields and its body.

    Deliberately not a YAML parser: only the top-level scalar fields are read,
    which is all any check here needs, and it keeps this file dependency-free.
    """
    text = path.read_text(encoding="utf-8")
    if not text.startswith("---\n"):
        raise AssertionError(f"{path}: no opening frontmatter delimiter")
    end = text.find("\n---\n", 4)
    if end < 0:
        raise AssertionError(f"{path}: no closing frontmatter delimiter")
    fields: dict[str, str] = {}
    lines = text[4:end].splitlines()
    index = 0
    while index < len(lines):
        line = lines[index]
        index += 1
        if line.startswith((" ", "\t")) or not line.strip():
            continue  # nested mapping entry or blank
        match = FRONTMATTER_FIELD_RE.match(line)
        if not match:
            continue
        key, value = match.group(1), match.group(2).strip()
        if value in {">", ">-", "|", "|-"}:
            # A block scalar: every description in this pack is written this
            # way. Collect the indented continuation before the next key.
            block: list[str] = []
            while index < len(lines) and (
                not lines[index].strip() or lines[index].startswith((" ", "\t"))
            ):
                block.append(lines[index].strip())
                index += 1
            if value.startswith("|"):
                value = "\n".join(block)
            else:
                # Folded: blank lines are paragraph breaks, the rest join with
                # a space.
                folded: list[str] = []
                for entry in block:
                    if not entry:
                        folded.append("\n")
                    elif folded and folded[-1] != "\n":
                        folded[-1] = f"{folded[-1]} {entry}"
                    else:
                        folded.append(entry)
                value = "".join(folded) if folded else ""
        fields[key] = value
    return fields, text[end + 5 :]


class SkillStructure(unittest.TestCase):
    def test_nine_skills_are_present(self) -> None:
        self.assertEqual(len(skill_dirs()), 9, [p.name for p in skill_dirs()])

    def test_frontmatter_name_matches_directory(self) -> None:
        for directory in skill_dirs():
            with self.subTest(skill=directory.name):
                fields, _ = frontmatter(directory / "SKILL.md")
                self.assertEqual(fields.get("name"), directory.name)
                self.assertTrue(fields.get("description"), "empty description")

    def test_skill_bodies_stay_under_the_disclosure_limit(self) -> None:
        # A skill is loaded in full once activated; the detail belongs in
        # references/ that the model reads only when it needs them.
        for directory in skill_dirs():
            with self.subTest(skill=directory.name):
                lines = (directory / "SKILL.md").read_text(encoding="utf-8").count("\n") + 1
                self.assertLessEqual(lines, MAX_SKILL_LINES, f"{lines} lines")

    def test_referenced_resources_exist(self) -> None:
        for directory in skill_dirs():
            _, body = frontmatter(directory / "SKILL.md")
            for reference in sorted(set(REFERENCE_RE.findall(body))):
                with self.subTest(skill=directory.name, reference=reference):
                    self.assertNotIn("..", Path(reference).parts)
                    self.assertTrue((directory / reference).is_file())


class SkillCatalog(unittest.TestCase):
    def test_catalog_matches_the_directories_on_disk(self) -> None:
        catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
        listed = {entry["name"] for entry in catalog["skills"]}
        self.assertEqual(listed, {p.name for p in skill_dirs()})

    def test_catalog_paths_resolve_from_the_repository_root(self) -> None:
        catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
        for entry in catalog["skills"]:
            with self.subTest(skill=entry["name"]):
                self.assertTrue((ROOT / entry["path"]).is_file(), entry["path"])

    def test_catalog_descriptions_match_the_skill_frontmatter(self) -> None:
        # The catalog is what an Agent Skills client reads to decide whether to
        # load a skill at all. A description that has drifted from SKILL.md
        # means the trigger surface and the instructions disagree.
        catalog = json.loads(CATALOG.read_text(encoding="utf-8"))
        for entry in catalog["skills"]:
            with self.subTest(skill=entry["name"]):
                fields, _ = frontmatter(ROOT / entry["path"])
                self.assertEqual(entry["description"].strip(), fields["description"].strip())


class VersionClaims(unittest.TestCase):
    """The pack states which sfh it was written for. Keep that honest.

    Both packs arrived declaring `target-sfh: "1.4.x"` and pinning the v1.4.0
    schema, and were nearly released inside 1.5.1 still saying so. A minor bump
    should fail here until someone has actually reread the skills against the
    new behaviour - that is the point, not an inconvenience.
    """

    def test_skills_target_the_current_minor_series(self) -> None:
        major, minor = crate_version()
        expected = f'"{major}.{minor}.x"'
        for directory in skill_dirs():
            with self.subTest(skill=directory.name):
                text = (directory / "SKILL.md").read_text(encoding="utf-8")
                match = re.search(r"target-sfh:\s*(\S+)", text)
                self.assertIsNotNone(match, "no target-sfh in frontmatter metadata")
                self.assertEqual(match.group(1), expected)

    def test_schema_pins_track_the_current_minor_series(self) -> None:
        major, minor = crate_version()
        pinned = []
        for path in sorted(ROOT.glob("examples/**/*.yaml")) + sorted(SKILLS.glob("**/*.yaml")):
            for found in SCHEMA_PIN_RE.finditer(path.read_text(encoding="utf-8")):
                pinned.append((path.relative_to(ROOT), found.group(0)))
        self.assertTrue(pinned, "no flow pins the published schema at all")
        for relative, url in pinned:
            with self.subTest(flow=str(relative)):
                found = SCHEMA_PIN_RE.search(url)
                assert found is not None
                self.assertEqual((int(found.group(1)), int(found.group(2))), (major, minor))


UNBOUNDED_CYCLE = """api_version: 1
name: unbounded
workspace: {mode: current}
steps:
  - id: build
    effects: read
    cmd: ["true"]
  - id: check
    effects: read
    cmd: ["true"]
    route:
      - {when_exit: 0, goto: end}
      - {goto: build}
"""

BOUNDED_AT_THE_SOURCE = """api_version: 1
name: bounded-at-source
workspace: {mode: current}
steps:
  - id: build
    effects: read
    cmd: ["true"]
  - id: check
    effects: read
    cmd: ["true"]
    max_visits: 3
    on_max_visits: goto:stuck
    route:
      - {when_exit: 0, goto: end}
      - {goto: build}
"""


def _has_pyyaml() -> bool:
    try:
        import yaml  # noqa: F401
    except ImportError:
        return False
    return True


@unittest.skipUnless(_has_pyyaml(), "the bundled linter needs PyYAML")
class LinterRules(unittest.TestCase):
    """Regression checks for the high-value heuristic rules.

    SFH042/SFH043 used to read the wrong node of the cycle.
    The rule checked whether the step a backward route points AT carries
    `max_visits`. A review/fix loop puts the bound on the fixer that jumps
    back - the step whose repeated failure is what you would give up on - so
    the rule fired on 17 of the 20 bundled flows while every one of them was
    properly bounded. A linter that is wrong that often on its own reference
    examples teaches authors to ignore it.
    """

    def lint(self, flow: str) -> str:
        import subprocess
        import sys
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "flow.yaml"
            path.write_text(flow, encoding="utf-8")
            proc = subprocess.run(
                [sys.executable, str(SKILLS / "tools/lint_sfh_flow.py"), str(path)],
                capture_output=True,
                text=True,
            )
            return proc.stdout

    def test_a_cycle_with_no_bound_anywhere_is_still_reported(self) -> None:
        output = self.lint(UNBOUNDED_CYCLE)
        self.assertIn("SFH042", output)
        self.assertIn("SFH043", output)

    def test_a_bound_on_the_step_that_jumps_back_counts(self) -> None:
        output = self.lint(BOUNDED_AT_THE_SOURCE)
        self.assertNotIn("SFH042", output)
        self.assertNotIn("SFH043", output)

    def test_remote_reads_are_read_but_known_remote_mutations_are_external(self) -> None:
        remote_read = """api_version: 1
name: remote-read
workspace: {mode: current}
steps:
  - {id: query, cmd: [web-search-cli, search, topic], effects: read}
"""
        remote_write = """api_version: 1
name: remote-write
workspace: {mode: current}
steps:
  - {id: create, cmd: [gh, issue, create, --title, test], effects: read}
"""
        self.assertNotIn("SFH033", self.lint(remote_read))
        self.assertIn("SFH033", self.lint(remote_write))

    def test_the_bundled_flows_are_clean(self) -> None:
        # The pack's own README makes this linter step 6 of the standard
        # authoring procedure, so its reference flows have to pass it.
        flows = sorted(ROOT.glob("examples/ponytail/[0-9][0-9]-*.yaml"))
        flows += sorted(SKILLS.glob("sfh-*/assets/*.yaml"))
        self.assertTrue(flows)
        for flow in flows:
            with self.subTest(flow=flow.name):
                output = self.lint(flow.read_text(encoding="utf-8"))
                offenders = [
                    line for line in output.splitlines() if line.startswith(("ERROR", "WARNING"))
                ]
                self.assertEqual(offenders, [])


class BundledCopies(unittest.TestCase):
    def test_the_two_linter_copies_stay_identical(self) -> None:
        # sfh-flow-design ships its own copy so the skill is self-contained
        # when installed alone; skills/tools/ holds the copy a maintainer runs
        # from the repository root. One of them silently going stale would mean
        # the skill teaches a lint the repository no longer performs.
        a = (SKILLS / "tools/lint_sfh_flow.py").read_bytes()
        b = (SKILLS / "sfh-flow-design/scripts/lint_sfh_flow.py").read_bytes()
        self.assertEqual(a, b)


if __name__ == "__main__":
    unittest.main()
