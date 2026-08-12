# Release-readiness review

Review only release blockers:

- version/tag/changelog/schema agreement;
- deterministic build, test, lint, packaging, and installer evidence;
- migration and compatibility notes;
- public examples and machine contracts;
- security support policy and artifact provenance;
- whether the release can be reproduced from a clean checkout.

Do not turn the release into a feature project. Non-blocking refactors belong after release.

If the release is ready, end with exactly:
REVIEW: PASS

Otherwise end with exactly:
REVIEW: REVISE
