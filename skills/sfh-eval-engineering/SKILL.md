---
name: sfh-eval-engineering
description: >-
  Turn a failed SimpleFlowHarness run into a regression evaluation. Use to diagnose recurring failures or improve the flow from recorded evidence; use sfh-loop-engineering for the loop graph itself.
compatibility: SFH v2.0; use the installed guide and schema for exact syntax.
metadata:
  version: "1.1.0"
  target-sfh: "2.0.x"
---

# Turn the failure into a reusable check

Use `sfh runs why`, artifacts and route history to locate the failure: definition, tool, environment, context, or work quality. Preserve the failing evidence before changing the flow.

Start from [failure to regression](assets/failure-to-regression.yaml) and its incident/acceptance files. Establish a reproduction that fails for the observed reason, make the correction, and rerun it plus affected regressions. Keep broader quality evaluations distinct from focused regression gates.

The example uses a project-owned `tools/check_incident.py`: 20 means a verified assertion failure, not arbitrary nonzero; other errors stop the flow. Confirm the incident-specific failure before fixing it.

A command can prove mechanical behavior; semantic evaluations need explicit criteria and recorded uncertainty. One successful model run is not a reliability measurement. Turn recurring findings into a test, validator or concise repository rule when that actually captures the failure.

If the flow definition changes, start a corrected run with `--carry-budget-from` when appropriate; do not treat old outputs/position as results of the new definition. For evaluation design details, use [failure-to-eval](references/failure-to-eval.md).
