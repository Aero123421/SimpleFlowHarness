use serde_json::Value;
use std::process::{Command, Output};

fn sfh(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_sfh"))
        .args(args)
        .output()
        .expect("sfh must start")
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be JSON ({error}): stdout={:?}, stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn assert_usage_envelope(args: &[&str], command: &str) {
    let output = sfh(args);
    assert_eq!(output.status.code(), Some(2), "args: {args:?}");
    let json = stdout_json(&output);
    assert_eq!(json["schema_version"], 1, "args: {args:?}");
    assert_eq!(json["command"], command, "args: {args:?}");
    assert_eq!(json["ok"], false, "args: {args:?}");
    assert_eq!(json["error"]["code"], "SFH_USAGE", "args: {args:?}");
}

#[test]
fn envelope_commands_keep_usage_errors_on_stdout_in_json_mode() {
    assert_usage_envelope(&["run", "--json"], "run");
    assert_usage_envelope(&["run", "flow.yaml", "--json", "--verbose"], "run");
    assert_usage_envelope(&["plan", "--json"], "plan");
    assert_usage_envelope(
        &["preflight", "--profiles", "overlay.yaml", "--json"],
        "preflight",
    );
    assert_usage_envelope(&["status", "--json", "--verbose"], "status");
    assert_usage_envelope(&["workspaces", "show", "--json"], "workspaces show");
    assert_usage_envelope(&["not-a-command", "--json"], "not-a-command");
}

#[test]
fn legacy_bare_json_commands_still_return_json_for_early_errors() {
    for args in [
        vec!["validate", "--json"],
        vec!["runs", "why", "--json"],
        vec!["runs", "why", "missing-run-9d3f", "--json"],
        vec!["runs", "why", "missing-run-9d3f", "--json", "--verbose"],
    ] {
        let output = sfh(&args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        let json = stdout_json(&output);
        assert_eq!(json["ok"], false, "args: {args:?}");
        assert!(json["error"].is_string(), "args: {args:?}, json: {json}");
    }
}
