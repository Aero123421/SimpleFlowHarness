//! `sfh status` / `sfh wait` - check on a run without staying attached to it.
//!
//! A detached run outlives its caller, so the caller needs a way to ask "is it
//! still going?" that cannot be fooled by a status file left behind by a
//! process that was killed. Liveness is therefore two signals, not one: the
//! recorded pid must still exist AND status.json must have been touched
//! recently. Either one alone is unreliable - pids get reused, and a wedged
//! process keeps its pid.

use crate::execute;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The engine heartbeats every 3s; 20 missed beats means it is not running,
/// whatever the pid table says.
const STALE_SEC: u64 = 60;

pub struct Snapshot {
    pub dir: PathBuf,
    /// Resolved state: running | done | failed | dead | unknown.
    pub state: &'static str,
    pub reason: Option<String>,
    pub step: String,
    pub steps_done: u64,
    pub cost_usd: f64,
    pub pid: u32,
    pub heartbeat_age_sec: u64,
    pub exit_code: Option<i64>,
    pub emit_file: Option<String>,
    pub emit_step: Option<String>,
    pub error: Option<String>,
    pub flow: String,
    pub started: String,
}

impl Snapshot {
    pub fn terminal(&self) -> bool {
        matches!(self.state, "done" | "failed" | "dead" | "stopped")
    }

    /// 0 = done, 1 = failed / dead / stopped, 3 = still running, 2 = cannot tell.
    pub fn exit(&self) -> i32 {
        match self.state {
            "done" => 0,
            "failed" | "dead" | "stopped" => 1,
            "running" => 3,
            _ => 2,
        }
    }
}

fn run_dirs(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.join("status.json").exists())
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

pub fn latest(root: &Path) -> Option<PathBuf> {
    run_dirs(root).pop()
}

fn age_sec(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX)
}

/// A poll that lands mid-write must not be reported as a broken run. The writer
/// renames into place, so any failure here is transient; retry briefly before
/// giving up.
pub fn read(dir: &Path) -> Result<Snapshot, String> {
    let mut last = String::new();
    for attempt in 0..4 {
        match read_once(dir) {
            Ok(s) => return Ok(s),
            Err(e) => {
                last = e;
                if attempt < 3 {
                    std::thread::sleep(Duration::from_millis(120));
                }
            }
        }
    }
    Err(last)
}

fn read_once(dir: &Path) -> Result<Snapshot, String> {
    let sp = dir.join("status.json");
    let text = std::fs::read_to_string(&sp).map_err(|_| {
        if dir.join("log.jsonl").exists() {
            format!(
                "{} has no status.json (run started with an older sfh?)",
                dir.display()
            )
        } else {
            format!("{} is not an sfh run directory", dir.display())
        }
    })?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{}: unreadable status.json: {e}", dir.display()))?;

    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(String::from)
            .unwrap_or_default()
    };
    let opt = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let pid = v.get("pid").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let age = age_sec(&sp);
    let raw = s("state");

    let mut reason = None;
    let state = match raw.as_str() {
        "done" => "done",
        "failed" => "failed",
        "stopped" => "stopped",
        "running" => {
            let alive = execute::pid_alive(pid);
            if !alive {
                reason = Some(format!("process {pid} is gone"));
                "dead"
            } else if age > STALE_SEC {
                // Live pid but no heartbeat: either the pid was reused by an
                // unrelated process, or this one is wedged. Not progressing.
                reason = Some(format!("no heartbeat for {age}s"));
                "dead"
            } else {
                "running"
            }
        }
        other => {
            reason = Some(format!("unrecognised state '{other}'"));
            "unknown"
        }
    };

    Ok(Snapshot {
        dir: dir.to_path_buf(),
        state,
        reason,
        step: s("current_step"),
        steps_done: v.get("steps_done").and_then(|x| x.as_u64()).unwrap_or(0),
        cost_usd: v.get("cost_usd").and_then(|x| x.as_f64()).unwrap_or(0.0),
        pid,
        heartbeat_age_sec: age,
        exit_code: v.get("exit_code").and_then(|x| x.as_i64()),
        emit_file: opt("emit_file"),
        emit_step: opt("emit_step"),
        error: opt("error"),
        flow: s("flow"),
        started: s("started_utc"),
    })
}

/// Resolve an explicit run dir, or the newest run under `root`.
pub fn resolve(target: Option<&Path>, root: &Path) -> Result<PathBuf, String> {
    match target {
        Some(d) => Ok(d.to_path_buf()),
        None => latest(root).ok_or_else(|| format!("no runs found under {}", root.display())),
    }
}

pub fn status(target: Option<&Path>, root: &Path, as_json: bool) -> i32 {
    let dir = match resolve(target, root) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sfh: {e}");
            return 2;
        }
    };
    let snap = match read(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sfh: {e}");
            return 2;
        }
    };
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "state": snap.state,
                "reason": snap.reason,
                "run_dir": snap.dir.display().to_string(),
                "flow": snap.flow,
                "started_utc": snap.started,
                "current_step": snap.step,
                "steps_done": snap.steps_done,
                "cost_usd": snap.cost_usd,
                "pid": snap.pid,
                "heartbeat_age_sec": snap.heartbeat_age_sec,
                "exit_code": snap.exit_code,
                "emit_step": snap.emit_step,
                "emit_file": snap.emit_file,
                "error": snap.error,
            }))
            .unwrap_or_default()
        );
        return snap.exit();
    }

    let extra = match snap.state {
        "running" => format!(
            "step '{}', {}s since heartbeat",
            snap.step, snap.heartbeat_age_sec
        ),
        "failed" => snap.error.clone().unwrap_or_default(),
        _ => snap.reason.clone().unwrap_or_default(),
    };
    println!(
        "{:<8} {} steps, ${:.4}{}{}",
        snap.state,
        snap.steps_done,
        snap.cost_usd,
        if extra.is_empty() { "" } else { " - " },
        extra
    );
    println!("run dir: {}", snap.dir.display());
    match snap.state {
        "running" => eprintln!(
            "sfh: still running (pid {}); `sfh wait` blocks until it finishes",
            snap.pid
        ),
        "done" => eprintln!("sfh: result: sfh wait {}", snap.dir.display()),
        "stopped" | "dead" => eprintln!(
            "sfh: this run was killed before it finished. resume with: sfh run {} --resume {}",
            if snap.flow.is_empty() {
                "<flow.yaml>"
            } else {
                &snap.flow
            },
            snap.dir.display()
        ),
        _ => {}
    }
    snap.exit()
}

/// Cancel a run: kill the process tree and record that it was deliberate, so a
/// later `sfh status` says "stopped" rather than the ambiguous "dead".
pub fn stop(target: Option<&Path>, root: &Path) -> i32 {
    let dir = match resolve(target, root) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sfh: {e}");
            return 2;
        }
    };
    let snap = match read(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sfh: {e}");
            return 2;
        }
    };
    if snap.terminal() {
        eprintln!(
            "sfh: nothing to stop - this run is already '{}'. run dir: {}",
            snap.state,
            dir.display()
        );
        return 0;
    }
    if !execute::kill_pid_tree(snap.pid) {
        eprintln!(
            "sfh: could not kill process {} (already gone?); check: sfh status {}",
            snap.pid,
            dir.display()
        );
        return 1;
    }
    // The run dies without a chance to record anything, so write the verdict
    // here. Cost and progress stay as of the last heartbeat, which is honest:
    // they are what was actually spent before the kill.
    let sp = dir.join("status.json");
    if let Ok(text) = std::fs::read_to_string(&sp) {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(m) = v.as_object_mut() {
                m.insert("state".into(), serde_json::json!("stopped"));
                m.insert("exit_code".into(), serde_json::json!(1));
                m.insert("error".into(), serde_json::json!("cancelled by sfh stop"));
                let _ = std::fs::write(&sp, serde_json::to_string_pretty(&v).unwrap_or_default());
            }
        }
    }
    println!("stopped {}", dir.display());
    eprintln!(
        "sfh: killed pid {} and its children. ${:.4} was spent before the stop. resume with: sfh run {} --resume {}",
        snap.pid,
        snap.cost_usd,
        if snap.flow.is_empty() { "<flow.yaml>" } else { &snap.flow },
        dir.display()
    );
    0
}

/// Print what a foreground run would have printed to stdout.
/// The detached child's stdout was captured verbatim, so prefer that; fall back
/// to the emitted step's chain file for runs that were never detached.
fn print_result(snap: &Snapshot) {
    let detached = snap.dir.join("detached.out.txt");
    if let Ok(t) = std::fs::read_to_string(&detached) {
        if !t.trim().is_empty() {
            let mut o = std::io::stdout();
            let _ = o.write_all(t.as_bytes());
            let _ = o.flush();
            return;
        }
    }
    if let Some(f) = &snap.emit_file {
        if let Ok(t) = std::fs::read_to_string(f) {
            println!("{}", t.trim_end());
        }
    }
}

pub fn wait(
    target: Option<&Path>,
    root: &Path,
    timeout_sec: Option<u64>,
    interval_sec: u64,
    quiet: bool,
) -> i32 {
    let dir = match resolve(target, root) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sfh: {e}");
            return 2;
        }
    };
    let started = SystemTime::now();
    let interval = Duration::from_secs(interval_sec.max(1));
    let mut last_step = String::new();
    loop {
        let snap = match read(&dir) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("sfh: {e}");
                return 2;
            }
        };
        if snap.terminal() {
            match snap.state {
                "done" => {
                    print_result(&snap);
                    if !quiet {
                        eprintln!(
                            "sfh: done. {} steps, ${:.4} reported. run dir: {}",
                            snap.steps_done,
                            snap.cost_usd,
                            snap.dir.display()
                        );
                    }
                }
                "failed" => {
                    eprintln!(
                        "sfh: FLOW FAILED: {}",
                        snap.error.as_deref().unwrap_or("(no error recorded)")
                    );
                    print_result(&snap);
                    eprintln!("sfh: run dir: {}", snap.dir.display());
                }
                _ => {
                    eprintln!(
                        "sfh: run did not finish ({}). resume with: sfh run {} --resume {}",
                        snap.reason
                            .as_deref()
                            .or(snap.error.as_deref())
                            .unwrap_or("no longer running"),
                        if snap.flow.is_empty() {
                            "<flow.yaml>"
                        } else {
                            &snap.flow
                        },
                        snap.dir.display()
                    );
                }
            }
            return snap
                .exit_code
                .map(|c| c as i32)
                .unwrap_or_else(|| snap.exit());
        }
        if !quiet && snap.step != last_step {
            eprintln!("sfh: waiting - step '{}'", snap.step);
            last_step = snap.step.clone();
        }
        if let Some(t) = timeout_sec {
            let elapsed = started.elapsed().map(|d| d.as_secs()).unwrap_or(0);
            if elapsed >= t {
                eprintln!(
                    "sfh: still running after {t}s; the run is NOT cancelled. check: sfh status {}",
                    dir.display()
                );
                return 3;
            }
        }
        std::thread::sleep(interval);
    }
}
