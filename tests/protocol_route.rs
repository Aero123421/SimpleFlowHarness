use std::process::Command;

#[test]
fn protocol_failure_can_route_to_a_salvage_step() {
    let sfh = env!("CARGO_BIN_EXE_sfh");
    let base = std::env::temp_dir().join(format!(
        "sfh-protocol-route-{}-{}",
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
            "api_version: 1\nname: protocol-salvage\nsteps:\n  - id: drifted\n    tool: codex\n    bin: \"{bin}\"\n    access: read\n    on_error: continue\n    prompt: this fixture is not the codex protocol\n    route:\n      - {{when_protocol_is: missing_terminal, goto: salvage}}\n      - {{when_protocol_is: invalid, goto: salvage}}\n      - {{goto: fail}}\n  - id: salvage\n    cmd: [\"{bin}\", --version]\n    route: [{{goto: end}}]\n"
        ),
    )
    .unwrap();

    let output = Command::new(sfh)
        .args(["run", flow.to_str().unwrap(), "--run-dir"])
        .arg(&run_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "salvage route failed; stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(run_dir.join("salvage.out.txt").exists());
    let stderr = std::fs::read_to_string(run_dir.join("drifted.err.txt")).unwrap();
    assert_eq!(
        stderr
            .matches("machine-readable protocol did not hold")
            .count(),
        1,
        "the sfh protocol diagnosis must be persisted exactly once: {stderr}"
    );
    let log = std::fs::read_to_string(run_dir.join("log.jsonl")).unwrap();
    assert!(
        log.lines().any(|line| {
            line.contains("\"event\":\"position\"")
                && line.contains("\"next\":\"salvage\"")
                && line.contains("\"protocol_state\":")
        }),
        "the durable route decision must record the protocol state: {log}"
    );

    let _ = std::fs::remove_dir_all(base);
}
