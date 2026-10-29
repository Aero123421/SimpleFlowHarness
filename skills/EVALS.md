# Evaluate selection and authoring separately

`evals/trigger-cases.json` contains selection prompts. Its skill lists are relevance labels, not instructions to load every specialist. Record selected skills and irrelevant activations; a negative case should not activate SFH authoring skills. Repeat comparisons under the same model/settings when authorized. Do not infer reliability from one result.

For authoring, give a fresh evaluator the candidate skill directories, the installed SFH guide/schema, and a natural task. Allow it to consult bundled references as needed. Do not supply a reference solution, intended YAML fields or previous failure analysis. Keep its first result before feedback.

Useful tasks cover an ordinary workflow, a bounded evaluator/correction loop, and recovery after interruption with uncertain effects. Assess the produced artifacts independently:

- Schema-valid YAML and successful strict validation/dry-run.
- Acceptance evidence and rejection paths, including unknown verdicts.
- Bounded repeated work and a durable exhaustion handoff.
- Workspace/context ownership, actual schema fields and adapter capabilities.
- Recovery that preserves evidence and does not claim exactly-once effects from a replay setting.

Use disposable command fixtures for route and interruption tests. Keep authored correctness, runtime defects and provider-authenticated behavior separate. No paid model or mutating remote operation is needed for structural/offline checks. Record those scopes explicitly.

Shorter text is a maintenance/context metric, not proof of better selection or authoring. Compare task results before making a performance claim.
