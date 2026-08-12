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
