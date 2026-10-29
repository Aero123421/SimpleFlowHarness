//! Execute the shipped graphs with local command workers. Preserve routes,
//! outcomes, context templates and visit bounds; do not test a rewritten recipe.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_example(relative: &str, scenario: &str, expected: i32) -> Value {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = Scratch(std::env::temp_dir().join(format!(
        "sfh-authoring-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )));
    std::fs::create_dir_all(&root.0).unwrap();
    let examples = std::env::var_os("SFH_AUTHORING_EXAMPLES_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.to_path_buf());
    let yaml: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&std::fs::read_to_string(examples.join(relative)).unwrap())
            .unwrap();
    let mut flow = serde_json::to_value(yaml).unwrap();
    flow["workspace"] = json!({"mode": "current"});
    flow.as_object_mut().unwrap().remove("profiles");
    // Fixed source files are substituted, dynamic evidence templates are intact.
    for context in flow["contexts"].as_object_mut().unwrap().values_mut() {
        if context.get("file").is_some() {
            *context =
                json!({"inline": "Fixed fixture acceptance: mechanical and semantic criteria"});
        }
    }
    let worker = repo.join("tests/fixtures/authoring_worker.py");
    let python = if cfg!(windows) { "python" } else { "python3" };
    for step in flow["steps"].as_array_mut().unwrap() {
        let id = step["id"].as_str().unwrap().to_owned();
        let object = step.as_object_mut().unwrap();
        for key in ["use", "fallback", "retry", "retry_on", "compact"] {
            object.remove(key);
        }
        object.insert("cmd".into(), json!([python, worker, id, scenario]));
        object
            .entry("prompt")
            .or_insert_with(|| json!("Run the fixture check."));
        object.insert("stdin".into(), json!("prompt"));
        object.insert("timeout_sec".into(), json!(10));
        // Keep declared effects; instrumentation writes disposable local counters.
    }
    let path = root.0.join("flow.yaml");
    std::fs::write(&path, serde_json::to_vec_pretty(&flow).unwrap()).unwrap();
    let validation = Command::new(env!("CARGO_BIN_EXE_sfh"))
        .args(["validate", path.to_str().unwrap(), "--strict"])
        .current_dir(&root.0)
        .output()
        .unwrap();
    assert!(
        validation.status.success(),
        "{relative} {scenario}: {validation:?}"
    );
    let run = root.0.join("run");
    let result = Command::new(env!("CARGO_BIN_EXE_sfh"))
        .arg("run")
        .arg(&path)
        .arg("--run-dir")
        .arg(&run)
        .arg("--json")
        .current_dir(&root.0)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(expected),
        "{relative} {scenario}: {result:?}"
    );
    let log = std::fs::read_to_string(run.join("log.jsonl")).unwrap();
    assert!(
        !log.contains("missing input evidence"),
        "{relative} {scenario}: {log}"
    );
    for operation in ["status", "wait"] {
        let output = Command::new(env!("CARGO_BIN_EXE_sfh"))
            .arg(operation)
            .arg(&run)
            .arg("--json")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{relative} {scenario}: {output:?}"
        );
    }
    serde_json::from_str(&std::fs::read_to_string(root.0.join("counts.json")).unwrap()).unwrap()
}

#[test]
fn repair_examples_pass_check_and_evaluator_evidence_and_stop_on_uninspectable_checks() {
    for (file, correction, evaluator) in [
        (
            "skills/sfh-loop-engineering/assets/bounded-evaluator-loop.yaml",
            "improve",
            "evaluate",
        ),
        (
            "skills/sfh-flow-design/assets/stable-flow-skeleton.yaml",
            "fix",
            "review",
        ),
        (
            "examples/ponytail/13-planner-builder-evaluator-loop.yaml",
            "improve",
            "evaluate",
        ),
        (
            "examples/ponytail/14-sequential-chunked-long-build.yaml",
            "repair",
            "review",
        ),
    ] {
        let counts = run_example(file, "loop-pass", 0);
        assert!(counts[correction].is_null(), "{file}: {counts}");
        assert_eq!(counts["verify"], 1, "{file}: {counts}");
        assert_eq!(counts[evaluator], 1, "{file}: {counts}");
        let counts = run_example(file, "loop-repair", 0);
        assert_eq!(counts[correction], 2, "{file}: {counts}");
        assert_eq!(counts["verify"], 3, "{file}: {counts}");
        assert_eq!(counts[evaluator], 2, "{file}: {counts}");
        let counts = run_example(file, "infra", 4);
        assert!(counts[correction].is_null(), "{file}: {counts}");
        assert!(counts[evaluator].is_null(), "{file}: {counts}");
        assert_eq!(counts["handoff"], 1, "{file}: {counts}");
        let counts = run_example(file, "loop-malformed", 4);
        assert_eq!(counts[correction], 1, "{file}: {counts}");
        assert_eq!(counts[evaluator], 1, "{file}: {counts}");
        let counts = run_example(file, "stalled", 4);
        assert_eq!(counts[correction], 4, "{file}: {counts}");
        assert_eq!(counts["handoff"], 1, "{file}: {counts}");
    }
}

#[test]
fn incident_examples_require_confirmed_failure_and_never_fix_an_inspection_error() {
    for (file, first_fix) in [
        (
            "skills/sfh-eval-engineering/assets/failure-to-regression.yaml",
            "fix",
        ),
        (
            "examples/ponytail/15-production-failure-to-eval-fix.yaml",
            "implement",
        ),
    ] {
        let counts = run_example(file, "confirmed-incident", 0);
        assert_eq!(counts[first_fix], 1, "{file}: {counts}");
        assert_eq!(counts["review"], 1, "{file}: {counts}");
        for scenario in [
            "not-reproduced",
            "repro-error-1",
            "repro-error-2",
            "repro-error-3",
            "repro-error-4",
            "repro-error-5",
            "repro-error-6",
            "repro-error-127",
        ] {
            let counts = run_example(file, scenario, 4);
            assert!(counts[first_fix].is_null(), "{file} {scenario}: {counts}");
            assert!(counts["review"].is_null(), "{file} {scenario}: {counts}");
        }
        for scenario in ["focused-error", "regression-error"] {
            let counts = run_example(file, scenario, 4);
            assert_eq!(counts[first_fix], 1, "{file} {scenario}: {counts}");
            assert!(counts["review"].is_null(), "{file} {scenario}: {counts}");
        }
    }
}
