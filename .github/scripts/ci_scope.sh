#!/usr/bin/env bash
set -euo pipefail

# Read NUL-delimited paths from stdin. Print true when any change can affect
# code, generated/validated artifacts, packaging, or CI itself. Unknown paths
# deliberately take the expensive path.
heavy=false
while IFS= read -r -d '' path; do
  case "$path" in
    docs/*|.github/ISSUE_TEMPLATE/*|.github/pull_request_template.md|LICENSE|LICENSE.*)
      ;;
    */*)
      heavy=true
      ;;
    README.md|README.ja.md)
      # Both READMEs carry whole flows that `cargo test` strictly validates and
      # dry-runs, so they are executable input under the same rule as skills/**
      # rather than prose. Only the Quick Start fence used to be checked, and
      # nothing ran that check on a README-only change.
      heavy=true
      ;;
    *.md)
      # Root prose only. Nested Markdown (notably skills/**) is executable or
      # validated repository input and was caught by the */* case above.
      ;;
    *)
      heavy=true
      ;;
  esac
done

printf '%s\n' "$heavy"
