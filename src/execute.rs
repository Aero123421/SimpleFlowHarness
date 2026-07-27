use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub enum Invocation {
    /// Spawned directly, no shell. argv[0] is resolved against PATH
    /// (on Windows also tries .exe/.cmd/.bat so npm shims work).
    Argv(Vec<String>),
    /// Run through `cmd /C` (Windows) or `sh -c` (Unix).
    Shell(String),
}

impl Invocation {
    pub fn describe(&self) -> String {
        match self {
            Invocation::Argv(a) => a
                .iter()
                .map(|s| {
                    if s.contains(' ') || s.is_empty() {
                        format!("\"{s}\"")
                    } else {
                        s.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            Invocation::Shell(s) => format!("$ {s}"),
        }
    }
}

pub struct ExecOutcome {
    pub exit_code: i32,
    pub timed_out: bool,
    pub interrupted: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub dur_ms: u128,
}

// ---------------------------------------------------------------------------
// Process-tree ownership: children must never outlive sfh.
//
// Windows: every child joins a job object created with KILL_ON_JOB_CLOSE, so
// the OS reaps the whole tree even if sfh is force-killed.
// Unix: each child gets its own session (so a timeout can kill the tree) plus,
// on Linux, PR_SET_PDEATHSIG=SIGKILL. Ctrl+C/SIGTERM are handled by a signal
// handler that kills every registered process group.
// ---------------------------------------------------------------------------

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
const MAX_TRACKED: usize = 512;
static TRACKED: [AtomicI32; MAX_TRACKED] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const Z: AtomicI32 = AtomicI32::new(0);
    [Z; MAX_TRACKED]
};

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

fn track(pid: i32) {
    for slot in TRACKED.iter() {
        if slot
            .compare_exchange(0, pid, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }
    }
}

fn untrack(pid: i32) {
    for slot in TRACKED.iter() {
        let _ = slot.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst);
    }
}

/// Install signal/console handlers and (on Windows) the kill-on-close job.
/// Call once from main before any child is spawned.
pub fn install_process_guard() {
    #[cfg(windows)]
    windows_guard::install();
    #[cfg(unix)]
    unix_guard::install();
}

#[cfg(unix)]
mod unix_guard {
    use super::*;

    extern "C" fn on_signal(_sig: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
        // Async-signal-safe: only kill(2) on a lock-free pid table.
        for slot in TRACKED.iter() {
            let pid = slot.load(Ordering::SeqCst);
            if pid > 0 {
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
            }
        }
    }

    pub fn install() {
        // Go through a typed fn pointer: casting the fn *item* straight to an
        // integer is a clippy error and easy to get wrong.
        let handler: extern "C" fn(libc::c_int) = on_signal;
        let handler = handler as libc::sighandler_t;
        unsafe {
            libc::signal(libc::SIGINT, handler);
            libc::signal(libc::SIGTERM, handler);
            libc::signal(libc::SIGHUP, handler);
        }
    }
}

#[cfg(windows)]
mod windows_guard {
    use super::*;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{BOOL, HANDLE};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    static JOB: OnceLock<usize> = OnceLock::new();

    unsafe extern "system" fn ctrl_handler(_kind: u32) -> BOOL {
        INTERRUPTED.store(true, Ordering::SeqCst);
        // The job object tears the tree down when sfh exits; flag and let the
        // run loop unwind so the run dir gets a proper failure record.
        1
    }

    pub fn install() {
        unsafe {
            SetConsoleCtrlHandler(Some(ctrl_handler), 1);
        }
        JOB.get_or_init(|| unsafe {
            let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            job as usize
        });
    }

    pub fn adopt(child: &Child) {
        use std::os::windows::io::AsRawHandle;
        let Some(&job) = JOB.get() else { return };
        if job == 0 {
            return;
        }
        unsafe {
            AssignProcessToJobObject(job as HANDLE, child.as_raw_handle() as HANDLE);
        }
    }
}

pub fn run_cmd(
    inv: &Invocation,
    stdin_data: Option<Vec<u8>>,
    cwd: Option<&Path>,
    timeout: Option<Duration>,
    env_remove: &[String],
    env_set: &[(String, String)],
) -> Result<ExecOutcome, String> {
    if interrupted() {
        return Err("interrupted before start".into());
    }
    let mut cmd = match inv {
        Invocation::Argv(argv) => {
            if argv.is_empty() {
                return Err("empty command".into());
            }
            let mut c = Command::new(resolve_program(&argv[0]));
            c.args(&argv[1..]);
            c
        }
        Invocation::Shell(line) => shell_command(line),
    };
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    for k in env_remove {
        cmd.env_remove(k);
    }
    for (k, v) in env_set {
        cmd.env(k, v);
    }
    cmd.stdin(if stdin_data.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // New session on Unix so a timeout can kill the whole tree, plus
    // parent-death signalling on Linux.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            #[cfg(target_os = "linux")]
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0);
            Ok(())
        });
    }

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn [{}]: {e}", inv.describe()))?;
    #[cfg(windows)]
    windows_guard::adopt(&child);
    let pid = child.id() as i32;
    track(pid);

    let stdin_thread = stdin_data.map(|data| {
        let mut si = child.stdin.take().expect("stdin piped");
        std::thread::spawn(move || {
            let _ = si.write_all(&data);
            // handle dropped here -> child sees EOF
        })
    });
    let rx_out = spawn_reader(child.stdout.take().expect("stdout piped"));
    let rx_err = spawn_reader(child.stderr.take().expect("stderr piped"));

    let mut timed_out = false;
    let mut was_interrupted = false;
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
        }
        if interrupted() {
            was_interrupted = true;
            kill_tree(&mut child);
            break child.wait().map_err(|e| e.to_string())?;
        }
        if let Some(t) = timeout {
            if start.elapsed() > t {
                timed_out = true;
                kill_tree(&mut child);
                break child.wait().map_err(|e| e.to_string())?;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    untrack(pid);

    // Drain the pipes with a deadline: a grandchild that inherited the write
    // ends can otherwise hold them open forever after the child exited.
    let drain_budget = timeout
        .map(|t| t.saturating_sub(start.elapsed()))
        .unwrap_or(Duration::from_secs(300))
        .max(Duration::from_secs(5));
    let drain_start = Instant::now();
    let mut drain_expired = false;
    let mut recv = |rx: &std::sync::mpsc::Receiver<(Vec<u8>, bool)>| -> (Vec<u8>, bool) {
        let left = drain_budget.saturating_sub(drain_start.elapsed());
        match rx.recv_timeout(left.max(Duration::from_millis(50))) {
            Ok(v) => v,
            Err(_) => {
                drain_expired = true;
                (Vec::new(), false)
            }
        }
    };
    let (stdout, out_trunc) = recv(&rx_out);
    let (mut stderr, err_trunc) = recv(&rx_err);
    if out_trunc || err_trunc {
        stderr.extend_from_slice(
            format!(
                "\n[sfh: captured output truncated at {} MB]\n",
                MAX_CAPTURE / 1024 / 1024
            )
            .as_bytes(),
        );
    }
    if drain_expired {
        stderr.extend_from_slice(
            b"\n[sfh: output drain timed out; a background grandchild process may still hold the pipe]\n",
        );
    }
    if let Some(t) = stdin_thread {
        let _ = t.join();
    }
    Ok(ExecOutcome {
        exit_code: status.code().unwrap_or(-1),
        timed_out,
        interrupted: was_interrupted,
        stdout,
        stderr,
        dur_ms: start.elapsed().as_millis(),
    })
}

/// Per-stream capture cap; the reader keeps draining past it (discarding) so
/// the child never blocks on a full pipe.
const MAX_CAPTURE: usize = 32 * 1024 * 1024;

fn spawn_reader<R: Read + Send + 'static>(mut r: R) -> std::sync::mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 65536];
        let mut truncated = false;
        loop {
            match r.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < MAX_CAPTURE {
                        let take = n.min(MAX_CAPTURE - buf.len());
                        buf.extend_from_slice(&tmp[..take]);
                        if take < n {
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
            }
        }
        let _ = tx.send((buf, truncated));
    });
    rx
}

/// Capture `<program> --version` (no AI call) for the run's provenance record.
pub fn probe_version(program: &str) -> Option<String> {
    let out = Command::new(resolve_program(program))
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(if out.stdout.is_empty() {
        &out.stderr
    } else {
        &out.stdout
    });
    text.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
}

#[cfg(windows)]
fn shell_command(line: &str) -> Command {
    use std::os::windows::process::CommandExt;
    let mut c = Command::new("cmd");
    c.raw_arg("/C");
    c.raw_arg(line);
    c
}

#[cfg(not(windows))]
fn shell_command(line: &str) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(line);
    c
}

/// On Windows, Command::new("foo") only finds foo.exe. npm-installed CLIs are .cmd
/// shims, so resolve by hand across PATH with common extensions.
#[cfg(windows)]
fn resolve_program(name: &str) -> String {
    let p = Path::new(name);
    if name.contains('/') || name.contains('\\') || p.extension().is_some() {
        return name.to_string();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            for ext in ["exe", "cmd", "bat"] {
                let cand = dir.join(format!("{name}.{ext}"));
                if cand.is_file() {
                    return cand.to_string_lossy().into_owned();
                }
            }
        }
    }
    name.to_string()
}

#[cfg(not(windows))]
fn resolve_program(name: &str) -> String {
    name.to_string()
}

#[cfg(windows)]
fn kill_tree(child: &mut Child) {
    let pid = child.id().to_string();
    let ok = Command::new("taskkill")
        .args(["/PID", &pid, "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        let _ = child.kill();
    }
}

#[cfg(unix)]
fn kill_tree(child: &mut Child) {
    unsafe {
        // Negative pid = the process group created via setsid.
        if libc::kill(-(child.id() as i32), libc::SIGKILL) != 0 {
            let _ = child.kill();
        }
    }
}

/// Classify a failure as transient (worth retrying) from the tool's own output.
pub fn is_transient_failure(stderr: &str, stdout: &str) -> bool {
    const NEEDLES: [&str; 18] = [
        "429",
        "rate limit",
        "rate_limit",
        "ratelimit",
        "too many requests",
        "overloaded",
        "server_error",
        "internal server error",
        "502",
        "503",
        "504",
        "temporarily unavailable",
        "connection reset",
        "econnreset",
        "socket hang up",
        "fetch failed",
        "network error",
        "reconnecting",
    ];
    let hay = format!("{stderr}\n{stdout}").to_lowercase();
    NEEDLES.iter().any(|n| hay.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_transient_failures() {
        assert!(is_transient_failure("HTTP 429 Too Many Requests", ""));
        assert!(is_transient_failure("", "Error: overloaded_error"));
        assert!(is_transient_failure("ERROR: Reconnecting... 3/5", ""));
        assert!(!is_transient_failure("SyntaxError: unexpected token", ""));
        assert!(!is_transient_failure("", "the model refused"));
    }

    #[test]
    fn describes_invocations() {
        let a = Invocation::Argv(vec!["tool".into(), "--flag".into(), "two words".into()]);
        assert_eq!(a.describe(), "tool --flag \"two words\"");
        assert_eq!(Invocation::Shell("echo hi".into()).describe(), "$ echo hi");
    }
}
