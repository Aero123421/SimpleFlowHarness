# Support

Use a GitHub issue for a reproducible sfh defect. For a usage question, open an
issue only after checking the English/Japanese README and `sfh guide`; clearly
label it as a question so it can be distinguished from a defect.

Before reporting a problem, collect:

```bash
sfh --version
sfh validate flow.yaml --strict
sfh plan flow.yaml
sfh runs why RUN_DIR --json
```

Also include the operating system, the relevant provider CLI version, the
smallest flow that reproduces the problem, and sanitized stderr. Never attach
credentials or an entire private run directory.

Provider outages, billing disputes, model quality, and provider CLI features
outside sfh's command construction/parsing boundary must be handled with that
provider.
