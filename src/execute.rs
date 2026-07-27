use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub dur_ms: u128,
}

pub fn run_cmd(
    inv: &Invocation,
    stdin_data: Option<Vec<u8>>,
    cwd: Option<&Path>,
    timeout: Option<Duration>,
    env_remove: &[String],
    env_set: &[(String, String)],
) -> Result<ExecOutcome, String> {
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

    // New process group on Unix so a timeout can kill the whole tree.
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn [{}]: {e}", inv.describe()))?;

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
    let status = loop {
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            break st;
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
            format!("\n[sfh: captured output truncated at {} MB]\n", MAX_CAPTURE / 1024 / 1024).as_bytes(),
        );
    }
    if drain_expired {
        stderr.extend_from_slice(
            b"\n[sfh: output drain timed out; a background grandchild process may still hold the pipe and keep running]\n",
        );
    }
    if let Some(t) = stdin_thread {
        let _ = t.join();
    }
    Ok(ExecOutcome {
        exit_code: status.code().unwrap_or(-1),
        timed_out,
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
