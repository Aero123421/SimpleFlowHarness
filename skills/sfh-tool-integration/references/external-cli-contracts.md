# External CLI contracts

For every command, specify:

```text
program identity
arguments and input encoding
working directory
environment variables removed/set
timeout
side-effect class
safe retry class
output schema
artifact retention
credential source
expected version/capability
```

## Command form

Prefer argv arrays:

```yaml
cmd: ["program", "--query", "{{vars.query}}"]
```

Avoid string shell commands. When a shell is necessary, pass untrusted values as positional arguments rather than interpolating them into script text.

## `preflight`

Use preflight to resolve command names and adapter flags, but remember that a custom or overridden binary may itself be untrusted. Review exact resolved paths.

## Output normalization

A robust wrapper separates:

- stdout result data
- stderr diagnostics/progress
- exit code outcome class
- raw provider response file

Suggested stable classes:

```text
0 success
10 retryable acquisition/transport failure
20 valid negative/domain result
30 malformed/identity mismatch
40 bounded timeout/pending
```

Document and test the mapping before using it in `outcomes`.

## Secrets

sfh v1.6 `--var` is not a secret channel. Use the external tool's credential store, environment injection outside the flow, or a secret manager. Do not print environment values or authorization headers.
