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
fn validation_and_run_browsing_use_the_same_error_envelope() {
    for (args, command, code) in [
        (vec!["validate", "--json"], "validate", "SFH_USAGE"),
        (vec!["runs", "why", "--json"], "runs why", "SFH_USAGE"),
        (
            vec!["runs", "why", "missing-run-9d3f", "--json"],
            "runs why",
            "SFH_PERSISTENCE_FAILURE",
        ),
        (
            vec!["runs", "why", "missing-run-9d3f", "--json", "--verbose"],
            "runs why",
            "SFH_USAGE",
        ),
        (
            vec!["runs", "clean", "--keep", "bad", "--json"],
            "runs clean",
            "SFH_USAGE",
        ),
    ] {
        let output = sfh(&args);
        assert_eq!(output.status.code(), Some(2), "args: {args:?}");
        let json = stdout_json(&output);
        assert_eq!(json["ok"], false, "args: {args:?}");
        assert_eq!(json["schema_version"], 1, "{json}");
        assert_eq!(json["command"], command, "{json}");
        assert_eq!(json["exit_code"], 2, "{json}");
        assert_eq!(json["error"]["code"], code, "{json}");
    }
}

#[test]
fn run_browsing_and_cleanup_return_envelopes_for_an_empty_root() {
    let root = std::env::temp_dir().join(format!("sfh-empty-machine-root-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    for sub in ["list", "clean"] {
        let output = sfh(&["runs", sub, "--runs-dir", root.to_str().unwrap(), "--json"]);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        let json = stdout_json(&output);
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["command"], format!("runs {sub}"));
        assert_eq!(json["ok"], true);
        assert_eq!(json["exit_code"], 0);
        assert!(json["error"].is_null());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn release_binary_emits_the_exact_embedded_resource_manifest() {
    let output = sfh(&["__release-manifest"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        output.stdout,
        include_bytes!("../release-content-manifest.txt")
    );
}

#[test]
fn inspecting_a_failed_run_does_not_overwrite_the_response_header() {
    let root = std::env::temp_dir().join(format!(
        "sfh-machine-fields-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let flow = root.join("flow.yaml");
    let run = root.join("run");
    let bin = serde_json::to_string(env!("CARGO_BIN_EXE_sfh")).unwrap();
    std::fs::write(
        &flow,
        format!("api_version: 1\nsteps:\n  - id: work\n    cmd: [{bin}, not-a-command]\n"),
    )
    .unwrap();
    let validation = sfh(&["validate", flow.to_str().unwrap(), "--json"]);
    assert_eq!(validation.status.code(), Some(0));
    assert_eq!(stdout_json(&validation)["command"], "validate");
    let output = sfh(&[
        "run",
        flow.to_str().unwrap(),
        "--run-dir",
        run.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let why = sfh(&["runs", "why", run.to_str().unwrap(), "--json"]);
    assert_eq!(why.status.code(), Some(0));
    let why = stdout_json(&why);
    assert_eq!(why["ok"], true);
    assert!(why["error"].is_null());
    assert!(why["run_error"].is_string(), "{why}");
    let show = sfh(&["runs", "show", run.to_str().unwrap(), "--json"]);
    assert_eq!(show.status.code(), Some(0));
    let show = stdout_json(&show);
    assert_eq!(show["ok"], true);
    assert_eq!(show["successful_steps"], 0);
    assert_eq!(show["failed"], 1);
    assert_eq!(show["sfh_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(show["run_sfh_version"], env!("CARGO_PKG_VERSION"));
    std::fs::write(&flow, "steps: not-an-array\n").unwrap();
    let invalid = sfh(&["validate", flow.to_str().unwrap(), "--json"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(stdout_json(&invalid)["error"]["code"], "SFH_FLOW_INVALID");
    std::fs::remove_dir_all(root).unwrap();
}
