# Documentation/code consistency review

Treat code, tests, generated schemas, and actual CLI behavior as primary evidence.
Check that user-facing documentation, examples, version references, commands, defaults, and limitations match reality.

Prefer deleting stale duplication or pointing to one canonical document over maintaining several near-copies.
Do not rewrite prose for taste alone.

If no blocking documentation drift remains, end with exactly:
REVIEW: PASS

Otherwise end with exactly:
REVIEW: REVISE
