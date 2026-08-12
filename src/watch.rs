//! `sfh status` / `sfh wait` - check on a run without staying attached to it.
//!
//! A detached run outlives its caller, so the caller needs a way to ask "is it
//! still going?" that cannot be fooled by a status file left behind by a
//! process that was killed. Liveness is therefore two signals, not one: the
//! recorded pid must still exist AND status.json must have been touched
//! recently. Either one alone is unreliable - pids get reused, and a wedged
//! process keeps its pid.

use crate::contain;
use crate::execute;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// The engine heartbeats every 3s; 20 missed beats means it is not running,
/// whatever the pid table says.
const STALE_SEC: u64 = 60;

pub struct Snapshot {
    pub dir: PathBuf,
    /// Resolved state: running | done | failed | stuck | dead | unknown.
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
    pub error_code: Option<crate::machine::ErrorCode>,
    pub flow: String,
    pub started: String,
    /// The idle clocks (F2). All three are absent from a status.json written by
    /// sfh <= 1.0, and the display drops the whole segment when they are, rather
    /// than printing "0s elapsed" about a run it cannot time.
    pub step_started: Option<String>,
    pub last_output: Option<String>,
    /// Fan-out member id -> queued/running/fallback state. Keep the values in
    /// the public JSON view; names alone cannot distinguish queue pressure from
    /// work that is actually executing.
    pub active_members: BTreeMap<String, String>,
    pub fanout_total: u64,
    pub fanout_completed: u64,
    pub visit: Option<u64>,
    pub nonce: Option<String>,
    /// Start time of the owning process, recorded when the run began. Lets
    /// `sfh stop` tell a reused pid apart from the original (rev_break #8).
    pub pid_start: Option<u64>,
}

impl Snapshot {
    /// "stuck" belongs here: the run is over and will not move on its own. It
    /// is listed with the other terminals rather than beside "running" so that
    /// every check guarding a terminal report (above all the nonce
    /// authentication) covers it too. A forged `state: "stuck"` gets no more
    /// trust for being the newest state name.
    pub fn terminal(&self) -> bool {
        matches!(self.state, "done" | "failed" | "stuck" | "dead" | "stopped")
    }

    /// 0 = done, 1 = failed / dead / stopped, 3 = still running, 2 = cannot
    /// tell, 4 = stuck (the flow reached a `goto: stuck` and wants a human).
    pub fn exit(&self) -> i32 {
        match self.state {
            "done" => 0,
            "failed" | "dead" | "stopped" => 1,
            "running" => 3,
            "stuck" => 4,
            _ => 2,
        }
    }
}

fn run_dirs(root: &Path) -> Vec<PathBuf> {
    // A directory entry may be a symlink (Unix) or a junction (Windows) that
    // points OUTSIDE the runs root, and is_dir() / exists() FOLLOW links, so a
    // no-argument `sfh status` / `sfh wait` / `sfh stop` (and --resume-latest,
    // which has its own copy of this filter in engine::latest_run_dir) would
    // otherwise select a run dir the caller never pointed at. Enumerate by
    // lstat (read_dir's file_type does not follow) and require the resolved
    // path to stay under the resolved root (rev_break #7).
    let Ok(canon_root) = root.canonicalize() else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .filter(|p| {
                contain::read_contained_opt(p, "status.json")
                    .map(|status| status.is_some())
                    .unwrap_or(false)
            })
            .filter(|p| match p.canonicalize() {
                Ok(c) => c.starts_with(&canon_root),
                Err(_) => false,
            })
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
    // Contained, no-follow read: status.json is a fixed name in a directory an
    // attacker controls on a forged run, and a symlink there used to be followed
    // to an external JSON file that was then reported as the run's state
    // (rev_break #6). An outward link is a hard error; a missing file keeps the
    // older friendly diagnostics.
    let text = match contain::read_contained_opt(dir, "status.json") {
        Ok(Some(t)) => t,
        Ok(None) => {
            return Err(if dir.join("log.jsonl").exists() {
                format!(
                    "{} has no status.json (run started with an older sfh?)",
                    dir.display()
                )
            } else {
                format!("{} is not an sfh run directory", dir.display())
            })
        }
        Err(e) => return Err(format!("{}: cannot read status.json: {e}", dir.display())),
    };
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{}: unreadable status.json: {e}", dir.display()))?;

    let s = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(String::from)
            .unwrap_or_default()
    };
    let opt = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    // Range-check the pid: the old `as u32` wrapped a forged 4294967296 to 0
    // and an absent pid read as 0 too; both are recorded as invalid so the
    // liveness check below resolves a claimed "running" to dead instead of
    // asking "is pid 0 alive?" (rev_break #9).
    let (pid, pid_valid) = match v.get("pid").and_then(|x| x.as_u64()) {
        Some(p) => match u32::try_from(p) {
            Ok(p) => (p, true),
            Err(_) => (0, false),
        },
        None => (0, false),
    };
    let age = age_sec(&sp);
    let raw = s("state");

    let mut reason = None;
    let state = match raw.as_str() {
        "done" => "done",
        "failed" => "failed",
        "stuck" => "stuck",
        "stopped" => "stopped",
        "running" => {
            if !pid_valid {
                reason = Some("status.json records no valid pid".to_string());
                "dead"
            } else if !execute::pid_alive(pid) {
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
        error_code: opt("error_code").and_then(|code| crate::machine::ErrorCode::parse(&code)),
        flow: s("flow"),
        started: s("started_utc"),
        step_started: opt("step_started_utc"),
        last_output: opt("last_output_utc"),
        active_members: v
            .get("active_members")
            .and_then(|x| x.as_object())
            .map(|m| {
                m.iter()
                    .map(|(member, state)| {
                        (
                            member.clone(),
                            state.as_str().unwrap_or("unknown").to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        fanout_total: v.get("fanout_total").and_then(|x| x.as_u64()).unwrap_or(0),
        fanout_completed: v
            .get("fanout_completed")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        visit: v.get("visit").and_then(|x| x.as_u64()),
        nonce: opt("nonce"),
        pid_start: v.get("pid_start").and_then(|x| x.as_u64()),
    })
}

/// Round a duration to one human-sized unit. Precision past this is noise for
/// the question being asked ("is this thing moving?").
fn human_dur(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// "<n> since <stamp>", or None when the stamp is missing or unparseable. A
/// stamp in the future (clock skew between the writer and this process) reports
/// 0s rather than a wrapped number.
fn since(stamp: Option<&String>) -> Option<String> {
    let secs = crate::engine::parse_utc_stamp(stamp?.as_str())?;
    Some(human_dur(crate::execute::epoch_secs().saturating_sub(secs)))
}

/// The second half of the running line: how long the current step has been in
/// there and how long since anything it started said a word. This is the pair
/// that tells a 40-minute step that is working from one that died quietly
/// (B-12); every signal the caller had before this - pid alive, heartbeat
/// fresh, state "running" - said the wedged run was healthy.
fn idle_segment(snap: &Snapshot) -> Option<String> {
    let elapsed = since(snap.step_started.as_ref())?;
    let visit = match snap.visit {
        Some(v) if v > 0 => format!(" (visit {v})"),
        _ => String::new(),
    };
    let quiet = match since(snap.last_output.as_ref()) {
        Some(q) => format!("{q} since last output"),
        // Null last_output_utc is a fact, not a gap: no child of this run has
        // written a byte yet.
        None => "no output yet".to_string(),
    };
    let fanout = if snap.fanout_total > 0 {
        let active = if snap.active_members.is_empty() {
            String::new()
        } else {
            format!(
                "; active {}",
                snap.active_members
                    .iter()
                    .map(|(member, state)| format!("{member}({state})"))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        format!(
            ", fan-out {}/{}{}",
            snap.fanout_completed, snap.fanout_total, active
        )
    } else {
        String::new()
    };
    Some(format!(
        "{}{visit}, {elapsed} elapsed, {quiet}{fanout}",
        snap.step
    ))
}

/// The flow path for a command hint: a placeholder when unknown, otherwise
/// quoted so a name with spaces (allowed since R-6) pastes back as one arg.
fn flow_arg(flow: &str) -> String {
    if flow.is_empty() {
        "<flow.yaml>".to_string()
    } else {
        execute::shell_quote(flow)
    }
}

/// Resolve an explicit run dir, or the newest run under `root`.
pub fn resolve(target: Option<&Path>, root: &Path) -> Result<PathBuf, String> {
    match target {
        Some(d) => Ok(d.to_path_buf()),
        None => latest(root).ok_or_else(|| format!("no runs found under {}", root.display())),
    }
}

/// A failure answer for a watch command, in whichever form the caller asked
/// for. In JSON mode this is an envelope, never a bare message: "sfh printed
/// prose and exited 2" is exactly what a machine caller cannot act on.
fn fail(as_json: bool, command: &str, code: crate::machine::ErrorCode, msg: &str) -> i32 {
    if as_json {
        crate::machine::emit(&crate::machine::error_envelope(
            command,
            code,
            msg,
            2,
            serde_json::json!({"state": "usage_error", "terminal": true}),
        ));
    } else {
        eprintln!("sfh: {msg}");
    }
    2
}

fn snapshot_error_code(snap: &Snapshot) -> crate::machine::ErrorCode {
    snap.error_code.unwrap_or(match snap.state {
        "stuck" => crate::machine::ErrorCode::Stuck,
        "dead" | "stopped" => crate::machine::ErrorCode::Interrupted,
        _ => snap
            .error
            .as_deref()
            .map(crate::engine::run_failure_code)
            .unwrap_or(crate::machine::ErrorCode::FlowInvalid),
    })
}

pub fn status(target: Option<&Path>, root: &Path, as_json: bool) -> i32 {
    let dir = match resolve(target, root) {
        Ok(d) => d,
        Err(e) => return fail(as_json, "status", crate::machine::ErrorCode::Usage, &e),
    };
    let snap = match read(&dir) {
        Ok(s) => s,
        Err(e) => return fail(as_json, "status", crate::machine::ErrorCode::Usage, &e),
    };
    // A terminal state an untrusted run dir asserts about itself is not reported
    // as fact: the same nonce authentication `sfh wait` and `sfh stop` run must
    // back it. A forged status.json that says "done" without a matching nonce
    // used to print as success and exit 0; it is now refused (rev_break #10).
    if snap.terminal() {
        if let Err(e) = nonce_consistent(&dir, &snap) {
            let message = format!(
                "refusing to report {} as '{}': {e}",
                dir.display(),
                snap.state
            );
            if as_json {
                crate::machine::emit(&crate::machine::error_envelope(
                    "status",
                    crate::machine::ErrorCode::PersistenceFailure,
                    &message,
                    1,
                    serde_json::json!({
                        "state": snap.state,
                        "run_dir": dir.display().to_string(),
                        "terminal": true,
                    }),
                ));
            } else {
                eprintln!("sfh: {message}");
            }
            return 1;
        }
    }
    if as_json {
        // Additive only: every field this command has ever emitted is still
        // here and still means what it did. The common envelope header and
        // `implicit_target` are new keys beside them, so a reader written
        // against 1.1 keeps working unchanged.
        let failure_code = snapshot_error_code(&snap);
        let failure_message = snap
            .error
            .as_deref()
            .or(snap.reason.as_deref())
            .unwrap_or("the run did not finish")
            .to_string();
        let body = serde_json::json!({
                "state": snap.state,
                "reason": snap.reason,
                "run_dir": snap.dir.display().to_string(),
                "flow": snap.flow,
                "started_utc": snap.started,
                "current_step": snap.step,
                "step_started_utc": snap.step_started,
                "last_output_utc": snap.last_output,
                "active_members": snap.active_members,
                "fanout_total": snap.fanout_total,
                "fanout_completed": snap.fanout_completed,
                "visit": snap.visit,
                "steps_done": snap.steps_done,
                "cost_usd": snap.cost_usd,
                "pid": snap.pid,
                "heartbeat_age_sec": snap.heartbeat_age_sec,
                "exit_code": snap.exit_code,
                "emit_step": snap.emit_step,
                "emit_file": snap.emit_file,
                "terminal": snap.terminal(),
                "run_id": snap.dir.file_name().map(|n| n.to_string_lossy().into_owned()),
                // A caller that omitted the path got the NEWEST run, which may
                // not be the one it started. Saying so is what lets an agent
                // notice it is about to report on somebody else's run.
                "implicit_target": target.is_none(),
        });
        let envelope = match snap.state {
            "done" => crate::machine::envelope("status", true, snap.exit(), body),
            "running" => crate::machine::envelope("status", false, snap.exit(), body),
            _ => crate::machine::error_envelope(
                "status",
                failure_code,
                &failure_message,
                snap.exit(),
                body,
            ),
        };
        crate::machine::emit(&envelope);
        return snap.exit();
    }

    let extra = match snap.state {
        // The idle clocks replace the bare step name when the run dir carries
        // them; a status.json from an older sfh keeps the original line.
        "running" => match idle_segment(&snap) {
            Some(seg) => format!("{seg}, {}s since heartbeat", snap.heartbeat_age_sec),
            None => format!(
                "step '{}', {}s since heartbeat",
                snap.step, snap.heartbeat_age_sec
            ),
        },
        "failed" | "stuck" => snap.error.clone().unwrap_or_default(),
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
        // Human status is one ordered stdout document. Mixing the summary and
        // its next-action hint across stdout/stderr lets log collectors splice
        // the two streams in the middle of a path or line. Scripts should use
        // `status --json`; the human form stays together here.
        "running" => println!(
            "sfh: still running (pid {}); `sfh wait` blocks until it finishes",
            snap.pid
        ),
        "done" => println!(
            "sfh: result: sfh wait {}",
            execute::shell_quote(&snap.dir.display().to_string())
        ),
        // A stuck run is finished but not done with: the work is saved and
        // waiting on a human, so say how to pick it up again.
        "stuck" => println!(
            "sfh: this run stopped for a human decision. after fixing what it is stuck on: sfh run {} --resume {}",
            flow_arg(&snap.flow),
            execute::shell_quote(&snap.dir.display().to_string())
        ),
        "stopped" | "dead" => println!(
            "sfh: this run was killed before it finished. resume with: sfh run {} --resume {}",
            flow_arg(&snap.flow),
            execute::shell_quote(&snap.dir.display().to_string())
        ),
        _ => {}
    }
    snap.exit()
}

/// Cancel a run: kill the process tree and record that it was deliberate, so a
/// later `sfh status` says "stopped" rather than the ambiguous "dead".
pub fn stop(target: Option<&Path>, root: &Path, as_json: bool) -> i32 {
    let dir = match resolve(target, root) {
        Ok(d) => d,
        Err(e) => return fail(as_json, "stop", crate::machine::ErrorCode::Usage, &e),
    };
    let snap = match read(&dir) {
        Ok(s) => s,
        Err(e) => return fail(as_json, "stop", crate::machine::ErrorCode::Usage, &e),
    };
    // Every early return below reports through this, so JSON mode cannot end up
    // with a bare message on stderr and nothing on stdout.
    let answer = |ok: bool, code: i32, state: &str, note: &str| -> i32 {
        if as_json {
            let body = serde_json::json!({
                "state": state,
                "terminal": true,
                "run_id": dir.file_name().map(|n| n.to_string_lossy().into_owned()),
                "run_dir": dir.display().to_string(),
                "pid": snap.pid,
                "implicit_target": target.is_none(),
                "note": note,
            });
            if ok {
                crate::machine::emit(&crate::machine::envelope("stop", true, code, body));
            } else {
                crate::machine::emit(&crate::machine::error_envelope(
                    "stop",
                    crate::machine::ErrorCode::Usage,
                    note,
                    code,
                    body,
                ));
            }
        } else {
            eprintln!("sfh: {note}");
        }
        code
    };
    // A stale heartbeat resolves the state to "dead", but when the pid is
    // still alive that is exactly the wedged process - or one that just came
    // back from suspend - that most needs stopping. "Already dead" would leave
    // nothing but a manual kill. A genuinely gone pid stays terminal. The
    // ownership verification below still runs, so a reused pid is not killed
    // on this path either.
    let alive = execute::pid_alive(snap.pid);
    if snap.terminal() && !(snap.state == "dead" && alive) {
        return answer(
            true,
            0,
            snap.state,
            &format!(
                "nothing to stop - this run is already '{}'. run dir: {}",
                snap.state,
                dir.display()
            ),
        );
    }
    if snap.state == "dead" && alive {
        eprintln!(
            "sfh: {} reads as dead ({}) but process {} is still alive; verifying ownership before stopping it",
            dir.display(),
            snap.reason.as_deref().unwrap_or("no heartbeat"),
            snap.pid
        );
    }
    if !verify_run_ownership(&dir, &snap) {
        return answer(
            false,
            1,
            snap.state,
            "refusing to stop: this run dir's ownership could not be verified",
        );
    }
    if !execute::kill_pid_tree(snap.pid) {
        return answer(
            false,
            1,
            snap.state,
            &format!(
                "could not kill process {} (already gone?); check: sfh status {}",
                snap.pid,
                execute::shell_quote(&dir.display().to_string())
            ),
        );
    }
    // The run dies without a chance to record anything, so write the verdict
    // here. Cost and progress stay as of the last heartbeat, which is honest:
    // they are what was actually spent before the kill. The read is contained
    // and no-follow like every other status.json access (rev_break #6).
    let sp = dir.join("status.json");
    let text = match contain::read_contained_opt(&dir, "status.json") {
        Ok(Some(text)) => text,
        Ok(None) => {
            return answer(
                false,
                1,
                "stopped",
                &format!(
                    "killed pid {} but cannot record the stop: status.json disappeared",
                    snap.pid
                ),
            )
        }
        Err(e) => {
            return answer(
                false,
                1,
                "stopped",
                &format!(
                    "killed pid {} but cannot safely read status.json to record the stop: {e}",
                    snap.pid
                ),
            )
        }
    };
    let mut v = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) => v,
        Err(e) => {
            return answer(
                false,
                1,
                "stopped",
                &format!(
                    "killed pid {} but cannot record the stop: unreadable status.json: {e}",
                    snap.pid
                ),
            )
        }
    };
    let Some(m) = v.as_object_mut() else {
        return answer(
            false,
            1,
            "stopped",
            &format!(
                "killed pid {} but cannot record the stop: status.json is not an object",
                snap.pid
            ),
        );
    };
    m.insert("state".into(), serde_json::json!("stopped"));
    m.insert("exit_code".into(), serde_json::json!(1));
    m.insert("error".into(), serde_json::json!("cancelled by sfh stop"));
    m.insert(
        "error_code".into(),
        serde_json::json!(crate::machine::ErrorCode::Interrupted.as_str()),
    );
    let encoded = serde_json::to_string_pretty(&v).unwrap_or_default();
    if let Err(e) = contain::write_private_atomic(&sp, encoded) {
        return answer(
            false,
            1,
            "stopped",
            &format!(
                "killed pid {} but cannot persist the stopped status: {e}",
                snap.pid
            ),
        );
    }
    if as_json {
        return answer(
            true,
            0,
            "stopped",
            &format!("killed pid {} and its children", snap.pid),
        );
    }
    println!("stopped {}", dir.display());
    println!(
        "sfh: killed pid {} and its children. ${:.4} was spent before the stop. resume with: sfh run {} --resume {}",
        snap.pid,
        snap.cost_usd,
        flow_arg(&snap.flow),
        execute::shell_quote(&dir.display().to_string())
    );
    0
}

/// Consistency between status.json and the run dir's sfh-nonce file, shared by
/// `sfh stop` (before killing), `sfh wait` (before trusting a terminal "done")
/// and `sfh status` (before reporting a terminal state). Returns Ok(()) when
/// the two agree, Err when they do not.
///
/// Runs started by an OLDER sfh have no nonce on either side; that is a
/// recognised legacy format and reads as consistent ONLY when the directory is
/// visibly a real run: a NON-EMPTY log.jsonl that is a regular file (not a
/// symlink). A bare status.json is trivial to forge, an EMPTY log.jsonl next
/// to it is just as trivial (rev_break #10), and a linked one would point the
/// check at a file outside the run dir (rev_break #6) - none are enough (R-3).
pub(crate) fn nonce_consistent(dir: &Path, snap: &Snapshot) -> Result<(), String> {
    // Contained, no-follow read: a symlink planted at the fixed name used to be
    // followed to an attacker-chosen file whose contents then authenticated the
    // run (rev_break #6). Missing reads as None; a containment violation or an
    // unreadable file is a hard error - fail CLOSED (rev_break #9: the old
    // `.ok()` swallowed every failure as "no nonce file").
    let file_raw = match contain::read_contained_opt(dir, "sfh-nonce") {
        Ok(Some(s)) => {
            let t = s.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        }
        Ok(None) => None,
        Err(e) => return Err(format!("cannot verify the run dir's sfh-nonce file: {e}")),
    };
    match (&file_raw, &snap.nonce) {
        (Some(raw), Some(s)) if !s.is_empty() => {
            // A malformed nonce file is an ERROR, not a fallback: the old
            // parser returned no pid on an unparseable one and the pid check
            // was then skipped, so a corrupted/crafted file failed OPEN
            // (rev_break #9).
            let (file_pid, file_start, file_nonce) = match contain::parse_nonce(raw)? {
                contain::Nonce::Bound { pid, start, nonce } => (Some(pid), start, nonce),
                contain::Nonce::Legacy { nonce } => (None, None, nonce),
            };
            if &file_nonce != s {
                return Err(
                    "the nonce in status.json does not match the run dir's sfh-nonce file \
                     (status.json was tampered with, or the dir was copied from another run)"
                        .to_string(),
                );
            }
            if let Some(fp) = file_pid {
                if fp != snap.pid {
                    return Err(format!(
                        "status.json names pid {} but the nonce belongs to pid {} \
                         (status.json was rewritten to point at another process)",
                        snap.pid, fp
                    ));
                }
            }
            // Start-time binding (rev_break #8): when BOTH sides recorded a
            // process start time they must agree, so the nonce of a run whose
            // process died cannot authenticate a reused pid. A side without a
            // recorded time (a nonce from the first pid-binding version, or a
            // legacy bare nonce) keeps the older (pid, nonce) check; the live
            // start-time comparison in verify_run_ownership covers `sfh stop`.
            if let (Some(fs), Some(ss)) = (file_start, snap.pid_start) {
                if fs != ss {
                    return Err(format!(
                        "status.json records process start {ss} but the nonce belongs to start {fs} \
                         (the original process is gone; this pid was reused or the dir was copied)"
                    ));
                }
            }
            Ok(())
        }
        (None, None) => {
            let log = dir.join("log.jsonl");
            let regular = log.symlink_metadata().map(|m| m.is_file()).unwrap_or(false);
            let nonempty = regular && log.metadata().map(|m| m.len() > 0).unwrap_or(false);
            if nonempty {
                Ok(())
            } else {
                Err(
                    "no nonce and no usable log.jsonl (missing, empty or a symlink) - not recognisable as an sfh run dir"
                        .to_string(),
                )
            }
        }
        _ => Err(
            "nonce present on only one side (status.json or the run dir was tampered with)"
                .to_string(),
        ),
    }
}

/// Whether the process that most recently owned this run dir is verifiably
/// gone, read directly from sfh-nonce rather than status.json. The two are
/// consulted for different reasons: status.json is cheap and, when it can be
/// read, already pairs the pid with a fresh heartbeat, which is the better
/// signal (see `read_once`'s "wedged" handling one layer up in every
/// caller). This function exists for the moment that fails: status.json
/// missing, corrupt, or caught mid-write, with nothing else durable to ask
/// (see the `--carry-budget-from` finality check in `engine::run`, the
/// only caller).
///
/// `Ok(None)`: no usable nonce to check against - a legacy pre-nonce run,
/// or a dry-run dir. There is nothing here to verify liveness from, so the
/// caller must find its proof elsewhere (or refuse).
///
/// `Ok(Some(true))`: the recorded owner is confirmed gone - either its pid
/// is not running at all, or a live pid at that exact number started at a
/// different time than recorded, which means the OS handed the number to
/// an unrelated process and the original is still gone (rev_break #8).
///
/// `Ok(Some(false))`: the recorded owner - or something that cannot be
/// told apart from it - is still alive. This is also the answer when a
/// live pid is found but the nonce records no start time to compare: an
/// old pid-binding format did not record one, and pid_alive alone is
/// advisory (see the module doc comment), so a live pid there is treated
/// as possibly still the owner rather than confirmed dead - the same
/// fail-closed default `wedged` uses when it cannot compute a verdict
/// either.
pub fn owner_verifiably_dead(dir: &Path) -> Result<Option<bool>, String> {
    let raw = match contain::read_contained_opt(dir, "sfh-nonce")? {
        Some(s) if !s.trim().is_empty() => s,
        _ => return Ok(None),
    };
    let (pid, start) = match contain::parse_nonce(raw.trim())? {
        contain::Nonce::Bound { pid, start, .. } => (pid, start),
        // No pid recorded at all: a run from before pid binding existed.
        // Nothing here to check liveness against.
        contain::Nonce::Legacy { .. } => return Ok(None),
    };
    if !execute::pid_alive(pid) {
        return Ok(Some(true));
    }
    Ok(match start {
        None => Some(false),
        Some(recorded) => Some(execute::pid_start_time(pid) != Some(recorded)),
    })
}

/// Verify that a run directory genuinely belongs to sfh before killing its pid.
/// Checks: (1) the nonce in status.json matches the nonce file in the run dir,
/// (2) the pid recorded in the nonce file (when present) matches status.json,
/// so a status.json rewritten to point at somebody else's process is refused,
/// (3) the target process is actually this sfh executable. Any failure is fatal.
///
/// Residual bound (rev_break #6): the nonce lives in the SAME directory an
/// attacker controls on a forged run, so a same-user attacker who can write both
/// files can make them agree and point at any pid. The checks below therefore do
/// not prove "this dir owns that process" against such an attacker; they prove
/// the weaker but still useful facts that (a) the dir was not copied/rewritten
/// from another run by mistake, and (b) the target is a same-named sfh binary,
/// never an unrelated process. The real boundary is the runs root being 0700
/// (protect_runs_root): a local attacker who can plant a dir there is already
/// the user. Full defence against pid reuse / a same-user forger would require
/// recording and comparing the process start time, which sfh does not keep; the
/// file-stem (not full-path) binary match is deliberate - a detached run is a
/// copy of the current binary, so an exact stem is the expected identity and a
/// full-path compare would break a binary that moved between launch and stop.
fn verify_run_ownership(dir: &Path, snap: &Snapshot) -> bool {
    if let Err(e) = nonce_consistent(dir, snap) {
        eprintln!("sfh: refusing to stop {}: {e}", dir.display());
        return false;
    }
    if snap.nonce.is_none() {
        eprintln!(
            "sfh: warning: {} was started by an older sfh that wrote no stop nonce; verifying the process itself instead",
            dir.display()
        );
    }
    // Live start-time check (rev_break #8): the process this pid names must be
    // the very one the run recorded. A pid reused after the run died names a
    // process with a DIFFERENT start time even though it passes the (pid,
    // nonce) file check and the executable-name check (a reused pid running
    // another sfh); refuse before killing anything. Skipped only when the run
    // predates start-time recording or the OS cannot answer (pid_start_time).
    if let Some(recorded) = snap.pid_start {
        if execute::pid_alive(snap.pid) {
            if let Some(live) = execute::pid_start_time(snap.pid) {
                if live != recorded {
                    eprintln!(
                        "sfh: refusing to kill pid {}: it started at {live} but this run recorded {recorded} \
                         (the pid was reused by an unrelated process)",
                        snap.pid
                    );
                    return false;
                }
            }
        }
    }
    if !execute::pid_is_sfh(snap.pid) {
        eprintln!(
            "sfh: refusing to kill pid {}: it is not running the same sfh executable as this one",
            snap.pid
        );
        return false;
    }
    true
}

/// Print what a foreground run would have printed to stdout.
/// The detached child's stdout was captured verbatim, so prefer that; fall back
/// to the emitted step's chain file for runs that were never detached.
///
/// Every path is canonicalized and required to stay under the run dir BEFORE it
/// is read, and the read itself does not follow a link on the final component:
/// status.json is attacker-reachable, and so is the run dir's own file table -
/// a symlink planted at the fixed name detached.out.txt used to print an
/// arbitrary file, and the old is_under-then-read_to_string pair re-resolved
/// the path for the read, leaving a window between check and use (rev_break #5).
/// A violation is an error, not a silent skip, so `sfh wait` can exit non-zero
/// instead of reporting success (S1-1).
/// Read a recorded emit file for the JSON path, under exactly the containment
/// `print_result` applies: a status.json an attacker can write must not turn
/// `wait --json` into a file-read primitive either.
fn read_emit_file(snap: &Snapshot, f: &str) -> Result<String, String> {
    let fp = Path::new(f);
    let fp = if fp.is_absolute() {
        fp.to_path_buf()
    } else {
        snap.dir.join(fp)
    };
    match contain::read_contained_abs(&snap.dir, &fp) {
        Ok(Some(t)) => Ok(t.trim_end().to_string()),
        Ok(None) => Err(format!("'{f}' is missing")),
        Err(e) => Err(format!("refused to read '{f}': {e}")),
    }
}

fn print_result(snap: &Snapshot) -> Result<(), String> {
    let detached = snap.dir.join("detached.out.txt");
    match contain::read_contained_abs(&snap.dir, &detached) {
        Ok(Some(t)) if !t.trim().is_empty() => {
            let mut o = std::io::stdout();
            let _ = o.write_all(t.as_bytes());
            let _ = o.flush();
            return Ok(());
        }
        Ok(_) => {}
        Err(e) => return Err(format!("refused to emit '{}': {e}", detached.display())),
    }
    if let Some(f) = &snap.emit_file {
        // Recorded emit paths are absolute, but a forged status.json can carry
        // a relative one: anchor it to the run dir before the containment check
        // instead of letting it resolve against the caller's cwd.
        let fp = Path::new(f);
        let fp = if fp.is_absolute() {
            fp.to_path_buf()
        } else {
            snap.dir.join(fp)
        };
        match contain::read_contained_abs(&snap.dir, &fp) {
            Ok(Some(t)) => println!("{}", t.trim_end()),
            Ok(None) => {}
            Err(e) => return Err(format!("refused to emit '{f}': {e}")),
        }
    }
    Ok(())
}

pub fn wait(
    target: Option<&Path>,
    root: &Path,
    timeout_sec: Option<u64>,
    interval_sec: u64,
    quiet: bool,
    as_json: bool,
) -> i32 {
    let dir = match resolve(target, root) {
        Ok(d) => d,
        Err(e) => return fail(as_json, "wait", crate::machine::ErrorCode::Usage, &e),
    };
    let started = SystemTime::now();
    let interval = Duration::from_secs(interval_sec.max(1));
    let mut last_step = String::new();
    loop {
        let snap = match read(&dir) {
            Ok(s) => s,
            Err(e) => return fail(as_json, "wait", crate::machine::ErrorCode::Usage, &e),
        };
        if snap.terminal() {
            if as_json {
                // The nonce check happens for JSON callers too, and BEFORE any
                // content is read: a run dir an attacker can write must not
                // become a file-read primitive just because the caller asked
                // for a machine answer.
                if let Err(e) = nonce_consistent(&dir, &snap) {
                    return fail(
                        as_json,
                        "wait",
                        crate::machine::ErrorCode::PersistenceFailure,
                        &format!(
                            "refusing to report {} as '{}': {e}",
                            dir.display(),
                            snap.state
                        ),
                    );
                }
                let code = match snap.state {
                    "done" => snap
                        .exit_code
                        .map(|c| i32::try_from(c).unwrap_or(1))
                        .unwrap_or(0),
                    _ => snap.exit(),
                };
                let result = snap
                    .emit_file
                    .as_deref()
                    .and_then(|f| read_emit_file(&snap, f).ok());
                let body = serde_json::json!({
                    "state": snap.state,
                    "terminal": true,
                    "run_id": snap.dir.file_name().map(|n| n.to_string_lossy().into_owned()),
                    "run_dir": snap.dir.display().to_string(),
                    "flow": snap.flow,
                    "implicit_target": target.is_none(),
                    "result": result,
                    "result_step": snap.emit_step,
                    "result_file": snap.emit_file,
                    "cost_usd": snap.cost_usd,
                    "steps_done": snap.steps_done,
                });
                if snap.state == "done" {
                    crate::machine::emit(&crate::machine::envelope("wait", true, code, body));
                } else {
                    crate::machine::emit(&crate::machine::error_envelope(
                        "wait",
                        snapshot_error_code(&snap),
                        snap.error.as_deref().unwrap_or("the run did not finish"),
                        code,
                        body,
                    ));
                }
                return code;
            }
            // Do not report anything an untrusted run dir asserted about
            // itself until the nonce backs it up - and for EVERY terminal
            // state, not just done. The check used to live inside the "done"
            // arm, so a forged status.json that said `failed` still reached
            // print_result and emitted the file it named: a run dir an
            // attacker can write became a file-read primitive, with the
            // non-zero exit code making it look like the refusal had worked.
            // The same applies to dead/stopped, which report content too.
            //
            // Before the match, so no arm can be added later that forgets.
            if let Err(e) = nonce_consistent(&dir, &snap) {
                eprintln!(
                    "sfh: refusing to report {} as '{}': {e}",
                    dir.display(),
                    snap.state
                );
                return 1;
            }
            match snap.state {
                "done" => {
                    if let Err(e) = print_result(&snap) {
                        eprintln!("sfh: {e}");
                        return 1;
                    }
                }
                "failed" => {
                    eprintln!(
                        "sfh: FLOW FAILED: {}",
                        snap.error.as_deref().unwrap_or("(no error recorded)")
                    );
                    if let Err(e) = print_result(&snap) {
                        eprintln!("sfh: {e}");
                        return 1;
                    }
                    eprintln!("sfh: run dir: {}", snap.dir.display());
                }
                // Same shape as "failed": the caller gets the partial result,
                // because a stuck run has produced real work and the point of
                // the exit code is to say who has to look at it next.
                "stuck" => {
                    eprintln!(
                        "sfh: FLOW STUCK: {}",
                        snap.error.as_deref().unwrap_or("(no reason recorded)")
                    );
                    if let Err(e) = print_result(&snap) {
                        eprintln!("sfh: {e}");
                        return 1;
                    }
                    eprintln!(
                        "sfh: run dir: {} - resume it once a human has decided",
                        snap.dir.display()
                    );
                }
                _ => {
                    eprintln!(
                        "sfh: run did not finish ({}). resume with: sfh run {} --resume {}",
                        snap.reason
                            .as_deref()
                            .or(snap.error.as_deref())
                            .unwrap_or("no longer running"),
                        flow_arg(&snap.flow),
                        execute::shell_quote(&snap.dir.display().to_string())
                    );
                }
            }
            // The recorded exit code is honoured ONLY for a nonce-authenticated
            // "done". failed/dead/stopped always exit 1 and stuck always exits
            // 4, whatever status.json claims: exit_code:0 next to a non-done
            // state used to return 0, laundering a forged "stopped" into
            // success (rev_break #10), and a forged "stuck" must not be able to
            // launder itself the same way. The state IS the exit code for those
            // - a real stuck run records 4 - so nothing is lost by deriving it.
            // For "done", anything that does not fit an i32 is treated as a
            // failure (an unchecked `as i32` wrapped 4294967296 to 0 -
            // rev_break #7).
            return match snap.state {
                "done" => snap
                    .exit_code
                    .map(|c| i32::try_from(c).unwrap_or(1))
                    .unwrap_or(0),
                _ => snap.exit(),
            };
        }
        if !quiet && snap.step != last_step {
            eprintln!("sfh: waiting - step '{}'", snap.step);
            last_step = snap.step.clone();
        }
        if let Some(t) = timeout_sec {
            let elapsed = started.elapsed().map(|d| d.as_secs()).unwrap_or(0);
            if elapsed >= t {
                if as_json {
                    // A wait timeout is NOT a cancellation, and the envelope has
                    // to say so plainly: the run is still going, and a caller
                    // that read this as "it failed" would abandon live work.
                    crate::machine::emit(&crate::machine::envelope(
                        "wait",
                        false,
                        3,
                        serde_json::json!({
                            "state": "running",
                            "terminal": false,
                            "timed_out": true,
                            "run_id": dir.file_name().map(|n| n.to_string_lossy().into_owned()),
                            "run_dir": dir.display().to_string(),
                            "implicit_target": target.is_none(),
                            "note": format!("still running after {t}s; the run is NOT cancelled"),
                            "next_actions": [
                                {"kind": "wait", "argv": ["sfh", "wait", &dir.display().to_string(), "--json"]},
                                {"kind": "stop", "argv": ["sfh", "stop", &dir.display().to_string(), "--json"]},
                            ],
                        }),
                    ));
                    return 3;
                }
                eprintln!(
                    "sfh: still running after {t}s; the run is NOT cancelled. check: sfh status {}",
                    execute::shell_quote(&dir.display().to_string())
                );
                return 3;
            }
        }
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sfh-watch-{tag}-{}", contain::random_nonce()));
        contain::mkdir_private(&dir).unwrap();
        dir
    }

    #[test]
    fn owner_verifiably_dead_reads_none_when_no_nonce_file_exists() {
        // Nothing here to verify liveness from - the caller (the
        // `--carry-budget-from` finality check) must find its proof
        // elsewhere, or refuse. This must not be conflated with "confirmed
        // dead": an absent nonce says nothing either way.
        let dir = temp_dir("no-nonce");
        assert_eq!(owner_verifiably_dead(&dir).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owner_verifiably_dead_reads_none_for_a_legacy_bare_nonce_with_no_pid() {
        // A bare token predates pid binding entirely; there is no pid to ask
        // the process table about.
        let dir = temp_dir("legacy-nonce");
        std::fs::write(dir.join("sfh-nonce"), "just-a-token").unwrap();
        assert_eq!(owner_verifiably_dead(&dir).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owner_verifiably_dead_confirms_life_when_the_recorded_pid_and_start_time_are_still_current()
    {
        let dir = temp_dir("alive");
        let pid = std::process::id();
        let start = execute::pid_start_time(pid);
        contain::write_nonce(&dir, pid, start, "tok").unwrap();
        assert_eq!(
            owner_verifiably_dead(&dir).unwrap(),
            Some(false),
            "the calling test process is definitely still running"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owner_verifiably_dead_treats_a_live_pid_with_no_recorded_start_as_unconfirmed() {
        // The first pid-binding format recorded no start time. A live pid
        // there cannot be told apart from the original owner - pid_alive
        // alone is advisory (rev_break #8) - so this must fail closed the
        // same way the "wedged" check does when it cannot compute a
        // verdict: not proven dead.
        let dir = temp_dir("alive-no-start");
        contain::write_nonce(&dir, std::process::id(), None, "tok").unwrap();
        assert_eq!(owner_verifiably_dead(&dir).unwrap(), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owner_verifiably_dead_confirms_death_when_a_live_pid_disagrees_with_the_recorded_start_time()
    {
        // The OS reusing a pid for an unrelated process is exactly what the
        // recorded start time exists to catch (rev_break #8): the number is
        // alive, but not as the process that owned this run.
        let dir = temp_dir("reused-pid");
        contain::write_nonce(&dir, std::process::id(), Some(1), "tok").unwrap();
        assert_eq!(
            owner_verifiably_dead(&dir).unwrap(),
            Some(true),
            "a live pid whose start time does not match the recording is a reused pid, not the original owner"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn owner_verifiably_dead_confirms_death_when_the_recorded_pid_has_actually_exited() {
        let dir = temp_dir("exited");
        let mut child = std::process::Command::new("true")
            .spawn()
            .expect("spawn a trivial child");
        let pid = child.id();
        let start = execute::pid_start_time(pid);
        child.wait().expect("wait for the child to exit");
        contain::write_nonce(&dir, pid, start, "tok").unwrap();
        assert_eq!(owner_verifiably_dead(&dir).unwrap(), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
