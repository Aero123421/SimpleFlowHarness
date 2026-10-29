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

/// A real managed child, with synthetic OpenCode output. It leaves an incomplete
/// JSON record until interrupted; a later resume can complete deterministically.
fn interrupted_fixture(sigint: bool) -> (Fixture, PathBuf, PathBuf, std::process::Output) {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let root = std::env::temp_dir().join(format!(
        "sfh-interrupted-clean-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut fixture = Fixture { root, owner: None };
    let source = fixture.root.join("child.rs");
    let child = fixture
        .root
        .join(if cfg!(windows) { "child.exe" } else { "child" });
    std::fs::write(&source, r##"
fn main() {
    use std::io::Write;
    if std::env::args().any(|arg| arg == "--version") { println!("2.2.1"); return; }
    if std::path::Path::new("release").exists() {
        println!("{}", r#"{"type":"text","sessionID":"ses_fixture","part":{"id":"part_fixture","messageID":"message_fixture","sessionID":"ses_fixture","type":"text","text":"resumed","time":{"start":1,"end":2}}}"#);
        return;
    }
    print!("{{\"type\":\"text\"");
    std::io::stdout().flush().unwrap();
    std::fs::write("started", "").unwrap();
    std::thread::sleep(std::time::Duration::from_secs(60));
}
"##).unwrap();
    let compiled = Command::new("rustc")
        .args(["--edition", "2021"])
        .arg(&source)
        .arg("-o")
        .arg(&child)
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    let flow = fixture.root.join("flow.yaml");
    let run = fixture.root.join("state/runs/001-interrupted");
    std::fs::write(&flow, format!(
        "api_version: 1\nsteps:\n  - id: work\n    tool: opencode\n    bin: {}\n    effects: read\n    access: read\n    timeout_sec: 15\n    prompt: fixture\n",
        serde_json::to_string(&child).unwrap()
    )).unwrap();
    fixture.owner = Some(
        Command::new(sfh)
            .arg("run")
            .arg(&flow)
            .arg("--run-dir")
            .arg(&run)
            .arg("--json")
            .current_dir(&fixture.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.root.join("started").exists() {
        assert!(Instant::now() < deadline, "fixture child did not start");
        assert!(fixture
            .owner
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        std::thread::sleep(Duration::from_millis(20));
    }
    if sigint {
        #[cfg(unix)]
        assert_eq!(
            unsafe { libc::kill(fixture.owner.as_ref().unwrap().id() as i32, libc::SIGINT) },
            0
        );
        #[cfg(not(unix))]
        panic!("SIGINT fixture is Unix-only");
    } else {
        let output = Command::new(sfh)
            .arg("stop")
            .arg(&run)
            .arg("--json")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while fixture
        .owner
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_none()
    {
        assert!(Instant::now() < deadline, "interrupted owner did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = fixture.owner.take().unwrap().wait_with_output().unwrap();
    (fixture, flow, run, output)
}

#[cfg(unix)]
#[test]
fn sigint_reports_interrupted_even_with_incomplete_structured_output() {
    let (_fixture, _flow, run, output) = interrupted_fixture(true);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "SFH_INTERRUPTED", "{result}");
    assert!(result["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|action| action["kind"] == "resume" && action["resumable"] == true));
    let status: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["error_code"], "SFH_INTERRUPTED");
    let log = std::fs::read_to_string(run.join("log.jsonl")).unwrap();
    let step = log
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .find(|event| event["event"] == "step_end")
        .unwrap();
    assert_eq!(step["interrupted"], true);
    assert_eq!(step["failure_code"], "SFH_INTERRUPTED");
    assert_ne!(step["protocol_state"], "valid");
}

#[test]
fn cleanup_and_retention_preserve_interrupted_runs_until_resumed_to_completion() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let modes: &[bool] = if cfg!(unix) { &[false, true] } else { &[false] };
    for &sigint in modes {
        let (fixture, flow, run, _output) = interrupted_fixture(sigint);
        let runs = run.parent().unwrap();
        for dry in [true, false] {
            let output = clean(sfh, runs, dry);
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("kept "),
                "{output:?}"
            );
            assert!(
                run.join("log.jsonl").is_file(),
                "interrupted evidence lost (SIGINT={sigint}, dry={dry})"
            );
        }
        std::fs::write(fixture.root.join("release"), "").unwrap();
        let completed = runs.join("002-completed");
        for dir in [&completed, &runs.join("999-keep")] {
            let output = Command::new(sfh)
                .arg("run")
                .arg(&flow)
                .arg("--run-dir")
                .arg(dir)
                .current_dir(&fixture.root)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        }
        // Age only the disposable fixture directories; keep=1 protects 999-keep.
        let python = if cfg!(windows) { "python" } else { "python3" };
        let aged = Command::new(python).args(["-c", "import os,sys,time; t=time.time()-172800; [os.utime(p,(t,t)) for p in sys.argv[1:]]"])
            .arg(&run).arg(&completed).output().unwrap();
        assert!(aged.status.success(), "{aged:?}");
        let state = fixture.root.join("state");
        std::fs::write(
            state.join("retention.yaml"),
            "runs:\n  older_than_days: 1\n  keep: 1\n",
        )
        .unwrap();
        let output = Command::new(sfh)
            .arg("run")
            .arg(&flow)
            .arg("--state-dir")
            .arg(&state)
            .current_dir(&fixture.root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            run.join("log.jsonl").is_file(),
            "retention removed interrupted evidence"
        );
        assert!(
            !completed.exists(),
            "retention must still remove completed old runs"
        );
        let resumed = Command::new(sfh)
            .arg("run")
            .arg(&flow)
            .arg("--resume")
            .arg(&run)
            .arg("--json")
            .current_dir(&fixture.root)
            .output()
            .unwrap();
        assert!(resumed.status.success(), "{resumed:?}");
        let result: serde_json::Value = serde_json::from_slice(&resumed.stdout).unwrap();
        assert_eq!(result["state"], "done");
        clean(sfh, runs, false);
        assert!(!run.exists(), "completed resumed run should be removable");
    }
}
