use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub enum Invocation {
    /// Spawned directly, no shell. argv[0] is resolved against PATH
    /// (on Windows also tries .exe/.cmd/.bat so npm shims work).
    Argv(Vec<String>),
    /// The same, except that some argv elements carry payload the durable log
    /// must not hold. Adapters that deliver the prompt through argv (agy's
    /// `-p <prompt>`) mark those indices when they build the command line; the
    /// child still receives the real argv, but every rendering of it - verbose
    /// progress, `step_start.cmd`, `step_end.cmd`, spawn errors - shows a
    /// summary instead of the text (spec P0-04).
    ///
    /// The prompt is flow data: it can carry a pasted file, a previous step's
    /// output, or anything a `--var` put there, and the run directory outlives
    /// the run.
    ArgvWithPayload {
        argv: Vec<String>,
        payload_at: Vec<usize>,
    },
    /// Run through `cmd /C` (Windows) or `sh -c` (Unix).
    Shell(String),
}

impl Invocation {
    /// The argv actually handed to the OS, payload included. Not for logging.
    pub fn argv(&self) -> Option<&[String]> {
        match self {
            Invocation::Argv(a) => Some(a),
            Invocation::ArgvWithPayload { argv, .. } => Some(argv),
            Invocation::Shell(_) => None,
        }
    }

    /// Mark argv elements as payload. A no-op for a shell invocation, whose
    /// text is the command itself and is never a prompt delivery channel.
    pub fn redact_argv(self, payload_at: Vec<usize>) -> Invocation {
        if payload_at.is_empty() {
            return self;
        }
        match self {
            Invocation::Argv(argv) | Invocation::ArgvWithPayload { argv, .. } => {
                Invocation::ArgvWithPayload { argv, payload_at }
            }
            shell => shell,
        }
    }

    /// How this command is written down anywhere a human or a durable artifact
    /// can see it. Binary path, flags, model, cwd and every other diagnostic
    /// argument survive; a marked payload becomes its length and digest, which
    /// is enough to prove two runs used the same prompt without storing it.
    pub fn describe(&self) -> String {
        let quote = |s: &String| {
            if s.contains(' ') || s.is_empty() {
                format!("\"{s}\"")
            } else {
                s.clone()
            }
        };
        match self {
            Invocation::Argv(a) => a.iter().map(quote).collect::<Vec<_>>().join(" "),
            Invocation::ArgvWithPayload { argv, payload_at } => argv
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    if payload_at.contains(&i) {
                        redacted_payload(s)
                    } else {
                        quote(s)
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
            Invocation::Shell(s) => format!("$ {s}"),
        }
    }
}

/// What replaces a payload argument in every log line.
pub fn redacted_payload(value: &str) -> String {
    format!(
        "<prompt chars={} sha256={}>",
        value.chars().count(),
        crate::sha256::hex(value.as_bytes())
    )
}

pub struct ExecOutcome {
    pub exit_code: i32,
    pub timed_out: bool,
    pub interrupted: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub dur_ms: u128,
    /// How long the child was silent before it exited or was killed: the
    /// elapsed time at that moment minus the last chunk seen on EITHER stream.
    /// A child that never wrote anything has idle_ms == its whole runtime.
    ///
    /// This is the second clock B-12 needed. Elapsed time alone cannot tell a
    /// 40-minute step that is working from one that stopped talking 38 minutes
    /// ago, and every external signal (pid alive, heartbeat fresh) said the
    /// wedged run was healthy.
    pub idle_ms: u64,
}

/// Where a child's output goes besides the capture buffer, and which run-level
/// clock its arrival touches. Bundled so `run_cmd` keeps one "observation"
/// parameter instead of growing one per watcher.
/// Receives every stdout byte before the bounded diagnostic capture drops any
/// of it. Structured presets use this to extract their final answer and usage
/// incrementally instead of treating a raw transcript size limit as a semantic
/// limit. Implementations must stay bounded: this callback runs on the pipe
/// reader thread and sees untrusted tool output.
pub trait OutputObserver: Send + Sync {
    fn observe(&self, chunk: &[u8]);
}

#[derive(Default, Clone)]
pub struct Observe {
    /// Mirror stdout to this file as it arrives.
    pub tee: Option<std::path::PathBuf>,
    /// Optional bounded semantic observer for structured stdout.
    pub stdout_observer: Option<Arc<dyn OutputObserver>>,
    /// Run-level activity clock in unix-epoch seconds, shared by every child of
    /// the run so `status.json` can report when ANY of them last said anything.
    /// 0 means nothing has been read yet.
    pub run_clock: Option<Arc<AtomicU64>>,
}

pub fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The two activity clocks a reader thread touches on every chunk. Both streams
/// share one instance: a tool that reports progress only on stderr (several do)
/// would otherwise look silent, and every one of its timeouts would be filed as
/// a hang.
#[derive(Clone)]
struct Activity {
    start: Instant,
    /// ms since `start` of the most recent chunk on either stream; 0 = none yet.
    last_ms: Arc<AtomicU64>,
    run_clock: Option<Arc<AtomicU64>>,
}

impl Activity {
    fn touch(&self) {
        let ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_ms.store(ms, Ordering::Relaxed);
        if let Some(c) = &self.run_clock {
            c.store(epoch_secs(), Ordering::Relaxed);
        }
    }
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

/// Stop accepting more work and terminate every child currently owned by this
/// process. Used for internal durability failures (for example status/log
/// persistence), where continuing to call paid tools would make the run
/// impossible to resume safely.
pub fn request_interrupt() {
    INTERRUPTED.store(true, Ordering::SeqCst);
    for slot in TRACKED.iter() {
        let pid = slot.load(Ordering::SeqCst);
        if pid <= 0 {
            continue;
        }
        #[cfg(unix)]
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        #[cfg(windows)]
        {
            let _ = kill_pid_tree(pid as u32);
        }
    }
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
    use windows_sys::Win32::Foundation::{CloseHandle, BOOL, HANDLE};
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
        JOB.get_or_init(|| configured_job().unwrap_or_default());
    }

    fn configured_job() -> Result<usize, String> {
        unsafe {
            let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(format!(
                    "cannot create a Windows process job: {}",
                    std::io::Error::last_os_error()
                ));
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
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if configured == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                return Err(format!("cannot configure a Windows process job: {error}"));
            }
            Ok(job as usize)
        }
    }

    pub struct ChildJob(usize);

    impl Drop for ChildJob {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0 as HANDLE);
            }
        }
    }

    pub fn adopt(child: &Child) -> Result<ChildJob, String> {
        use std::os::windows::io::AsRawHandle;
        let Some(&job) = JOB.get() else {
            return Err("the Windows process job was not initialized".into());
        };
        if job == 0 {
            return Err("the Windows process job could not be created or configured".into());
        }
        let assigned =
            unsafe { AssignProcessToJobObject(job as HANDLE, child.as_raw_handle() as HANDLE) };
        if assigned == 0 {
            return Err(format!(
                "cannot attach child pid {} to the kill-on-close Windows job: {}",
                child.id(),
                std::io::Error::last_os_error()
            ));
        }
        // A process-wide job guarantees cleanup if sfh itself is killed. A
        // second, nested job scopes cleanup to this one leaf: closing it after
        // the direct child exits kills background grandchildren immediately,
        // without terminating unrelated parallel siblings. Windows 8+ supports
        // this nesting when neither job uses UI restrictions.
        let leaf_job = configured_job()?;
        let assigned = unsafe {
            AssignProcessToJobObject(leaf_job as HANDLE, child.as_raw_handle() as HANDLE)
        };
        if assigned == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(leaf_job as HANDLE);
            }
            return Err(format!(
                "cannot attach child pid {} to its per-leaf Windows job: {error}",
                child.id()
            ));
        }
        Ok(ChildJob(leaf_job))
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
        // no-follow: a resumed --detach hands the run dir to the background copy,
        // and a symlink planted at detached.out.txt / detached.err.txt must not
        // redirect the child's stdio to a file outside the run dir (rev_break #1).
        let f = crate::contain::create_nofollow(p)
            .map_err(|e| format!("cannot create {}: {e}", p.display()))?;
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
        // ...and wake it, or a run that is STOPPED - suspended laptop, ^Z, a
        // debugger - never acts on the SIGTERM at all. That is the case most
        // in need of stopping, and without the SIGCONT the graceful path is
        // skipped entirely: the process gets SIGKILLed below and its agents,
        // which live in their own sessions, are left running.
        libc::kill(pid as i32, libc::SIGCONT);
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
    // Poll rather than asking once. SIGKILL is not synchronous: the process
    // still has to be scheduled to die, and it stays visible as a zombie until
    // whoever adopted it reaps it - a detached run's parent is init, which is
    // prompt but not instant. Checking immediately reported "could not kill"
    // for a process that was already on its way out.
    for _ in 0..30 {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
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

/// Quote one argument for a copy-pasteable command hint. Flow names may carry
/// spaces (R-6), so a hint that prints a run dir or flow path unquoted breaks
/// the moment the user pastes it back: `sfh run 研究 2026.07/... --resume ...`
/// falls apart into separate arguments. Safe characters pass through; anything
/// else is wrapped in double quotes and escaped for the platform's primary
/// shell.
///
/// There is no single quoting that is simultaneously correct for cmd.exe,
/// PowerShell and POSIX sh - backslash is a literal on Windows but the escape
/// character on Unix, and `$()` / backtick are inert inside cmd.exe double
/// quotes but expand under POSIX sh and PowerShell. The hint is therefore
/// escaped for the shell sfh itself uses on each platform (rev_break #13,
/// rev_regression R-6):
/// - Unix (sh -c): escape `\`, `"`, `$` and backtick so a hostile flow value in
///   a forged status.json cannot smuggle `$(...)` / backtick command execution
///   into a pasted resume command.
/// - Windows (cmd /C): escape only `"`. Backslash is left literal (doubling it
///   would corrupt every Windows path), and `$` / backtick have no special
///   meaning to cmd.exe inside double quotes, so they carry no injection vector
///   on the default Windows shell.
pub fn shell_quote(s: &str) -> String {
    if s.is_empty() {
        return "\"\"".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | ','))
    {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        #[cfg(not(windows))]
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        #[cfg(windows)]
        if c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Is this pid still running? Pid reuse makes it advisory, so callers pair it
/// with a heartbeat freshness check before declaring a run dead.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let signalable = if unsafe { libc::kill(pid as i32, 0) } == 0 {
        true
    } else {
        // EPERM: it exists, it just is not ours to signal.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    };
    // A zombie answers kill(pid, 0) exactly as a live process does - the pid is
    // still in the table, holding an exit status nobody has collected - but it
    // is not running, and where PID 1 does not reap orphans it never will be.
    // That host is not exotic: it is every container started without an init,
    // which is precisely where a `--detach` run lives. Counting a zombie as
    // alive made a SIGKILLed run read `running` until its heartbeat went stale,
    // told `sfh stop` to verify ownership of a process whose /proc/<pid>/exe a
    // zombie no longer has (so the stop was refused), and left
    // `--carry-budget-from` refusing permanently (see `carry_source_is_final`).
    signalable && !pid_is_zombie(pid)
}

/// Whether this pid names a process that has already exited and is only waiting
/// for its parent to collect the status (state `Z`).
///
/// Linux: the state character is field 3 of /proc/<pid>/stat - the first token
/// after the parenthesized `comm`, which may itself contain spaces and
/// parentheses, so it is split off at the LAST ')' exactly as `pid_start_time`
/// does with the same string.
///
/// Anything it cannot read answers `false`: "not provably a zombie". Liveness
/// is only ever narrowed here by positive evidence, never widened by the
/// absence of it, which keeps every caller's fail-closed reading intact.
#[cfg(target_os = "linux")]
fn pid_is_zombie(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().next())
        == Some("Z")
}

/// Every other Unix, macOS included: "cannot tell", which reads as
/// not-a-zombie and so leaves liveness exactly as it was before this check
/// existed.
///
/// Not a stub for want of trying. macOS cannot answer through `proc_pidinfo`
/// at all - both BSD-info flavors reach the process through `proc_find`, which
/// skips a `SZOMB` entry by design, so every flavor reports "no such process"
/// for precisely the case being asked about. The call that does see zombies is
/// the `KERN_PROC_PID` sysctl, and libc exposes neither `kinfo_proc` nor
/// `extern_proc` on Apple targets: reading `p_stat` would mean hand-computing
/// its offset inside a large layout-sensitive struct, and getting that wrong
/// fails in the dangerous direction - a live process misread as dead lets
/// `sfh stop` skip a real one and lets a carry run against a run still
/// spending.
///
/// The trade is worth taking because the condition is not durable here. This
/// bug needs a PID 1 that does not reap orphans; macOS's is launchd, which
/// always does, so a detached run's zombie is collected in the moment rather
/// than outliving the run. On Linux - where a container's PID 1 is routinely
/// not an init at all - it is durable, and that is where the check is
/// implemented.
#[cfg(all(unix, not(target_os = "linux")))]
fn pid_is_zombie(_pid: u32) -> bool {
    false
}

/// The start time of a process, as an opaque u64 that is unique per pid on one
/// boot (Windows: FILETIME of creation; Linux: clock ticks since boot; macOS:
/// microseconds since the epoch). Used to bind the stop nonce to the process
/// that owns a run, so a pid REUSED by an unrelated process after the run died
/// is told apart from the original: `sfh stop` compares this value against the
/// one recorded when the run started, and refuses when they differ (rev_break
/// #8). `None` when the OS cannot answer (process gone, or an unsupported
/// platform); callers then fall back to the weaker (pid, nonce) binding.
#[cfg(windows)]
pub fn pid_start_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return None;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut creation: FILETIME = std::mem::zeroed();
        let mut exit: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let ok = GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        Some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64)
    }
}

/// Start time of a process (Linux: field 22 of /proc/<pid>/stat, clock ticks
/// since boot).
#[cfg(target_os = "linux")]
pub fn pid_start_time(pid: u32) -> Option<u64> {
    if pid == 0 {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 2 (comm) is parenthesized and may itself contain spaces and
    // parentheses, so split after the LAST ')'. The fields that follow are
    // state(3) ppid(4) ... starttime(22): the 20th whitespace-separated token.
    let rest = stat.rsplit_once(')').map(|(_, r)| r)?;
    let ticks = rest.split_whitespace().nth(19)?;
    ticks.parse::<u64>().ok()
}

/// Start time of a process (macOS: proc_pidinfo(PROC_PIDTBSDINFO) ->
/// pbi_start_tvsec/usec; there is no /proc).
#[cfg(target_os = "macos")]
pub fn pid_start_time(pid: u32) -> Option<u64> {
    // struct proc_bsdinfo from <sys/proc_info.h>; only the fields up to the
    // start time are needed, but every preceding field must keep its exact
    // type and width or the offset of pbi_start_tvsec is wrong.
    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [u8; 16], // MAXCOMLEN
        pbi_name: [u8; 32], // 2 * MAXCOMLEN
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }
    extern "C" {
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
    }
    const PROC_PIDTBSDINFO: libc::c_int = 3;
    if pid == 0 {
        return None;
    }
    let mut info: ProcBsdInfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<ProcBsdInfo>() as libc::c_int;
    let n = unsafe {
        proc_pidinfo(
            pid as libc::c_int,
            PROC_PIDTBSDINFO,
            0,
            &mut info as *mut ProcBsdInfo as *mut libc::c_void,
            size,
        )
    };
    if n < size {
        return None;
    }
    Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

/// Other Unix targets: no portable way to read a process start time without
/// procfs, so refuse; the nonce then binds (pid, token) only, on the same
/// access-control bound as before start-time recording existed (rev_break #8).
#[cfg(all(unix, not(target_os = "linux"), not(target_os = "macos")))]
pub fn pid_start_time(_pid: u32) -> Option<u64> {
    None
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
    obs: Observe,
) -> Result<ExecOutcome, String> {
    if interrupted() {
        return Err("interrupted before start".into());
    }
    let mut cmd = match inv.argv() {
        Some(argv) => {
            if argv.is_empty() {
                return Err("empty command".into());
            }
            let mut c = Command::new(resolve_program(&argv[0]));
            c.args(&argv[1..]);
            c
        }
        None => match inv {
            Invocation::Shell(line) => shell_command(line),
            _ => unreachable!("only Shell has no argv"),
        },
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
    let leaf_job = match windows_guard::adopt(&child) {
        Ok(job) => job,
        Err(e) => {
            // The child has not received its prompt yet. Fail before paid work
            // and tear down any descendants it managed to open during process
            // startup; running unowned would violate the no-orphan contract.
            kill_tree(&mut child);
            let _ = child.wait();
            return Err(e);
        }
    };
    let pid = child.id() as i32;
    track(pid);

    let stdin_thread = stdin_data.map(|data| {
        let mut si = child.stdin.take().expect("stdin piped");
        std::thread::spawn(move || {
            let _ = si.write_all(&data);
            // handle dropped here -> child sees EOF
        })
    });
    let activity = Activity {
        start,
        last_ms: Arc::new(AtomicU64::new(0)),
        run_clock: obs.run_clock.clone(),
    };
    let tee_enabled = Arc::new(AtomicBool::new(true));
    let stdout_semantically_observed = obs.stdout_observer.is_some();
    let rx_out = spawn_reader(
        child.stdout.take().expect("stdout piped"),
        obs.tee,
        obs.stdout_observer,
        Arc::clone(&tee_enabled),
        activity.clone(),
    );
    let rx_err = spawn_reader(
        child.stderr.take().expect("stderr piped"),
        None,
        None,
        Arc::clone(&tee_enabled),
        activity.clone(),
    );

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
    // The signal handler kills tracked process groups directly. The child can
    // therefore become waitable between `try_wait` and the loop's interrupt
    // check; preserve the run-level cause instead of reporting an ordinary
    // exit merely because reaping won that race.
    was_interrupted |= interrupted();
    untrack(pid);
    #[cfg(windows)]
    drop(leaf_job);
    #[cfg(unix)]
    unsafe {
        // A command may exit after backgrounding a descendant that still owns
        // stdout/stderr. The descendant is part of this leaf, not a detached
        // workflow, so close the whole session even on a nominal root exit.
        // Otherwise the pipe drain can stall for minutes and the process can
        // outlive both its step and a normally exiting sfh.
        libc::kill(-pid, libc::SIGKILL);
    }
    // The live tee is only an observability aid while the child is running.
    // Stop reopening it before the durable canonical snapshot is published.
    // A grandchild can keep the pipe reader blocked after a timeout, but it
    // must not keep a Windows file handle open or append after publication.
    tee_enabled.store(false, Ordering::SeqCst);
    // Keep the time of death fixed. Reader threads may still be holding a
    // final buffered chunk at this instant; their activity snapshot is taken
    // after the drain below, then clamped back to this point.
    let death_elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

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
    if out_trunc {
        let semantic = if stdout_semantically_observed {
            "; the structured semantic observer processed the complete stream"
        } else {
            ""
        };
        stderr.extend_from_slice(
            format!(
                "\n[sfh: raw stdout middle omitted at {} MB capture limit{semantic}]\n",
                MAX_CAPTURE / 1024 / 1024
            )
            .as_bytes(),
        );
    }
    if err_trunc {
        stderr.extend_from_slice(
            format!(
                "\n[sfh: raw stderr middle omitted at {} MB capture limit]\n",
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
    // A final stdout/stderr chunk can be read just after try_wait observed the
    // process exit. Sampling before the pipe drain made that healthy final
    // output invisible and exaggerated idle_ms into a false hang. Activity
    // observed during the drain cannot be later than the child's death for
    // this calculation, so clamp it to that fixed instant.
    let idle_ms = idle_at(death_elapsed_ms, activity.last_ms.load(Ordering::Relaxed));
    Ok(ExecOutcome {
        exit_code: status.code().unwrap_or(-1),
        timed_out,
        // Catch a signal that arrived while the already-dead child's pipes
        // were being drained as well. The engine treats cancellation as
        // stronger than a completed leaf.
        interrupted: was_interrupted || interrupted(),
        stdout,
        stderr,
        dur_ms: start.elapsed().as_millis(),
        idle_ms,
    })
}

fn idle_at(death_elapsed_ms: u64, last_activity_ms: u64) -> u64 {
    death_elapsed_ms.saturating_sub(last_activity_ms.min(death_elapsed_ms))
}

/// Per-stream diagnostic capture cap. The reader always drains and semantic
/// observers still receive the complete stream. Oversized raw output retains
/// both its beginning and end: headers/session identity tend to be at the
/// beginning, while final answers and terminal errors are at the end.
const MAX_CAPTURE: usize = 32 * 1024 * 1024;
const CAPTURE_HEAD: usize = MAX_CAPTURE / 2;
const CAPTURE_TAIL: usize = MAX_CAPTURE - CAPTURE_HEAD;
const CAPTURE_GAP: &[u8] = b"\n[sfh: raw output middle omitted after 32 MB capture limit]\n";

struct BoundedCapture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl BoundedCapture {
    fn new() -> Self {
        Self {
            head: Vec::with_capacity(CAPTURE_HEAD),
            tail: VecDeque::with_capacity(CAPTURE_TAIL),
            total: 0,
        }
    }

    fn push(&mut self, mut chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len());
        if self.head.len() < CAPTURE_HEAD {
            let take = chunk.len().min(CAPTURE_HEAD - self.head.len());
            self.head.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
        }
        if chunk.is_empty() {
            return;
        }
        if chunk.len() >= CAPTURE_TAIL {
            self.tail.clear();
            self.tail.extend(
                chunk[chunk.len().saturating_sub(CAPTURE_TAIL)..]
                    .iter()
                    .copied(),
            );
            return;
        }
        let overflow = self
            .tail
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(CAPTURE_TAIL);
        if overflow > 0 {
            self.tail.drain(..overflow);
        }
        self.tail.extend(chunk.iter().copied());
    }

    fn finish(self) -> (Vec<u8>, bool) {
        let truncated = self.total > MAX_CAPTURE;
        if !truncated {
            let mut all = self.head;
            all.extend(self.tail);
            return (all, false);
        }
        let mut all = Vec::with_capacity(MAX_CAPTURE + CAPTURE_GAP.len());
        all.extend(self.head);
        all.extend_from_slice(CAPTURE_GAP);
        all.extend(self.tail);
        (all, true)
    }
}

/// `tee`: mirror each chunk to this file as it arrives. Without it nothing is
/// observable until the child exits, so a 30-minute step is indistinguishable
/// from a hung one. The file is rewritten with the cleaned text at the end, so
/// what is visible mid-run is the raw stream and what remains is canonical.
fn spawn_reader<R: Read + Send + 'static>(
    mut r: R,
    tee: Option<std::path::PathBuf>,
    observer: Option<Arc<dyn OutputObserver>>,
    tee_enabled: Arc<AtomicBool>,
    activity: Activity,
) -> std::sync::mpsc::Receiver<(Vec<u8>, bool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let tee = tee.and_then(|p| {
            // no-follow on the final component: the tee target is a predictable
            // <tag>.out.txt inside a run dir that is untrusted on a resumed run,
            // and File::create FOLLOWS a symlink - a planted link let the child's
            // stdout truncate and fill a file outside the run dir before the
            // cleaned-text rewrite (itself no-follow) even ran. A link at the
            // tee target now fails the open, so the step's capture falls back to
            // memory instead of writing outside the run dir (rev_break #3).
            let f = crate::contain::create_nofollow(&p).ok()?;
            crate::contain::restrict_file(&f);
            drop(f);
            Some(p)
        });
        let mut capture = BoundedCapture::new();
        let mut tee_written = 0usize;
        let mut tmp = [0u8; 65536];
        loop {
            match r.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    // Before the raw capture cap is consulted: a noisy tool is
                    // still active, and a structured observer must see every
                    // byte so its final answer/accounting cannot be truncated.
                    activity.touch();
                    if let Some(observer) = &observer {
                        observer.observe(&tmp[..n]);
                    }
                    if tee_written < MAX_CAPTURE && tee_enabled.load(Ordering::SeqCst) {
                        let take = n.min(MAX_CAPTURE - tee_written);
                        if let Some(path) = &tee {
                            // The live tee is prefix-only and bounded. It is an
                            // in-progress view; the canonical head+tail snapshot
                            // atomically replaces it after the child exits.
                            if let Ok(mut f) = crate::contain::append_private(path) {
                                let _ = f.write_all(&tmp[..take]);
                                let _ = f.flush();
                            }
                        }
                        tee_written += take;
                    }
                    capture.push(&tmp[..n]);
                }
            }
        }
        let _ = tx.send(capture.finish());
    });
    rx
}

/// Where a program name resolves on this machine, or `None` when it does not.
///
/// Deliberately does NOT spawn anything: preflight has to be able to say "that
/// binary is not installed" without running a stranger's executable to find
/// out. A name that already carries a path separator or an extension is taken
/// as a path and only checked for existence.
pub fn which(name: &str) -> Option<String> {
    let direct = Path::new(name);
    if name.contains('/') || name.contains('\\') {
        return direct.is_file().then(|| name.to_string());
    }
    // The extension list has to be the one `resolve_program` uses at spawn
    // time, or preflight reports a path the run would not actually launch.
    //
    // On Windows that is a two-case rule, and collapsing it either way is
    // wrong. A BARE name is completed with the npm shim extensions and never
    // launched extension-less, so `bash` must not report a file called `bash`.
    // A name that ALREADY carries an extension is handed to the OS verbatim,
    // so `pwsh.exe` and `claude.cmd` are looked up as written - appending to
    // them would find nothing and preflight would block a program that runs.
    let exts: &[&str] = if cfg!(windows) {
        if Path::new(name).extension().is_some() {
            &[""]
        } else {
            &[".exe", ".cmd", ".bat"]
        }
    } else {
        &[""]
    };
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for ext in exts {
            let cand = dir.join(format!("{name}{ext}"));
            if is_executable_file(&cand) {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// A PATH candidate the OS would actually exec. On Unix `execvp` skips a file
/// without the exec bit and keeps searching, so reporting the first readable
/// match would name a file the run never runs.
fn is_executable_file(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// True when a resolved path is the Windows WSL launcher rather than a shell
/// that can run in this OS.
///
/// `%SystemRoot%\System32` sits near the front of PATH on every Windows box and
/// ships `bash.exe`/`wsl.exe`, which start a Linux distribution. Git for Windows
/// does not put its own `bash.exe` on PATH. So a flow that says `bash` on
/// Windows gets WSL, where the repository's Windows paths are meaningless and a
/// worktree's `.git` gitfile points somewhere that does not exist - the commands
/// fail in seconds, for a reason that has nothing to do with the code under
/// test, and the failure text flows on to whatever reads that step's output.
///
/// Takes the resolved path as text so the rule is testable on every platform.
pub fn is_wsl_launcher(resolved_path: &str) -> bool {
    let lower = resolved_path.to_ascii_lowercase().replace('/', "\\");
    // sysnative and syswow64 are the same directory seen through the 32/64-bit
    // redirector, so all three spellings have to count.
    ["\\system32\\", "\\sysnative\\", "\\syswow64\\"]
        .iter()
        .any(|dir| lower.contains(dir))
        && (lower.ends_with("\\bash.exe") || lower.ends_with("\\wsl.exe"))
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
        Observe::default(),
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
///
/// Every needle below names a way a PROVIDER can fail: a rate limit, a 5xx, a
/// dropped socket, a serving-side abort. That is what `retry_on: transient`
/// exists for, and matching it against an AI CLI's own report is sound.
///
/// It is not sound against arbitrary program output, which is why the caller
/// decides what counts as the tool's report (see `leaf`): a `cmd:` step's
/// STDOUT is its RESULT - the test list, the diff, the JSON - and a test named
/// `tcp_502_returns_error` failing deterministically is not a rate limit. sfh
/// used to re-run the whole verification suite for exactly that reason.
pub fn is_transient_failure(stderr: &str, stdout: &str) -> bool {
    const NEEDLES: [&str; 22] = [
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
        // Serving-side aborts. Observed live: opencode reported
        // "An error occurred in model serving ... [Inference engine abort.
        // Finish reason: [STOP_ENGINE_ERROR].]" seventeen minutes into a step,
        // mid-edit. Nothing about the request was wrong, and the next attempt
        // succeeded - exactly what retry_on: transient exists for.
        "stop_engine_error",
        "inference engine abort",
        "error in model serving",
        "error occurred in model serving",
    ];
    let hay = format!("{stderr}\n{stdout}").to_lowercase();
    NEEDLES.iter().any(|n| hay.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that has exited but has not been reaped still answers
    /// `kill(pid, 0)`, so `pid_alive` used to call it alive. Where PID 1 does
    /// not reap orphans - every container started without an init, which is
    /// where a `--detach` run lives - that answer never changed, and it is the
    /// answer `sfh status`, `sfh stop` and `--carry-budget-from` are all built
    /// on.
    ///
    /// Reaped here at the end rather than left behind: a test that leaks a
    /// zombie into a suite which itself runs under a non-reaping init is the
    /// bug it is testing for.
    ///
    /// Linux only, because that is where `pid_is_zombie` can answer. macOS
    /// reaches every process through `proc_find`, which skips a `SZOMB` entry
    /// by design, so no `proc_pidinfo` flavor can see the state this asserts
    /// on - see `pid_is_zombie` for why reading it the one way that works is
    /// not worth the ABI assumption, and why the condition is not durable
    /// under launchd anyway.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_zombie_is_not_alive_even_though_it_still_answers_signal_zero() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .spawn()
            .expect("spawn a child that exits at once");
        let pid = child.id();
        // Wait for the exit WITHOUT reaping, so the pid is a zombie held open
        // by this process still owing it a wait().
        for _ in 0..200 {
            if pid_is_zombie(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            pid_is_zombie(pid),
            "the child should be an unreaped zombie by now"
        );
        assert_eq!(
            unsafe { libc::kill(pid as i32, 0) },
            0,
            "a zombie must still answer signal 0 - that is what made this a bug"
        );
        assert!(
            !pid_alive(pid),
            "a zombie is not running, so pid_alive must not say it is"
        );
        let _ = child.wait();
    }

    #[test]
    fn the_windows_wsl_launcher_is_recognised_wherever_it_is_spelled() {
        // The exact path a bare `bash` resolves to on a stock Windows box.
        for p in [
            r"C:\Windows\System32\bash.exe",
            r"c:\windows\system32\BASH.EXE",
            r"C:/Windows/System32/bash.exe",
            r"C:\Windows\Sysnative\wsl.exe",
            r"C:\Windows\SysWOW64\bash.exe",
        ] {
            assert!(is_wsl_launcher(p), "{p} should be recognised as WSL");
        }
        // Real shells that can run in this OS, and unrelated System32 binaries,
        // must not be caught: a false positive here blocks a working flow.
        for p in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\msys64\usr\bin\bash.exe",
            r"C:\Windows\System32\cmd.exe",
            r"C:\Windows\System32\where.exe",
            "/bin/bash",
            "/usr/bin/bash",
            "/opt/system32-tools/bin/bash",
        ] {
            assert!(!is_wsl_launcher(p), "{p} must not be treated as WSL");
        }
    }

    // PATH is process-global and these tests run in parallel, so the candidate
    // predicate is exercised directly rather than by pointing `which` at a
    // temporary directory - a test that reaches for `set_var("PATH", ...)`
    // corrupts whatever else is resolving a program at that moment.
    #[test]
    #[cfg(windows)]
    fn a_program_name_that_already_carries_an_extension_is_looked_up_as_written() {
        // `Command::new` hands such a name to the OS verbatim, so appending
        // .exe/.cmd/.bat to it would find nothing and preflight would block a
        // program that runs perfectly well. cmd.exe is always in System32,
        // which is always on PATH.
        assert!(
            which("cmd.exe").is_some(),
            "a name with an extension must resolve as written"
        );
        assert!(which("cmd").is_some(), "and a bare name still completes");
    }

    #[test]
    fn a_path_candidate_the_os_would_not_exec_is_not_a_program() {
        let dir = std::env::temp_dir().join(format!("sfh-which-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("sfh-which-probe");
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Readable but not executable: execvp skips it and keeps searching,
            // so reporting it would name a file the run never runs.
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!is_executable_file(&p));
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(is_executable_file(&p));
        // A directory with a program's name is not a program.
        let d = dir.join("sfh-which-dir");
        std::fs::create_dir_all(&d).unwrap();
        assert!(!is_executable_file(&d));
        assert!(!is_executable_file(&dir.join("nothing-here")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_blocked_pipe_reader_does_not_hold_the_canonical_output_open() {
        use std::io::Read;
        use std::sync::mpsc;

        struct OneChunkThenBlock {
            sent: bool,
            release: mpsc::Receiver<()>,
        }

        impl Read for OneChunkThenBlock {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if !self.sent {
                    self.sent = true;
                    buf[..7].copy_from_slice(b"partial");
                    return Ok(7);
                }
                let _ = self.release.recv();
                Ok(0)
            }
        }

        let dir = std::env::temp_dir().join(format!(
            "sfh-blocked-tee-{}-{}",
            std::process::id(),
            epoch_secs()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("step.out.txt");
        let (release_tx, release_rx) = mpsc::channel();
        let reader = OneChunkThenBlock {
            sent: false,
            release: release_rx,
        };
        let tee_enabled = Arc::new(AtomicBool::new(true));
        let captured = spawn_reader(
            reader,
            Some(output.clone()),
            None,
            Arc::clone(&tee_enabled),
            Activity {
                start: Instant::now(),
                last_ms: Arc::new(AtomicU64::new(0)),
                run_clock: None,
            },
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while std::fs::read(&output).ok().as_deref() != Some(b"partial") {
            assert!(
                Instant::now() < deadline,
                "the live tee never published its first chunk"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        tee_enabled.store(false, Ordering::SeqCst);
        crate::contain::write_private_atomic(&output, b"canonical").unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(
            captured.recv_timeout(Duration::from_secs(2)).unwrap().0,
            b"partial"
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"canonical");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_capture_keeps_head_and_tail_while_observer_sees_every_byte() {
        #[derive(Default)]
        struct CountObserver {
            seen: std::sync::Mutex<(usize, VecDeque<u8>)>,
        }
        impl OutputObserver for CountObserver {
            fn observe(&self, chunk: &[u8]) {
                let mut seen = self.seen.lock().unwrap();
                seen.0 = seen.0.saturating_add(chunk.len());
                for byte in chunk {
                    if seen.1.len() == 16 {
                        seen.1.pop_front();
                    }
                    seen.1.push_back(*byte);
                }
            }
        }

        let mut source = vec![b'x'; MAX_CAPTURE + 1024];
        source[..5].copy_from_slice(b"BEGIN");
        let end = source.len();
        source[end - 5..].copy_from_slice(b"FINAL");
        let expected_len = source.len();
        let observer = Arc::new(CountObserver::default());
        let tee_enabled = Arc::new(AtomicBool::new(false));
        let captured = spawn_reader(
            std::io::Cursor::new(source),
            None,
            Some(Arc::clone(&observer) as Arc<dyn OutputObserver>),
            tee_enabled,
            Activity {
                start: Instant::now(),
                last_ms: Arc::new(AtomicU64::new(0)),
                run_clock: None,
            },
        )
        .recv_timeout(Duration::from_secs(5))
        .unwrap();

        assert!(captured.1);
        assert!(captured.0.starts_with(b"BEGIN"));
        assert!(captured.0.ends_with(b"FINAL"));
        assert!(captured
            .0
            .windows(CAPTURE_GAP.len())
            .any(|window| window == CAPTURE_GAP));
        let seen = observer.seen.lock().unwrap();
        assert_eq!(seen.0, expected_len);
        assert!(seen
            .1
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .ends_with(b"FINAL"));
    }

    #[test]
    fn idle_clock_includes_final_pipe_activity_without_counting_drain_time() {
        assert_eq!(idle_at(1_000, 250), 750);
        assert_eq!(idle_at(1_000, 1_000), 0);
        // A reader can timestamp a buffered final chunk just after the process
        // death snapshot. Clamp it to death instead of underflowing or treating
        // the drain itself as runtime.
        assert_eq!(idle_at(1_000, 1_025), 0);
    }

    #[test]
    fn exe_match_is_exact_stem_not_substring() {
        // rev_complete S1-2: the old substring match would let `sfh stop` kill an
        // unrelated `sfh-helper` because its name contains "sfh". The match must
        // be an EXACT file-stem comparison. Test against our own binary so the
        // positive case is portable, then prove a "<stem>-helper" is rejected.
        let own = std::env::current_exe().expect("current_exe");
        let own_str = own.to_str().expect("utf8 exe path");
        assert!(exe_path_is_ours(own_str), "our own executable must match");

        let stem = own
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        assert!(!stem.is_empty());
        let helper = format!("/some/path/{stem}-helper");
        assert!(
            !exe_path_is_ours(&helper),
            "'{stem}-helper' contains the stem but is a different binary and must NOT match"
        );
        let helper_exe = format!(r"C:\tools\{stem}-helper.exe");
        assert!(!exe_path_is_ours(&helper_exe));

        // Unrelated programs and empty paths never match.
        assert!(!exe_path_is_ours("/usr/bin/python3"));
        assert!(!exe_path_is_ours(""));
    }

    #[test]
    fn detects_transient_failures() {
        assert!(is_transient_failure("HTTP 429 Too Many Requests", ""));
        assert!(is_transient_failure("", "Error: overloaded_error"));
        assert!(is_transient_failure("ERROR: Reconnecting... 3/5", ""));
        // Verbatim from a live opencode run that died 17 minutes into a step.
        assert!(is_transient_failure(
            "",
            r#"{"type":"error","error":{"name":"UnknownError","data":{"message":"\"An error occurred in model serving, error message is: [Inference engine abort. Finish reason: [STOP_ENGINE_ERROR].]\""}}}"#
        ));
        assert!(!is_transient_failure("SyntaxError: unexpected token", ""));
        assert!(!is_transient_failure("", "the model refused"));
    }

    #[test]
    fn describes_invocations() {
        let a = Invocation::Argv(vec!["tool".into(), "--flag".into(), "two words".into()]);
        assert_eq!(a.describe(), "tool --flag \"two words\"");
        assert_eq!(Invocation::Shell("echo hi".into()).describe(), "$ echo hi");
    }

    /// P0-04. The prompt an argv-delivery adapter carries is flow data - a
    /// pasted file, an upstream step's output, whatever a --var held - and
    /// every description of the command line ends up in a durable artifact.
    /// The diagnostics that make a command line worth logging (binary, flags,
    /// model, access) have to survive; the payload must not.
    #[test]
    fn an_argv_prompt_never_reaches_a_command_description() {
        let secret = "line one\nSSH_KEY=hunter2\nplease review the diff";
        let inv = Invocation::Argv(vec![
            "agy".into(),
            "--model".into(),
            "big-model".into(),
            "--mode".into(),
            "plan".into(),
            "-p".into(),
            secret.into(),
        ])
        .redact_argv(vec![6]);
        let described = inv.describe();
        assert!(
            !described.contains("hunter2") && !described.contains("please review"),
            "the prompt leaked into {described}"
        );
        for keep in ["agy", "--model", "big-model", "--mode", "plan", "-p"] {
            assert!(described.contains(keep), "{keep} must stay in {described}");
        }
        assert!(
            described.contains(&format!("chars={}", secret.chars().count())),
            "the summary reports the prompt size: {described}"
        );
        assert!(
            described.contains(&crate::sha256::hex(secret.as_bytes())),
            "the summary pins the exact prompt by digest: {described}"
        );
        // The child still gets the real thing.
        assert_eq!(
            inv.argv().and_then(|a| a.last()).map(String::as_str),
            Some(secret)
        );
        // Nothing marked, nothing changed.
        assert_eq!(
            Invocation::Argv(vec!["tool".into(), "x".into()])
                .redact_argv(vec![])
                .describe(),
            "tool x"
        );
    }

    #[test]
    fn shell_quote_leaves_safe_paths_alone() {
        assert_eq!(shell_quote("flow.yaml"), "flow.yaml");
        assert_eq!(shell_quote(".sfh/runs/x-1"), ".sfh/runs/x-1");
        assert_eq!(shell_quote("C:/AI/sfh/.sfh/runs"), "C:/AI/sfh/.sfh/runs");
        assert_eq!(shell_quote("--var=k=v"), "--var=k=v");
    }

    #[test]
    fn shell_quote_wraps_spaces_and_unicode() {
        assert_eq!(shell_quote("space runs/x 1"), "\"space runs/x 1\"");
        assert_eq!(
            shell_quote(".sfh/runs/20260101-研究 2026.07"),
            "\".sfh/runs/20260101-研究 2026.07\""
        );
        assert_eq!(shell_quote(""), "\"\"");
    }

    #[cfg(not(windows))]
    #[test]
    fn shell_quote_escapes_for_posix() {
        // POSIX sh: \" is a literal quote, \\ a literal backslash, and $ / `
        // are escaped so a forged flow value cannot inject $(...) or backtick
        // command execution into a pasted resume command (rev_break #13).
        assert_eq!(
            shell_quote(r"C:\AI\Simple Flow"),
            r#""C:\\AI\\Simple Flow""#
        );
        assert_eq!(shell_quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(shell_quote("$(reboot)"), r#""\$(reboot)""#);
        assert_eq!(shell_quote("`id`"), r#""\`id\`""#);
    }

    #[cfg(windows)]
    #[test]
    fn shell_quote_escapes_for_cmd() {
        // cmd.exe: backslash is literal (doubling it would corrupt Windows
        // paths - rev_regression R-6) and $ / backtick are inert inside double
        // quotes, so only the quote itself needs escaping.
        assert_eq!(shell_quote(r"C:\AI\Simple Flow"), r#""C:\AI\Simple Flow""#);
        assert_eq!(shell_quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(shell_quote("$(reboot)"), r#""$(reboot)""#);
    }
}
