use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    owner: Option<Child>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.as_mut() {
            let _ = owner.kill();
            let _ = owner.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn clean(sfh: &str, root: &Path, dry: bool) -> std::process::Output {
    let mut command = Command::new(sfh);
    command.args(["runs", "clean", "--runs-dir"]).arg(root);
    command.args(["--older-than", "0", "--keep", "0"]);
    if dry {
        command.arg("--dry-run");
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    output
}

#[test]
fn clean_and_dry_run_keep_a_real_live_run_then_remove_it_after_completion() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let root = std::env::temp_dir().join(format!(
        "sfh-clean-process-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut fixture = Fixture { root, owner: None };
    let flow = fixture.root.join("flow.yaml");
    let runs = fixture.root.join("runs");
    let run = runs.join("live");
    let python = if cfg!(windows) { "python" } else { "python3" };
    std::fs::write(
        &flow,
        format!(
            "api_version: 1\nsteps:\n  - id: hold\n    effects: read\n    timeout_sec: 15\n    cmd: [{python}, -c, \"import pathlib,time; exec('while not pathlib.Path(\\\"release\\\").exists(): time.sleep(0.02)')\"]\n"
        ),
    )
    .unwrap();
    fixture.owner = Some(
        Command::new(sfh)
            .args(["run", flow.to_str().unwrap(), "--run-dir"])
            .arg(&run)
            .current_dir(&fixture.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !run.join("hold.prompt.txt").exists()
        && !std::fs::read_to_string(run.join("log.jsonl"))
            .unwrap_or_default()
            .contains("step_start")
    {
        assert!(Instant::now() < deadline, "fixture did not start");
        assert!(fixture
            .owner
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(20));
    for dry in [true, false] {
        let output = clean(sfh, &runs, dry);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("kept "), "{stdout}");
        assert!(!stdout.contains("would remove 1 "), "{stdout}");
        assert!(run.join("log.jsonl").is_file(), "live evidence lost");
        assert!(run.join("status.json").is_file(), "live status lost");
        assert!(fixture
            .owner
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
    }
    std::fs::write(fixture.root.join("release"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = fixture.owner.as_mut().unwrap().try_wait().unwrap() {
            assert!(status.success(), "fixture failed to finish: {status}");
            break;
        }
        assert!(Instant::now() < deadline, "fixture did not finish");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(20));
    let output = clean(sfh, &runs, false);
    assert!(String::from_utf8_lossy(&output.stdout).contains("removed 1 run dir"));
    assert!(!run.exists(), "dead completed run should be removable");
}
