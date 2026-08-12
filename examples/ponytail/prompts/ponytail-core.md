# Ponytail-inspired implementation discipline

This is a portable, paraphrased working contract inspired by Dietrich Gebert's Ponytail project.
It is not a vendored copy of the upstream Skill.

Apply these rules throughout the step:

1. Understand before minimizing. Trace the actual execution path, callers, contracts, and existing patterns before editing.
2. Ask whether new code is necessary. If the repository already satisfies the requirement, do not create a ceremonial change.
3. Prefer, in order:
   - an existing helper or pattern in this repository;
   - the language standard library;
   - a native platform, browser, database, or operating-system capability;
   - a dependency that is already installed;
   - only then, the smallest local implementation that works.
4. Prefer deletion and reuse over new layers. Avoid one-implementation interfaces, one-product factories, speculative configuration, wrappers that only delegate, and scaffolding for an imagined future.
5. Touch the fewest files that correctly solve the root problem. A tiny patch in the wrong layer is not minimal; it is a second bug.
6. For a bug, fix the shared root cause after inspecting sibling callers. Do not patch only the path named by the report when the same defect remains elsewhere.
7. Do not simplify away trust-boundary validation, data-loss prevention, security controls, required accessibility, error handling, compatibility, or an explicit requirement.
8. Non-trivial new logic must leave the smallest useful runnable regression check behind. Do not build a test framework for one check.
9. Do not add a dependency for a few clear lines unless the dependency materially improves correctness or removes owned complexity.
10. When choosing a deliberately simple solution with a known ceiling, state the ceiling and the concrete signal that would justify upgrading it.

The desired result is the smallest correct, observable, maintainable change—not merely the fewest characters.
