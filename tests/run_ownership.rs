use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn assert_run_busy(output: std::process::Output) {
    assert_eq!(output.status.code(), Some(2));
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "run-busy response was not JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        body["error"]["code"], "SFH_RUN_BUSY",
        "unexpected run response: {body}"
    );
}

#[test]
fn a_live_run_exclusively_owns_its_run_directory() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = std::env::temp_dir().join(format!(
        "sfh-run-ownership-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    let flow = base.join("flow.yaml");
    let run_dir = base.join("run");
    let python = if cfg!(windows) { "python" } else { "python3" };
    std::fs::write(
        &flow,
        format!(
            "api_version: 1\nname: ownership\nsteps:\n  - id: hold\n    cmd: [{python}, -c, 'import time; time.sleep(30)']\n    effects: read\n"
        ),
    )
    .unwrap();

    let mut owner = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&run_dir.join("status.json"));
    assert!(
        owner.try_wait().unwrap().is_none(),
        "fixture run exited early"
    );

    let duplicate = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_run_busy(duplicate);

    let resume = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--resume"])
        .arg(&run_dir)
        .arg("--json")
        .output()
        .unwrap();
    assert_run_busy(resume);

    let stopped = Command::new(sfh)
        .arg("stop")
        .arg(&run_dir)
        .output()
        .unwrap();
    if !stopped.status.success() {
        let _ = owner.kill();
        panic!(
            "failed to stop fixture run: stdout={}; stderr={}",
            String::from_utf8_lossy(&stopped.stdout),
            String::from_utf8_lossy(&stopped.stderr)
        );
    }
    let _ = owner.wait();
    let _ = std::fs::remove_dir_all(base);
}
