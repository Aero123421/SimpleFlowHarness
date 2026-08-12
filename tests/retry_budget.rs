use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("sfh-retry-budget-{}-{nanos}", std::process::id()))
}

fn only_run_dir(runs: &Path) -> PathBuf {
    std::fs::read_dir(runs)
        .expect("runs directory exists")
        .filter_map(Result::ok)
        .find(|entry| entry.path().join("log.jsonl").is_file())
        .expect("one run was recorded")
        .path()
}

#[test]
fn retry_backoff_lands_before_wall_clock_failure_and_attempts_are_visible() {
    let root = temp_dir();
    std::fs::create_dir_all(&root).unwrap();
    let flow = root.join("flow.yaml");
    let runs = root.join("runs");
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let quoted_sfh = sfh.replace('\'', "''");
    std::fs::write(
        &flow,
        format!(
            r#"api_version: 1
defaults:
  wall_clock_sec: 4
  on_budget: goto:wrap
  budget_reserve: {{wall_clock_sec: 3}}
steps:
  - id: retrying
    cmd: ["sfh-this-program-does-not-exist-f6"]
    effects: read
    retry: {{max: 2, backoff_sec: 5}}
    retry_on: any
  - id: wrap
    cmd: ['{quoted_sfh}', '--version']
    effects: read
    route: [{{goto: end}}]
"#
        ),
    )
    .unwrap();

    let planned = Command::new(sfh)
        .args(["plan", flow.to_str().unwrap(), "--json"])
        .output()
        .expect("sfh plan works");
    assert!(planned.status.success());
    let plan: Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(plan["steps"][0]["retry"]["max_retries"], 2);
    assert_eq!(plan["steps"][0]["retry"]["max_attempts"], 3);
    assert_eq!(
        plan["steps"][0]["retry"]["counts_toward_max_total_steps"],
        false
    );
    assert_eq!(
        plan["steps"][0]["invocations"][0]["retry"]["max_attempts"],
        3
    );
    assert_eq!(
        plan["static_max_leaves"]["retry_attempts_count_toward_max_total_steps"],
        false
    );

    let result = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--runs-dir"])
        .arg(&runs)
        .arg("-q")
        .output()
        .expect("sfh runs");
    assert_eq!(
        result.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let run_dir = only_run_dir(&runs);
    let log = std::fs::read_to_string(run_dir.join("log.jsonl")).unwrap();
    let events: Vec<Value> = log
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let retry_end = events
        .iter()
        .find(|event| event["event"] == "step_end" && event["step"].as_str() == Some("retrying"))
        .expect("failed attempt was durably recorded");
    assert_eq!(retry_end["attempts"], 1);
    assert_eq!(retry_end["retry_budget_exhausted"], true);
    assert!(events
        .iter()
        .any(|event| { event["event"] == "budget_landing" && event["trigger"] == "wall_clock" }));
    assert!(events
        .iter()
        .any(|event| event["event"] == "step_start" && event["step"] == "wrap"));

    let shown = Command::new(sfh)
        .args(["runs", "show"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .expect("runs show works");
    assert!(shown.status.success());
    let details: Value = serde_json::from_slice(&shown.stdout).unwrap();
    let retry_summary = details["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["step"] == "retrying")
        .unwrap();
    assert_eq!(retry_summary["attempts"], 1);

    let _ = std::fs::remove_dir_all(root);
}
