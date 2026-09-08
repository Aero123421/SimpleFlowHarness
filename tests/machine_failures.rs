use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn parse(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "machine response was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn watch(sfh: &str, command: &str, run_dir: &Path) -> (Output, Value) {
    let output = Command::new(sfh)
        .arg(command)
        .arg(run_dir)
        .arg("--json")
        .output()
        .unwrap();
    let body = parse(&output);
    (output, body)
}

fn run_json(sfh: &str, base: &Path, flow_text: &str) -> (Output, Value) {
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    std::fs::write(&flow, flow_text).unwrap();
    let output = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    let body = parse(&output);
    (output, body)
}

fn assert_watch_code(sfh: &str, run_dir: &Path, expected_code: &str) {
    for command in ["status", "wait"] {
        let (output, body) = watch(sfh, command, run_dir);
        assert_eq!(output.status.code(), Some(1), "{command}: {body}");
        assert_eq!(body["error"]["code"], expected_code, "{command}: {body}");
    }
}

fn temp_dir(label: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!(
        "sfh-machine-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn protocol_failure_keeps_the_same_stable_code_across_run_status_and_wait() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = temp_dir("protocol-code");
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    std::fs::write(
        &flow,
        format!(
            "api_version: 1\nsteps:\n  - id: drifted\n    tool: codex\n    bin: \"{bin}\"\n    access: read\n    prompt: this fixture does not speak codex JSONL\n"
        ),
    )
    .unwrap();

    let run = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(1));
    let run_body = parse(&run);
    let code = run_body["error"]["code"].as_str().unwrap();
    assert!(
        ["SFH_PROTOCOL_INVALID", "SFH_TERMINAL_MISSING"].contains(&code),
        "unexpected protocol classification: {run_body}"
    );

    for command in ["status", "wait"] {
        let (output, body) = watch(sfh, command, &run_dir);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(body["error"]["code"], code, "{command}: {body}");
        assert!(body["error"].is_object(), "{command}: {body}");
    }

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn an_ordinary_step_failure_is_classified_as_step_failed_everywhere() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = temp_dir("step-failed-code");
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    // sfh itself is the portable non-zero exit: an unknown flag is a usage
    // error on every OS, with no shell quoting to get wrong.
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    std::fs::write(
        &flow,
        format!("api_version: 1\nsteps:\n  - id: boom\n    cmd: [\"{bin}\", --not-a-real-flag]\n"),
    )
    .unwrap();

    let run = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(1));
    let run_body = parse(&run);
    assert_eq!(
        run_body["error"]["code"], "SFH_STEP_FAILED",
        "a valid flow whose step exited non-zero is not a static authoring error: {run_body}"
    );

    for command in ["status", "wait"] {
        let (output, body) = watch(sfh, command, &run_dir);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            body["error"]["code"], "SFH_STEP_FAILED",
            "{command}: {body}"
        );
    }

    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn runtime_step_failure_ignores_error_code_words_in_step_id_and_run_path() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = temp_dir("SFH_PROTOCOL_INVALID");
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    std::fs::write(
        &flow,
        format!(
            "api_version: 1\nsteps:\n  - id: SFH_PROTOCOL_INVALID\n    cmd: [\"{bin}\", --not-a-real-flag]\n"
        ),
    )
    .unwrap();

    let run = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(1));
    let run_body = parse(&run);
    assert_eq!(run_body["error"]["code"], "SFH_STEP_FAILED", "{run_body}");
    assert!(run_body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("SFH_PROTOCOL_INVALID"));

    assert_watch_code(sfh, &run_dir, "SFH_STEP_FAILED");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn static_invalid_code_words_do_not_select_runtime_or_persistence_codes() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    for id in ["SFH_STEP_FAILED", "persisted"] {
        let base = temp_dir(&format!("static-code-word-{id}"));
        let flow = format!(
            "api_version: 1\nsteps:\n  - id: {id}\n    cmd: [\"{bin}\", --version]\n    on_error: definitely-not-an-action\n"
        );
        let (output, body) = run_json(sfh, &base, &flow);
        assert_eq!(output.status.code(), Some(2), "{id}: {body}");
        assert_eq!(body["error"]["code"], "SFH_FLOW_INVALID", "{id}: {body}");
        let _ = std::fs::remove_dir_all(base);
    }
}

#[test]
fn runtime_foreach_limits_and_input_failures_are_step_failed() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");

    let base = temp_dir("foreach-over-limit-code");
    let items = (0..101)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let indented = items
        .lines()
        .map(|line| format!("        {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let flow = format!(
        "api_version: 1\nname: foreach-over-limit\nsteps:\n  - id: each\n    foreach:\n      from: |\n{indented}\n    cmd: [\"{bin}\", \"--version\"]\n"
    );
    let (output, body) = run_json(sfh, &base, &flow);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(body["error"]["code"], "SFH_STEP_FAILED", "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("foreach produced 101 items"));
    assert_watch_code(sfh, &base.join("run"), "SFH_STEP_FAILED");
    let _ = std::fs::remove_dir_all(base);

    let base = temp_dir("foreach-input-code");
    let flow = format!(
        r#"api_version: 1
name: foreach-input
steps:
  - id: source
    cmd: ["{bin}", "--version"]
  - id: each
    foreach:
      from: "{{{{steps.source.output}}}}"
      split: json
    cmd: ["{bin}", "--version"]
"#
    );
    let (output, body) = run_json(sfh, &base, &flow);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(body["error"]["code"], "SFH_STEP_FAILED", "{body}");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no JSON array found"));
    assert_watch_code(sfh, &base.join("run"), "SFH_STEP_FAILED");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn runtime_leaf_budget_limits_are_step_failed_for_foreach_and_parallel() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    for (label, flow) in [
        (
            "foreach",
            format!(
                r#"api_version: 1
name: foreach-budget
defaults:
  max_total_steps: 1
steps:
  - id: each
    foreach: {{from: "a\nb"}}
    cmd: ["{bin}", "--version"]
"#
            ),
        ),
        (
            "parallel",
            format!(
                r#"api_version: 1
name: parallel-budget
defaults:
  max_total_steps: 1
steps:
  - id: fan
    parallel:
      - id: left
        cmd: ["{bin}", "--version"]
      - id: right
        cmd: ["{bin}", "--version"]
"#
            ),
        ),
    ] {
        let base = temp_dir(&format!("leaf-budget-{label}"));
        let (output, body) = run_json(sfh, &base, &flow);
        assert_eq!(output.status.code(), Some(1), "{label}: {body}");
        assert_eq!(body["error"]["code"], "SFH_STEP_FAILED", "{label}: {body}");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("max_total_steps"));
        assert_watch_code(sfh, &base.join("run"), "SFH_STEP_FAILED");
        let _ = std::fs::remove_dir_all(base);
    }
}

#[test]
fn static_invalid_flow_without_runtime_marker_stays_flow_invalid() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    let base = temp_dir("static-invalid-control");
    let flow = format!(
        r#"api_version: 1
name: static-invalid
steps:
  - id: bad
    cmd: ["{bin}", "--version"]
    on_error: definitely-not-an-action
"#
    );
    let (output, body) = run_json(sfh, &base, &flow);
    assert_eq!(output.status.code(), Some(2), "{body}");
    assert_eq!(body["error"]["code"], "SFH_FLOW_INVALID", "{body}");
    let _ = std::fs::remove_dir_all(base);
}

#[test]
fn max_visits_stuck_is_classified_as_stuck_everywhere() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = temp_dir("stuck-code");
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    let bin = sfh.replace('\\', "/").replace('"', "\\\"");
    std::fs::write(
        &flow,
        format!(
            "api_version: 1\nsteps:\n  - id: loop\n    cmd: [\"{bin}\", --version]\n    max_visits: 1\n    on_max_visits: goto:stuck\n    route: [{{goto: loop}}]\n"
        ),
    )
    .unwrap();

    let run = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(run.status.code(), Some(4));
    assert_eq!(parse(&run)["error"]["code"], "SFH_STUCK");

    for command in ["status", "wait"] {
        let (output, body) = watch(sfh, command, &run_dir);
        assert_eq!(output.status.code(), Some(4));
        assert_eq!(body["error"]["code"], "SFH_STUCK", "{command}: {body}");
    }

    let _ = std::fs::remove_dir_all(base);
}
