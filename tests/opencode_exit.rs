use std::process::Command;

#[test]
fn text_only_completion_requires_a_clean_process_even_with_declared_success_overrides() {
    let root = std::env::temp_dir().join(format!(
        "sfh-opencode-exit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let stub_src = root.join("fixture.rs");
    let stub = root.join(if cfg!(windows) {
        "fixture.exe"
    } else {
        "fixture"
    });
    std::fs::write(&stub_src, r#"
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") { println!("2.2.1"); return; }
    if args.iter().any(|a| a == "--help") { println!("run --standalone --format --model --agent --auto"); return; }
    print!("{}", std::fs::read_to_string(std::env::var("SFH_TRANSCRIPT").unwrap()).unwrap());
    std::process::exit(std::env::var("SFH_FIXTURE_EXIT").unwrap().parse().unwrap());
}
"#).unwrap();
    let compiled = Command::new("rustc")
        .args(["--edition", "2021"])
        .arg(&stub_src)
        .arg("-o")
        .arg(&stub)
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    let transcript = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cli-2026-10/opencode-2.2.1-text.jsonl");
    for code in [0, 7] {
        let run = root.join(format!("run-{code}"));
        let flow = root.join(format!("flow-{code}.yaml"));
        let bin = serde_json::to_string(&stub).unwrap();
        let transcript = serde_json::to_string(&transcript).unwrap();
        std::fs::write(&flow, format!("api_version: 1\nsteps:\n  - id: work\n    tool: opencode\n    bin: {bin}\n    access: read\n    prompt: test\n    exit_conflict: trust_protocol\n    outcomes: {{7: {{result: complete}}}}\n    env:\n      SFH_TRANSCRIPT: {transcript}\n      SFH_FIXTURE_EXIT: '{code}'\n")).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_sfh"))
            .arg("run")
            .arg(&flow)
            .arg("--run-dir")
            .arg(&run)
            .arg("--json")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if code == 0 { 0 } else { 1 }),
            "{output:?}"
        );
        let log = std::fs::read_to_string(run.join("log.jsonl")).unwrap();
        let step = log
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|v| v["event"] == "step_end")
            .unwrap();
        assert_eq!(step["protocol_state"], "exit_bound");
        assert_eq!(step["terminal_seen"], false);
        assert!(step["terminal_success"].is_null());
    }
    std::fs::remove_dir_all(root).unwrap();
}
