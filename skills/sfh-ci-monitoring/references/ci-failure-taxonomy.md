# CI failure taxonomy

| Class | Examples | sfh action |
|---|---|---|
| success | all required jobs succeeded | end/next phase |
| logical failure | test, lint, build, policy gate failed | capture evidence → fix loop |
| infrastructure | runner unavailable, service outage, API transport | bounded retry/fallback |
| flaky-known | registered flaky test under explicit policy | bounded rerun, record occurrence |
| cancelled-superseded | newer SHA replaced run via concurrency | stop observing this run; locate intended new run only if flow owns that transition |
| cancelled-human | user cancelled | stuck/handoff |
| action_required | approval/environment gate | stuck/human action |
| timed_out | job or watch deadline | diagnose resource/hang; do not assume product failure |
| stale/unknown | unexpected conclusion or identity mismatch | fail closed |
| auth/permission | `gh` cannot read run/logs | configuration/capability failure, not retry forever |

## Broken monitoring anti-patterns

- select latest run
- poll without expected SHA
- treat every non-success conclusion as the same
- rerun any failure automatically
- use an unbounded `while true`
- discard failed logs before diagnosis
- pass entire logs to every AI lap
- let an AI decide whether a JSON `conclusion` means success
- push fixes from a read-only monitoring step
