# Security policy

## Supported versions

The latest v1.1.x release receives security fixes. Older releases may be used
to reproduce a report but are not maintained.

## Reporting a vulnerability

Please do not open a public issue for a vulnerability that could expose local
files, execute unintended commands, escape a declared permission boundary, or
tamper with resume/run artifacts.

Use GitHub's private vulnerability reporting from the repository's
**Security → Advisories → Report a vulnerability** page. Include:

- the affected version and operating system;
- a minimal flow and commands needed to reproduce it;
- the expected and actual trust boundary;
- whether symlinks, junctions, detached processes, or a resumed run are
  involved; and
- any proposed mitigation.

Do not include live credentials, session tokens, or private provider output.
You should receive an acknowledgement within seven days. Fix timing depends on
severity and reproducibility; coordinated disclosure is preferred.

## Boundary reminder

sfh launches tools with the current user's OS rights. Its `access` setting maps
to provider CLI flags and is not an operating-system sandbox. Escape hatches
such as `--force-resume`, `unsafe_shell_template`, `allow_access_override`, and
`allow_dynamic_exec_paths` intentionally weaken normal checks.
