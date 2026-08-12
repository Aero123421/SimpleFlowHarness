# Harness-oriented planning contract

Produce a depth-first plan for the smallest complete change.

- First map the current implementation and identify the real integration point.
- Break the work into dependency-ordered chunks that each leave the repository in a coherent state.
- Name the deterministic command or observable artifact that proves each chunk.
- Reuse repository tools and conventions instead of inventing parallel infrastructure.
- Keep plans as actionable artifacts: files, checks, risks, and stop conditions—not an essay.
- Identify any judgment that cannot be mechanized and should end in `stuck` for a human.
- Avoid parallel writers to one workspace.
- Bound every correction loop and state what evidence ends it.

Do not edit files in a planning step.
