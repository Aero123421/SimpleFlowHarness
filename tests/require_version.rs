use serde_json::Value;
use std::process::Command;

#[test]
fn a_version_mismatch_is_a_capability_error_before_the_run_directory_exists() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = std::env::temp_dir().join(format!(
        "sfh-require-version-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    std::fs::write(
        &flow,
        format!(
            "api_version: 1\nname: version-gate\ndefaults:\n  tool: codex\n  access: read\n  require_version: '>=9999.0.0'\nsteps:\n  - id: must_not_start\n    bin: \"{bin}\"\n    prompt: never run this\n"
        ),
    )
    .unwrap();

    let output = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "version-gate response was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(body["error"]["code"], "SFH_CAPABILITY_UNAVAILABLE");
    assert!(
        !run_dir.exists(),
        "the capability gate must run before run-dir creation"
    );

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn a_matching_version_passes_the_gate_and_allows_the_step_to_start() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = std::env::temp_dir().join(format!(
        "sfh-require-version-match-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    std::fs::write(
        &flow,
        format!(
            "api_version: 1\nname: version-match\ndefaults:\n  tool: codex\n  access: read\n  require_version: '= {}'\nsteps:\n  - id: starts\n    bin: \"{bin}\"\n    prompt: this invocation will reach the sfh fixture\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();

    let output = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_ne!(output.status.code(), Some(2));
    assert!(
        run_dir.join("starts.err.txt").exists(),
        "a matching declaration must allow the configured process to start; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn unusable_version_output_fails_closed_as_a_capability_error() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = std::env::temp_dir().join(format!(
        "sfh-require-version-unusable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    std::fs::write(
        &flow,
        "api_version: 1\ndefaults:\n  tool: codex\n  access: read\n  require_version: '>=1.0'\nsteps:\n  - id: must_not_start\n    bin: definitely-not-a-real-binary-9d3f\n    prompt: never run this\n",
    )
    .unwrap();

    let output = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["error"]["code"], "SFH_CAPABILITY_UNAVAILABLE");
    assert!(!run_dir.exists());

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn plan_reports_the_declaration_without_measuring_or_enforcing_it() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = std::env::temp_dir().join(format!(
        "sfh-plan-version-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let flow = base.join("flow.yaml");
    std::fs::write(
        &flow,
        "api_version: 1\ndefaults:\n  tool: codex\n  access: read\n  require_version: '>=9999.0.0'\nsteps:\n  - id: planned\n    prompt: x\n",
    )
    .unwrap();

    let output = Command::new(sfh)
        .args(["plan", flow.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["required_versions"][0]["requirement"], ">=9999.0.0");
    assert!(body["required_versions"][0]["observed"].is_null());

    let _ = std::fs::remove_dir_all(base);
}
