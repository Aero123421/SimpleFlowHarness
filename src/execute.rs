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
        // CreateProcess is called with bInheritHandles=TRUE, so without this
        // every child - and every grandchild - gets a duplicate of whatever
        // pipe our caller handed us. Anything reading that pipe then waits for
        // the deepest descendant instead of for sfh, which is how a detached
        // run ends up blocking the very caller it was supposed to free.
        // Redirected stdio is unaffected: Rust marks those handles itself.
        super::windows_no_inherit_std();
        JOB.get_or_init(|| unsafe {
            let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return 0;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            // KILL_ON_JOB_CLOSE and nothing else. JOB_OBJECT_LIMIT_BREAKAWAY_OK
            // looks harmless - "only a deliberate CREATE_BREAKAWAY_FROM_JOB can
            // leave" - but it is not ours to grant: msys2/Git-Bash spawns with
            // that flag, so with BREAKAWAY_OK a `cmd: ["sh", "-c", ...]` step
            // leaves descendants running after sfh is killed. Measured: an
            // orphaned `sleep` survived `sfh stop` with the flag, died without
            // it. The guarantee that nothing outlives sfh is worth more than
            // letting a flow step launch its own detached run.
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

// ---------------------------------------------------------------------------
// Detached launch: the exact opposite of the ownership above. `--detach` hands
// a run to a copy of sfh that must OUTLIVE this process and its caller, so the
// child deliberately leaves the job object / session instead of joining it.
// ---------------------------------------------------------------------------

pub struct Detached {
    pub pid: u32,
    /// Set when the OS refused to let the child leave the caller's job object,
    /// meaning it still dies if the caller's process tree is torn down.
    pub warning: Option<String>,
}

/// stdout/stderr to the given files (truncating), stdin closed: nothing is
/// lost and no console is held open. `env` is set on the child only (the
/// parent's own environment is untouched).
fn detached_command(
    exe: &Path,
    args: &[String],
    out_file: &Path,
    err_file: &Path,
    env: &[(&str, &str)],
) -> Result<Command, String> {
    let mk = |p: &Path| -> Result<std::fs::File, String> {
        let f =
            std::fs::File::create(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
        crate::contain::restrict_file(&f);
        Ok(f)
    };
    let mut c = Command::new(exe);
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    c.stdin(Stdio::null());
    c.stdout(Stdio::from(mk(out_file)?));
    c.stderr(Stdio::from(mk(err_file)?));
    Ok(c)
}

/// Spawn `exe args...` in the background, disowned.
#[cfg(windows)]
pub fn spawn_detached(
    exe: &Path,
    args: &[String],
    out_file: &Path,
    err_file: &Path,
    env: &[(&str, &str)],
) -> Result<Detached, String> {
    use std::os::windows::process::CommandExt;
    // Win32 process-creation flags. CREATE_BREAKAWAY_FROM_JOB is the one that
    // matters: without it the child joins the caller's job object and dies with
    // it, which is exactly what --detach exists to avoid.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const BASE: u32 = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;

    let mut c = detached_command(exe, args, out_file, err_file, env)?;
    c.creation_flags(BASE | CREATE_BREAKAWAY_FROM_JOB);
    match c.spawn() {
        Ok(child) => Ok(Detached {
            pid: child.id(),
            warning: None,
        }),
        // A job without JOB_OBJECT_LIMIT_BREAKAWAY_OK rejects the flag outright.
        // Running anyway beats not running, but say so plainly.
        Err(e) => {
            let mut c = detached_command(exe, args, out_file, err_file, env)?;
            c.creation_flags(BASE);
            let child = c
                .spawn()
                .map_err(|e2| format!("cannot start the detached run: {e2}"))?;
            Ok(Detached {
                pid: child.id(),
                warning: Some(format!(
                    "the calling process is inside a job object that forbids breakaway ({e}); \
                     the run was started anyway but will still be killed if that process tree \
                     is torn down. Note sfh's own job is deliberately one of these, so a flow \
                     step cannot launch a background run that outlives the flow - use --resume \
                     to pick it up instead"
                )),
            })
        }
    }
}

/// Spawn `exe args...` in the background, disowned.
#[cfg(unix)]
pub fn spawn_detached(
    exe: &Path,
    args: &[String],
    out_file: &Path,
    err_file: &Path,
    env: &[(&str, &str)],
) -> Result<Detached, String> {
    use std::os::unix::process::CommandExt;
    let mut c = detached_command(exe, args, out_file, err_file, env)?;
    unsafe {
        // New session: no controlling terminal, so closing the caller's
        // terminal does not SIGHUP the run. Deliberately no PR_SET_PDEATHSIG -
        // this child is meant to outlive us.
        c.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = c
        .spawn()
        .map_err(|e| format!("cannot start the detached run: {e}"))?;
    Ok(Detached {
        pid: child.id(),
        warning: None,
    })
}

/// Every child is a console program whose output sfh captures, so none of them
/// need a console of their own. Without this each step - and every `taskkill` -
/// flashes a cmd window on top of whatever the user is doing, which on a flow
/// with a dozen steps is unusable.
#[cfg(windows)]
fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Clear HANDLE_FLAG_INHERIT on our own standard handles so the next child
/// cannot hold a caller's pipe open. Safe to call late: it does not affect this
/// process's own use of those handles, only what a child inherits.
#[cfg(windows)]
fn windows_no_inherit_std() {
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        unsafe {
            let h = GetStdHandle(id);
            if !h.is_null() {
                SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Kill a process and its descendants by pid. Used by `sfh stop` on a detached
/// run, which by construction is not our child, so `kill_tree` does not apply.
#[cfg(windows)]
pub fn kill_pid_tree(pid: u32) -> bool {
    let mut c = Command::new("taskkill");
    c.args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut c);
    c.status().map(|s| s.success()).unwrap_or(false)
}

/// Kill a process and its descendants by pid. Used by `sfh stop` on a detached
/// run, which by construction is not our child, so `kill_tree` does not apply.
///
/// SIGTERM first, deliberately. Every child sfh spawns gets its OWN session
/// (setsid, so a timeout can kill its subtree), which means the children are
/// not in the detached run's process group. `kill(-pid)` therefore never
/// reaches them: it kills sfh and leaves the agents running. Signalling sfh
/// instead lets its own handler kill each tracked child's group, which is
/// where the grandchildren live. SIGKILL is only the backstop for a process
/// too wedged to handle a signal.
#[cfg(unix)]
pub fn kill_pid_tree(pid: u32) -> bool {
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    for _ in 0..50 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
    !pid_alive(pid)
}

/// Check whether the given pid belongs to a running sfh process.
/// Used by `sfh stop` to avoid killing unrelated processes.
///
/// The ownership question has to work identically on Windows, macOS and Linux,
/// but each OS answers "what executable is this pid running?" differently, so
/// below there is one `pid_exe_path` per OS feeding ONE shared comparison:
/// - Windows: QueryFullProcessImageNameW on a PROCESS_QUERY_LIMITED_INFORMATION
///   handle (no elevated rights needed for one's own processes).
/// - Linux: readlink /proc/<pid>/exe.
/// - macOS: proc_pidpath(3) from libSystem - macOS has NO /proc, so the old
///   readlink path made `sfh stop` fail for every run on macOS.
///
/// The comparison is an EXACT file-stem match against our own executable: a
/// detached run is always a copy of the current binary, so that is precisely
/// the expected name, and a substring match would let `sfh stop` kill an
/// unrelated `sfh-helper` (or, renamed, anything containing "sfh").
#[cfg(windows)]
pub fn pid_is_sfh(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        if ok == 0 {
            return false;
        }
        exe_path_is_ours(&String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Check whether the given pid belongs to a running sfh process (Linux:
/// /proc/<pid>/exe).
#[cfg(target_os = "linux")]
pub fn pid_is_sfh(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(p) => {
            // A binary deleted while running reads as "/path/sfh (deleted)".
            let s = p.to_string_lossy();
            let s = s.strip_suffix(" (deleted)").unwrap_or(&s);
            exe_path_is_ours(s)
        }
        Err(_) => false,
    }
}

/// Check whether the given pid belongs to a running sfh process (macOS:
/// proc_pidpath; there is no /proc).
#[cfg(target_os = "macos")]
pub fn pid_is_sfh(pid: u32) -> bool {
    extern "C" {
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut libc::c_char,
            buffersize: u32,
        ) -> libc::c_int;
    }
    if pid == 0 {
        return false;
    }
    // PROC_PIDPATHINFO_MAXSIZE in <sys/proc_info.h>.
    let mut buf = vec![0 as libc::c_char; 4096];
    let n = unsafe { proc_pidpath(pid as libc::c_int, buf.as_mut_ptr(), buf.len() as u32) };
    if n <= 0 {
        return false;
    }
    let path = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
    exe_path_is_ours(&path)
}

/// Other Unix targets: try procfs if it happens to be mounted, otherwise
/// refuse - sfh's supported platforms are Windows/macOS/Linux, and refusing
/// is the safe direction for one we cannot verify.
#[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
pub fn pid_is_sfh(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(p) => exe_path_is_ours(&p.to_string_lossy()),
        Err(_) => false,
    }
}

/// True when `exe_path` is the same program as the running sfh: an exact,
/// case-insensitive file-stem match (sfh.exe and sfh compare equal; sfh-helper
/// does not). Falls back to the name "sfh" when our own path is unavailable.
fn exe_path_is_ours(exe_path: &str) -> bool {
    let stem = |p: &str| {
        Path::new(p)
            .file_stem()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let target = stem(exe_path);
    if target.is_empty() {
        return false;
    }
    match std::env::current_exe().ok().map(|p| {
        p.file_stem()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }) {
        Some(own) if !own.is_empty() => target == own,
        _ => target == "sfh",
    }
}

/// Is this pid still running? Pid reuse makes it advisory, so callers pair it
/// with a heartbeat freshness check before declaring a run dead.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if unsafe { libc::kill(pid as i32, 0) } == 0 {
        return true;
    }
    // EPERM: it exists, it just is not ours to signal.
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Is this pid still running? Pid reuse makes it advisory, so callers pair it
/// with a heartbeat freshness check before declaring a run dead.
#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    if pid == 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE
    }
}

pub fn run_cmd(
    inv: &Invocation,
    stdin_data: Option<Vec<u8>>,
    cwd: Option<&Path>,
    timeout: Option<Duration>,
    env_remove: &[String],
    env_set: &[(String, String)],
    tee_stdout: Option<std::path::PathBuf>,
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
    #[cfg(windows)]
    no_window(&mut cmd);

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
    let rx_out = spawn_reader(child.stdout.take().expect("stdout piped"), tee_stdout);
    let rx_err = spawn_reader(child.stderr.take().expect("stderr piped"), None);

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

/// `tee`: mirror each chunk to this file as it arrives. Without it nothing is
/// observable until the child exits, so a 30-minute step is indistinguishable
/// from a hung one. The file is rewritten with the cleaned text at the end, so
/// what is visible mid-run is the raw stream and what remains is canonical.
fn spawn_reader<R: Read + Send + 'static>(
    mut r: R,
    tee: Option<std::path::PathBuf>,
) -> std::sync::mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = tee.and_then(|p| {
            let f = std::fs::File::create(p).ok()?;
            crate::contain::restrict_file(&f);
            Some(f)
        });
        let mut buf = Vec::new();
        let mut tmp = [0u8; 65536];
        let mut truncated = false;
        loop {
            match r.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < MAX_CAPTURE {
                        let take = n.min(MAX_CAPTURE - buf.len());
                        if let Some(f) = sink.as_mut() {
                            // Same cap as the in-memory capture, so a runaway
                            // tool cannot fill the disk through this path.
                            let _ = f.write_all(&tmp[..take]);
                            let _ = f.flush();
                        }
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
/// Best-effort `--version` for the provenance record. Bounded: a CLI that
/// hangs on --version (an auth prompt, a stuck update check) must not stop the
/// run from starting - this is metadata, not a dependency.
pub fn probe_version(program: &str) -> Option<String> {
    let out = run_cmd(
        &Invocation::Argv(vec![program.to_string(), "--version".to_string()]),
        None,
        None,
        Some(Duration::from_secs(15)),
        &[],
        &[],
        None,
    )
    .ok()?;
    if out.timed_out {
        return None;
    }
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
    let mut tk = Command::new("taskkill");
    tk.args(["/PID", &pid, "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut tk);
    let ok = tk.status().map(|s| s.success()).unwrap_or(false);
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
