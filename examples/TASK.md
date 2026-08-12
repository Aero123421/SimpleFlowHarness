# Task

Replace this file with the change you actually want made. `managed-loop.yaml`
pins it as the named context `task`, so whatever is written here is what the
`inspect`, `implement` and `fix` steps are handed — hashed, in a fixed order,
and recorded in `<step>.context.json` so that after the run you can answer
"what did the model actually see".

The placeholder below is a real, self-contained task, so the example runs as
shipped rather than only after you have edited it.

## Goal

Add a `--count` flag to the project's CLI that prints how many items the
current command would process, and exits without processing any of them.

## Acceptance

- `--count` prints a single integer on stdout and nothing else.
- `--count` exits 0 and performs no writes.
- The existing behaviour without the flag is unchanged.
- A test covers both the flag and its absence.

## Constraints

- Prefer the design that is already there; do not restructure the CLI parser.
- If you change a public interface, say why.
