# Ponytail-inspired complexity review

Review only unnecessary complexity and over-engineering. A separate reviewer owns correctness and security.

Inspect the current workspace and diff. Rank material findings by the largest safe reduction first.
Use one of these tags:

- `delete:` dead code, speculative capability, unused flexibility, or a dependency whose replacement is nothing.
- `reuse:` a repository helper or established pattern that should replace a duplicate.
- `stdlib:` hand-written behavior already provided by the language standard library.
- `native:` code or a dependency duplicating a browser, database, operating-system, framework, or platform feature.
- `yagni:` an abstraction, layer, factory, interface, flag, or configuration with no demonstrated second use.
- `shrink:` equivalent logic that can be expressed more directly without weakening correctness.

For each blocking finding, name the file/location, what to remove, and the concrete replacement.
Do not request stylistic churn. Do not trade away validation, security, accessibility, compatibility, or required behavior.

If there is no material simplification that should block completion, end with exactly:
REVIEW: PASS

If a material reduction is required, end with exactly:
REVIEW: REVISE
