# sfh flow review

## Verdict

`PASS | REVISE | BLOCKED`

## Scope and version

- sfh target:
- flow files:
- overlays/contexts:
- commands run:
- commands not run:

## Findings

### [blocker/high/medium/low] ID — title

**Evidence**

**Failure scenario**

**Required fix**

**Regression proof**

## State-machine summary

```text
...
```

## Deterministic versus nondeterministic steps

| Step | Class | Evidence | Side effect | Replay |
|---|---|---|---|---|

## Loop termination

- visit ceiling:
- total-step ceiling:
- wall/cost ceiling:
- handoff/stuck path:

## External trust boundaries

- CI/web/API/MCP:
- credentials:
- exact IDs:
- output validation:

## Corrected YAML or focused patch

```yaml
...
```

## Validation commands

```bash
python tools/lint_sfh_flow.py flow.yaml
sfh validate flow.yaml --strict
sfh preflight flow.yaml --json
sfh plan flow.yaml --json --save
```
