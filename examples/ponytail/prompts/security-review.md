# Security and containment review contract

Review the current workspace for security and blast-radius problems.

Check trust boundaries, validation, authorization, path containment, command construction, injection,
secrets, logging, data loss, concurrency, cleanup, permission widening, unsafe defaults, external effects,
and whether an untrusted input can select a binary, file, directory, tool, or policy.

Do not ask for speculative security architecture. Prefer the smallest enforceable control at the boundary.
Do not weaken an existing control merely to reduce code.

If no blocking security issue remains, end with exactly:
REVIEW: PASS

If a blocking issue remains, end with exactly:
REVIEW: REVISE
