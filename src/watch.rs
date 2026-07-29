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
    pub flow: String,
    pub started: String,
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
            .filter(|p| p.join("status.json").exists())
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
        flow: s("flow"),
        started: s("started_utc"),
        nonce: opt("nonce"),
        pid_start: v.get("pid_start").and_then(|x| x.as_u64()),
    })
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
    // A terminal state an untrusted run dir asserts about itself is not reported
    // as fact: the same nonce authentication `sfh wait` and `sfh stop` run must
    // back it. A forged status.json that says "done" without a matching nonce
    // used to print as success and exit 0; it is now refused (rev_break #10).
    if snap.terminal() {
        if let Err(e) = nonce_consistent(&dir, &snap) {
            eprintln!(
                "sfh: refusing to report {} as '{}': {e}",
                dir.display(),
                snap.state
            );
            return 1;
        }
    }
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
        "running" => eprintln!(
            "sfh: still running (pid {}); `sfh wait` blocks until it finishes",
            snap.pid
        ),
        "done" => eprintln!(
            "sfh: result: sfh wait {}",
            execute::shell_quote(&snap.dir.display().to_string())
        ),
        // A stuck run is finished but not done with: the work is saved and
        // waiting on a human, so say how to pick it up again.
        "stuck" => eprintln!(
            "sfh: this run stopped for a human decision. after fixing what it is stuck on: sfh run {} --resume {}",
            flow_arg(&snap.flow),
            execute::shell_quote(&snap.dir.display().to_string())
        ),
        "stopped" | "dead" => eprintln!(
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
    // A stale heartbeat resolves the state to "dead", but when the pid is
    // still alive that is exactly the wedged process - or one that just came
    // back from suspend - that most needs stopping. "Already dead" would leave
    // nothing but a manual kill. A genuinely gone pid stays terminal. The
    // ownership verification below still runs, so a reused pid is not killed
    // on this path either.
    let alive = execute::pid_alive(snap.pid);
    if snap.terminal() && !(snap.state == "dead" && alive) {
        eprintln!(
            "sfh: nothing to stop - this run is already '{}'. run dir: {}",
            snap.state,
            dir.display()
        );
        return 0;
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
        return 1;
    }
    if !execute::kill_pid_tree(snap.pid) {
        eprintln!(
            "sfh: could not kill process {} (already gone?); check: sfh status {}",
            snap.pid,
            execute::shell_quote(&dir.display().to_string())
        );
        return 1;
    }
    // The run dies without a chance to record anything, so write the verdict
    // here. Cost and progress stay as of the last heartbeat, which is honest:
    // they are what was actually spent before the kill. The read is contained
    // and no-follow like every other status.json access (rev_break #6).
    let sp = dir.join("status.json");
    if let Ok(Some(text)) = contain::read_contained_opt(&dir, "status.json") {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(m) = v.as_object_mut() {
                m.insert("state".into(), serde_json::json!("stopped"));
                m.insert("exit_code".into(), serde_json::json!(1));
                m.insert("error".into(), serde_json::json!("cancelled by sfh stop"));
                let _ = contain::write_private(
                    &sp,
                    serde_json::to_string_pretty(&v).unwrap_or_default(),
                );
            }
        }
    }
    println!("stopped {}", dir.display());
    eprintln!(
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
fn nonce_consistent(dir: &Path, snap: &Snapshot) -> Result<(), String> {
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
