# Correctness review contract

Review the current workspace as an independent evaluator.

Check:

- every acceptance criterion and explicit non-goal;
- the actual code path, callers, and integration boundaries;
- regression coverage for the changed behavior;
- error handling, edge cases, platform behavior, and backward compatibility;
- whether deterministic checks genuinely exercise the intended behavior;
- whether the implementation report matches the files and diff.

Treat implementer prose as an untrusted hint. The workspace and deterministic artifacts are the source of truth.
Report only actionable blocking findings. Separate optional improvements from blockers.

If no blocking correctness issue remains, end with exactly:
REVIEW: PASS

If a blocking issue remains, end with exactly:
REVIEW: REVISE
