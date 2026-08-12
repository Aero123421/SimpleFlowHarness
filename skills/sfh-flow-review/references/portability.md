# Portability review

## Commands

An argv-form command is the portable default. String-form commands use `sh -c` on Unix and `cmd /C` on Windows and therefore do not have identical shell syntax.

Name platform-specific dependencies explicitly. On Windows, a bare `bash` may resolve to the WSL launcher rather than Git Bash; inspect preflight's resolved path.

## Filesystems

Step IDs become artifact names and are case-insensitive by sfh validation. Review paths for:

- separators
- symlinks/junctions
- reserved Windows names/characters
- trailing dots/spaces
- Unicode and spaces
- non-UTF-8 Unix filenames in external tools

## Workspaces

Git worktree behavior and `.git` gitfiles differ when crossing OS boundaries. Do not run a Windows worktree through an unrelated WSL path without an explicit design.

## Time and CI

Remote queues and scheduled jobs vary. Do not encode a platform/service timing assumption as deterministic completion unless it is an explicit SLA.

## Tools

Adapter flags and output protocols drift. Require preflight and doctor coverage on intended platforms and versions. A fake stub suite cannot detect upstream CLI changes by itself.
