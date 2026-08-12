use std::path::{Path, PathBuf};

/// An exclusive, process-lifetime claim on one run directory.
///
/// The file remains as an ordinary private artifact after the process exits,
/// but the OS lock is released automatically. That makes a crashed owner
/// reclaimable without deleting a lock path and avoids a check-then-act race
/// between two simultaneous resumes.
pub struct RunLease {
    _file: std::fs::File,
}

#[derive(Debug)]
pub enum RunLeaseError {
    Busy,
    Io(std::io::Error),
}

/// Try to claim `dir` without following a planted final-component symlink.
/// The returned handle must stay alive for the whole run attempt.
pub fn try_run_lease(dir: &Path) -> Result<RunLease, RunLeaseError> {
    let path = dir.join("sfh-run.lock");
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(RunLeaseError::Io)?;
        let mut permissions = file.metadata().map_err(RunLeaseError::Io)?.permissions();
        if permissions.mode() & 0o777 != 0o600 {
            permissions.set_mode(0o600);
            file.set_permissions(permissions)
                .map_err(RunLeaseError::Io)?;
        }
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::WouldBlock {
                Err(RunLeaseError::Busy)
            } else {
                Err(RunLeaseError::Io(error))
            };
        }
        Ok(RunLease { _file: file })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
            .map_err(|error| {
                if matches!(error.raw_os_error(), Some(5 | 32 | 33)) {
                    RunLeaseError::Busy
                } else {
                    RunLeaseError::Io(error)
                }
            })?;
        Ok(RunLease { _file: file })
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    RunLeaseError::Busy
                } else {
                    RunLeaseError::Io(error)
                }
            })?;
        Ok(RunLease { _file: file })
    }
}

/// Reject absolute paths and `..` traversal components without resolving.
/// Use this for paths that will be joined and stored, not read immediately.
pub fn validate_relative(candidate: &str) -> Result<(), String> {
    let p = Path::new(candidate);
    if p.is_absolute() {
        return Err(format!(
            "refusing path '{candidate}': absolute paths are not allowed in run artifacts"
        ));
    }
    if p.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::RootDir
        )
    }) {
        return Err(format!(
            "refusing path '{candidate}': path traversal is not allowed in run artifacts"
        ));
    }
    Ok(())
}

/// Resolve `candidate` against `base` the way `contained` does, but treat a
/// MISSING file as `Ok(None)` instead of an error. A path that is absolute,
/// carries a `..` component, or resolves (symlinks included) outside `base`
/// is still a hard error: a run dir is untrusted input on --resume, and an
/// artifact that vanished after the log was written is ordinary, but an
/// artifact that points somewhere else is an attack.
///
/// A missing path is NOT an unconditional `Ok(None)`: the deepest EXISTING
/// ancestor is still canonicalized and required to stay under `base`. Without
/// that, `run/escape -> /outside` with `escape/missing` absent would return
/// `Ok(None)` without ever noticing the outward symlink, because the NotFound
/// short-circuit fired before any resolution (rev_break #4). Only a path whose
/// existing prefix is genuinely inside the run dir reads as "absent".
pub fn contained_opt(base: &Path, candidate: &str) -> Result<Option<PathBuf>, String> {
    validate_relative(candidate)?;
    let joined = base.join(candidate);
    let canon_base = base
        .canonicalize()
        .map_err(|e| format!("cannot resolve run dir {}: {e}", base.display()))?;
    match joined.symlink_metadata() {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // The target is gone, but an attacker-controlled run dir may still
            // route the path through an outward symlink on an intermediate
            // component. Resolve the deepest existing ancestor and require it
            // to be contained before treating the file as merely absent.
            let mut ancestor = joined.clone();
            loop {
                match ancestor.symlink_metadata() {
                    Ok(_) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => match ancestor.parent() {
                        Some(p) if p != ancestor => ancestor = p.to_path_buf(),
                        _ => return Ok(None),
                    },
                    Err(e) => {
                        return Err(format!(
                            "cannot stat '{}' under {}: {e}",
                            candidate,
                            base.display()
                        ))
                    }
                }
            }
            let canon_anc = ancestor.canonicalize().map_err(|e| {
                format!(
                    "cannot resolve the parent of '{}' under {}: {e}",
                    candidate,
                    base.display()
                )
            })?;
            if !canon_anc.starts_with(&canon_base) {
                return Err(format!(
                    "refusing path '{candidate}': its parent resolves outside the run dir {}",
                    canon_base.display()
                ));
            }
            return Ok(None);
        }
        Err(e) => {
            return Err(format!(
                "cannot stat '{}' under {}: {e}",
                candidate,
                base.display()
            ))
        }
    }
    let canon = joined.canonicalize().map_err(|e| {
        format!(
            "cannot resolve '{}' under {}: {e}",
            candidate,
            base.display()
        )
    })?;
    if !canon.starts_with(&canon_base) {
        return Err(format!(
            "refusing path '{candidate}': resolves outside the run dir {}",
            canon_base.display()
        ));
    }
    let out = deverbatim(canon);
    // Windows `canonicalize` decides containment on the verbatim (`\\?\...`)
    // spelling, but `deverbatim` hands back the ordinary spelling for downstream
    // consumers. A component created through the verbatim API can carry trailing
    // spaces or dots (`".. "`) that the Win32 name space keeps as a literal
    // directory name while cmd.exe / msys tools re-normalize it to `..` when they
    // parse the de-verbatim'd path, pointing at the run dir's parent. Re-check the
    // de-verbatim'd form for any component that trims to a traversal so the spelling
    // sfh returns cannot mean something different to the tool that consumes it
    // (rev_break #3).
    if has_hidden_traversal(&out) {
        return Err(format!(
            "refusing path '{candidate}': a path component normalizes to a traversal ('..' or '.') outside the verbatim prefix"
        ));
    }
    Ok(Some(out))
}

/// True when any component of `p` would mean something DIFFERENT once a
/// non-verbatim consumer (cmd.exe, msys, the Win32 file dialog parsing) parses
/// the de-verbatim'd path. The Win32 non-verbatim parser strips trailing dots
/// AND spaces from each component, so `".. "` is a literal directory under the
/// verbatim namespace but re-parses to `..` - a traversal to the run dir's
/// parent (the old trim-then-strip check missed it: `".. "` trims to `".."`,
/// strips to `""`, and `""` is neither `"."` nor `".."`), and `"foo. "`
/// re-parses to `foo`, so the path sfh returns would name a DIFFERENT file than
/// the one canonicalize checked (rev_break #1).
///
/// The rule is therefore strict on Windows: a component whose stripped form
/// differs from its raw form (or strips to nothing / `.` / `..`) is refused,
/// full stop. A legitimate Win32 name never consists only of dots and spaces.
/// On Unix a trailing space or dot is a literal byte the shell does not
/// re-normalize, so only bare `.` / `..` (which canonicalize never emits) are
/// rejected there - refusing `"foo "` on Unix would break a legal name.
fn has_hidden_traversal(p: &Path) -> bool {
    p.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        if cfg!(windows) {
            let stripped = s.trim_end_matches(['.', ' ']);
            return stripped != s || stripped.is_empty() || stripped == "." || stripped == "..";
        }
        s == "." || s == ".."
    })
}

/// Windows `canonicalize` returns verbatim paths (`\\?\C:\...`, `\\?\UNC\...`)
/// that the containment check needs but downstream consumers (template fields
/// rendered into argv, cmd.exe, msys tools) cannot parse. The decision is made
/// on the verbatim form; hand back the ordinary spelling of the same resolved
/// path so a resumed run's stderr_file/output_file stays usable.
#[cfg(windows)]
fn deverbatim(p: PathBuf) -> PathBuf {
    let Some(s) = p.to_str() else { return p };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

#[cfg(not(windows))]
fn deverbatim(p: PathBuf) -> PathBuf {
    p
}

/// Read a regular file WITHOUT following a link on the final component. Unix:
/// O_NOFOLLOW (ELOOP if the final component is a symlink). Windows: refuse
/// up front when the final component is a reparse point, then open with
/// FILE_FLAG_OPEN_REPARSE_POINT so the open cannot chase a link planted between
/// the check and the open. Used for every read of a run-dir artifact, so a
/// symlink at a fixed name (log.jsonl, status.json, sfh-nonce, a chain file)
/// reads as an error instead of following out of the run dir (rev_break #6).
fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        if path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            return Err(std::io::Error::other("refusing to read through a symlink"));
        }
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        std::fs::File::open(path)
    }
}

fn read_nofollow(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = open_nofollow(path)?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}

/// `read_nofollow` that stops after `max_bytes`. Used where the file is judged
/// rather than handed on (route predicates): the cap bounds a runaway stderr,
/// and the truncation can land mid-character, so the bytes are decoded lossily
/// instead of failing the whole read the way `read_to_string` would.
fn read_nofollow_capped(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::Read;
    let f = open_nofollow(path)?;
    let mut buf = Vec::new();
    f.take(max_bytes).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Read a file that must be contained within `base`; a missing file reads as
/// `Ok(None)` (see `contained_opt`). The read itself is no-follow on the final
/// component (see `read_nofollow`).
///
/// Residual TOCTOU (rev_break #5): containment is decided by canonicalizing in
/// `contained_opt`, then the path is re-opened here by name. The no-follow open
/// closes the window for the FINAL component (a last-second swap to a symlink
/// fails the open), but an attacker who can modify the run dir BETWEEN the two
/// operations could still swap an INTERMEDIATE directory for an outward symlink.
/// sfh bounds that rather than closing it entirely: run dirs sfh creates are
/// 0700 (mkdir_private), and on --resume the run dir itself is verified not to
/// be a symlink and warned about when it is group/world writable (run_inner),
/// so racing an intermediate requires write access to a dir that is already the
/// user's. A fully handle-based open-then-verify (openat from an O_NOFOLLOW
/// directory fd) is not portable across Windows/macOS/Linux, so the
/// check-then-read form is kept with the access-control bound stated here.
pub fn read_contained_opt(base: &Path, candidate: &str) -> Result<Option<String>, String> {
    match contained_opt(base, candidate)? {
        Some(p) => read_nofollow(&p)
            .map(Some)
            .map_err(|e| format!("cannot read {}: {e}", p.display())),
        None => Ok(None),
    }
}

/// Contained read of an ABSOLUTE path that is expected to live under `base`
/// (e.g. the emit file or detached.out.txt recorded in status.json). A symlink
/// on the final component, or any resolution outside `base`, is a hard error;
/// a genuinely missing file reads as `Ok(None)` (rev_break #5/#6).
pub fn read_contained_abs(base: &Path, abs: &Path) -> Result<Option<String>, String> {
    read_contained_abs_inner(base, abs, None)
}

/// `read_contained_abs` that reads at most `max_bytes`. For artifacts a route
/// predicate only inspects (`<id>.err.txt`), where an unbounded read would let
/// a chatty child decide how much memory routing costs.
pub fn read_contained_abs_capped(
    base: &Path,
    abs: &Path,
    max_bytes: u64,
) -> Result<Option<String>, String> {
    read_contained_abs_inner(base, abs, Some(max_bytes))
}

fn read_contained_abs_inner(
    base: &Path,
    abs: &Path,
    max_bytes: Option<u64>,
) -> Result<Option<String>, String> {
    let canon_base = base
        .canonicalize()
        .map_err(|e| format!("cannot resolve run dir {}: {e}", base.display()))?;
    match abs.symlink_metadata() {
        Ok(md) => {
            if md.file_type().is_symlink() {
                return Err(format!(
                    "refusing to read '{}': it is a symlink (run artifacts must be regular files)",
                    abs.display()
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot stat {}: {e}", abs.display())),
    }
    let canon = abs
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", abs.display()))?;
    if !canon.starts_with(&canon_base) {
        return Err(format!(
            "refusing to read '{}': it resolves outside the run dir {}",
            abs.display(),
            canon_base.display()
        ));
    }
    match max_bytes {
        Some(n) => read_nofollow_capped(&canon, n),
        None => read_nofollow(&canon),
    }
    .map(Some)
    .map_err(|e| format!("cannot read {}: {e}", canon.display()))
}

/// Write the stop nonce for a run dir, binding the random token to the pid AND
/// the process start time that own the run. `sfh stop` requires the token, the
/// pid and (when recorded) the start time to match status.json and the live
/// process, so:
/// - a status.json rewritten to point at somebody else's process fails the pid
///   check even though an attacker who controls the directory can write both
///   files;
/// - a pid REUSED by an unrelated process after the run died fails the start
///   time check (the old (pid, nonce) pair could not tell a reused pid from the
///   original process, so `sfh stop` could kill an unrelated sfh whose name
///   matched - rev_break #8).
pub fn write_nonce(dir: &Path, pid: u32, start: Option<u64>, nonce: &str) -> std::io::Result<()> {
    let body = match start {
        Some(s) => format!("{pid} {s} {nonce}"),
        None => format!("{pid} {nonce}"),
    };
    // The detaching parent and child intentionally publish the same binding.
    // Replace atomically so a stop/status reader never observes the truncate
    // window between those concurrent writers.
    write_private_atomic(&dir.join("sfh-nonce"), body)
}

/// A parsed sfh-nonce file.
#[derive(Debug, PartialEq, Eq)]
pub enum Nonce {
    /// Current format: the token bound to the owning process. `start` is the
    /// process start time sfh >= 1.x records; a file written by the first
    /// pid-binding version has none.
    Bound {
        pid: u32,
        start: Option<u64>,
        nonce: String,
    },
    /// A bare token written before pid binding existed.
    Legacy { nonce: String },
}

/// Parse an sfh-nonce file: "<pid> <start> <nonce>" (current), "<pid> <nonce>"
/// (first pid-binding version) or a bare "<nonce>" (before pid binding).
/// Malformed content is an ERROR, never a silent Legacy fallback: the old
/// parser returned `None` for an unparseable pid and the caller then SKIPPED
/// the pid check, so a corrupted or crafted nonce file failed OPEN (rev_break
/// #9). A file that cannot be parsed as any known format is refused outright.
pub fn parse_nonce(raw: &str) -> Result<Nonce, String> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    match parts.len() {
        1 => Ok(Nonce::Legacy {
            nonce: parts[0].to_string(),
        }),
        2 => {
            let pid = parts[0]
                .parse::<u32>()
                .map_err(|_| format!("invalid pid '{}' in the sfh-nonce file", parts[0]))?;
            Ok(Nonce::Bound {
                pid,
                start: None,
                nonce: parts[1].to_string(),
            })
        }
        3 => {
            let pid = parts[0]
                .parse::<u32>()
                .map_err(|_| format!("invalid pid '{}' in the sfh-nonce file", parts[0]))?;
            let start = parts[1].parse::<u64>().map_err(|_| {
                format!(
                    "invalid process start time '{}' in the sfh-nonce file",
                    parts[1]
                )
            })?;
            Ok(Nonce::Bound {
                pid,
                start: Some(start),
                nonce: parts[2].to_string(),
            })
        }
        0 => Err("the sfh-nonce file is empty".to_string()),
        _ => Err("the sfh-nonce file has too many fields".to_string()),
    }
}

/// Verify that `child` is contained within `parent` (both must exist).
pub fn is_under(parent: &Path, child: &Path) -> bool {
    match (parent.canonicalize(), child.canonicalize()) {
        (Ok(p), Ok(c)) => c.starts_with(p),
        _ => false,
    }
}

/// Generate a 128-bit random nonce as a 32-char hex string.
pub fn random_nonce() -> String {
    use std::fmt::Write as _;
    let mut buf = [0u8; 16];
    fill_random(&mut buf);
    let mut out = String::with_capacity(buf.len() * 2);
    for byte in buf {
        write!(&mut out, "{byte:02x}").expect("writing to a String cannot fail");
    }
    out
}

// --- owner-only permissions for run artifacts -------------------------------
// Run dirs hold rendered prompts, model output and session ids in plaintext,
// so sfh's own writes are forced to 0700 (directories) / 0600 (files) on Unix
// instead of whatever the umask would grant (0755/0644 under the usual 022).
// On Windows there is no chmod equivalent that is cheap enough to wrap every
// write: a real restriction needs an explicit DACL built and applied through
// SetNamedSecurityInfo, which is deliberately not done here - Windows keeps
// the inherited ACL (under a user profile that is normally the user, SYSTEM
// and Administrators, not other local users).

/// FILE_FLAG_OPEN_REPARSE_POINT: open the reparse point itself instead of its
/// target, so a planted symlink/junction is NOT followed to a file outside the
/// run dir. The Windows counterpart of Unix O_NOFOLLOW (rev_break #2).
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

/// Write a file that must end up 0600 on Unix. Created with mode 0600 so the
/// plaintext never exists world-readable, then re-checked for pre-existing
/// files whose older permissions would otherwise survive the rewrite.
///
/// The open does not follow a link on the final component on ANY platform: a
/// run dir is untrusted input on --resume, so a symlink planted at a fixed name
/// (sfh-nonce, notes.md, log.jsonl, a step's prompt) must not redirect sfh's
/// write to a file outside the run dir. Unix: O_NOFOLLOW fails the open (ELOOP)
/// when the final component is a symlink. Windows: FILE_FLAG_OPEN_REPARSE_POINT
/// opens the reparse point itself, so writing a planted symlink/junction does
/// not truncate and fill its target (rev_break #2 - the old Windows branch was
/// a plain std::fs::write, which followed reparse points). Both directions are
/// fail-closed: a regular file or a fresh creation opens normally (rev_break #1).
/// Intermediate directories are still resolved by the kernel - the
/// canonicalize-before-open check in `contained_opt` bounds those, and run dirs
/// are 0700 so a local attacker cannot swap an intermediate after the fact.
pub fn write_private(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        f.write_all(contents.as_ref())?;
        let mut p = f.metadata()?.permissions();
        if p.mode() & 0o777 != 0o600 {
            p.set_mode(0o600);
            f.set_permissions(p)?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        f.write_all(contents.as_ref())
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        std::fs::write(path, contents.as_ref())
    }
}

/// `write_private`, plus a durability barrier before returning. Used for the
/// temporary side of write-then-rename state snapshots: the rename must never
/// publish bytes that are still only in a userspace/kernel write buffer.
pub fn write_private_sync(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        f.write_all(contents.as_ref())?;
        let mut p = f.metadata()?.permissions();
        if p.mode() & 0o777 != 0o600 {
            p.set_mode(0o600);
            f.set_permissions(p)?;
        }
        f.sync_all()?;
        sync_parent_directory(path)
    }
    #[cfg(windows)]
    {
        use std::io::Write;
        use std::os::windows::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        f.write_all(contents.as_ref())?;
        f.sync_all()
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let mut f = std::fs::File::create(path)?;
        use std::io::Write;
        f.write_all(contents.as_ref())?;
        f.sync_all()
    }
}

/// Persist a newly created file name (or an atomic replacement) as well as its
/// bytes. Some Unix filesystems do not support fsync on directories; in that
/// case the file barrier is still the strongest portable guarantee available.
#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    match std::fs::File::open(parent)?.sync_all() {
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Durably write a private temporary file and atomically replace `path`.
/// The temporary name includes the process id and a process-local sequence so
/// neither separate sfh processes nor concurrent writers inside one process
/// ever share an in-flight file.
pub fn write_private_atomic(path: &Path, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    static PUBLISH_LOCK: Mutex<()> = Mutex::new(());

    // MoveFileExW may transiently reject two simultaneous replacements of the
    // same destination even though both source names are unique. sfh's status
    // writers are cheap and infrequent, so serializing the publish boundary is
    // both simpler and more deterministic than platform-specific retry loops.
    let _publish = PUBLISH_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = PathBuf::from(tmp);
    let result = write_private_sync(&tmp, contents)
        .and_then(|_| atomic_replace_file(&tmp, path))
        .and_then(|_| sync_parent_directory(path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(not(windows))]
fn atomic_replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}

#[cfg(windows)]
fn atomic_replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FileRenameInfoEx, MoveFileExW, SetFileInformationByHandle, FILE_RENAME_INFO,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_REPLACE_EXISTING,
        MOVEFILE_WRITE_THROUGH,
    };

    // FileRenameInfoEx with POSIX replacement semantics (Windows 10 1709+)
    // can retire the old directory entry while a reader still has that old
    // file open. MoveFileExW alone repeatedly returns sharing/access errors in
    // that case, so a status poller can otherwise starve the heartbeat writer.
    //
    // The constants live in a windows-sys feature unrelated to their API; keep
    // the documented values local instead of enabling a broad module solely
    // for two integers. If the filesystem/OS does not support the extended
    // rename class, fall back to MoveFileExW below.
    const DELETE_ACCESS: u32 = 0x0001_0000;
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;
    const FILE_RENAME_REPLACE_IF_EXISTS: u32 = 0x0000_0001;
    const FILE_RENAME_POSIX_SEMANTICS: u32 = 0x0000_0002;

    let destination = if to.is_absolute() {
        to.to_path_buf()
    } else {
        std::env::current_dir()?.join(to)
    };
    let destination_wide: Vec<u16> = destination.as_os_str().encode_wide().collect();
    let base_size = std::mem::size_of::<FILE_RENAME_INFO>();
    let extra_chars = destination_wide.len().saturating_sub(1);
    let buffer_size = base_size
        .checked_add(extra_chars.saturating_mul(std::mem::size_of::<u16>()))
        .ok_or_else(|| std::io::Error::other("rename information buffer is too large"))?;
    if let Ok(source) = std::fs::OpenOptions::new()
        .access_mode(DELETE_ACCESS | SYNCHRONIZE_ACCESS)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(from)
    {
        let words = buffer_size.div_ceil(std::mem::size_of::<usize>());
        let mut storage = vec![0usize; words];
        let info = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        let rename_ok = unsafe {
            (*info).Anonymous.Flags = FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS;
            (*info).RootDirectory = std::ptr::null_mut();
            (*info).FileNameLength = u32::try_from(
                destination_wide
                    .len()
                    .saturating_mul(std::mem::size_of::<u16>()),
            )
            .map_err(|_| std::io::Error::other("destination path is too long"))?;
            std::ptr::copy_nonoverlapping(
                destination_wide.as_ptr(),
                (*info).FileName.as_mut_ptr(),
                destination_wide.len(),
            );
            SetFileInformationByHandle(
                source.as_raw_handle().cast(),
                FileRenameInfoEx,
                info.cast(),
                u32::try_from(buffer_size)
                    .map_err(|_| std::io::Error::other("rename buffer is too large"))?,
            )
        };
        if rename_ok != 0 {
            return Ok(());
        }
    }

    let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    for attempt in 0..=20 {
        let ok = unsafe {
            MoveFileExW(
                from_wide.as_ptr(),
                to_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // A status/nonce reader, the detaching sibling process, or an AV
        // scanner can briefly hold the destination against replacement.
        // Retry only Windows' access/share/lock failures; malformed paths and
        // real filesystem errors still fail immediately.
        let transient = matches!(error.raw_os_error(), Some(5 | 32 | 33));
        if !transient || attempt == 20 {
            return Err(error);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    unreachable!("the bounded replacement loop always returns")
}

/// Open (creating if needed) an append-only file that must be 0600 on Unix.
/// No-follow on the final component for the same reason as `write_private`
/// (rev_break #1, and rev_break #2 for the Windows branch).
pub fn append_private(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let mut p = f.metadata()?.permissions();
        if p.mode() & 0o777 != 0o600 {
            p.set_mode(0o600);
            f.set_permissions(p)?;
        }
        Ok(f)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    }
}

/// Create (or truncate) a file for writing WITHOUT following a symlink on the
/// final component. Used for the detached run's stdio redirections, which land
/// in a run dir that may be untrusted on a resumed --detach. Unix: O_NOFOLLOW.
/// Windows: FILE_FLAG_OPEN_REPARSE_POINT opens the reparse point itself rather
/// than its target, so a planted symlink/junction is not followed to a file
/// outside the run dir (rev_break #1).
pub fn create_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        std::fs::File::create(path)
    }
}

/// create_dir_all plus 0700 on Unix - but ONLY for directories this call
/// creates. A runs root the user already had (e.g. a group-shared 0770 dir
/// handed to --runs-dir) keeps its existing permissions: sfh may tighten what
/// it makes, never what it was given.
pub fn mkdir_private(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    let mut created: Vec<PathBuf> = Vec::new();
    #[cfg(unix)]
    {
        // Walk up to the deepest existing ancestor; everything below it is ours.
        let mut cur: &Path = path;
        loop {
            match std::fs::metadata(cur) {
                Ok(_) => break,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    created.push(cur.to_path_buf());
                    match cur.parent() {
                        Some(p) if !p.as_os_str().is_empty() => cur = p,
                        _ => break,
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for d in created {
            if let Ok(m) = std::fs::metadata(&d) {
                let mut p = m.permissions();
                if p.mode() & 0o777 != 0o700 {
                    p.set_mode(0o700);
                    std::fs::set_permissions(&d, p)?;
                }
            }
        }
    }
    Ok(())
}

/// Create exactly the final directory component, failing atomically if a
/// concurrent process already created it. Missing parents are made private
/// first; the final `create_dir` is the ownership boundary.
pub fn mkdir_private_new(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        mkdir_private(parent)?;
    }
    std::fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

/// Restrict a file that was created through a raw handle (tee streams and the
/// detached run's stdio redirections). No-op outside Unix; see above.
pub fn restrict_file(f: &std::fs::File) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = f.metadata() {
            let mut p = m.permissions();
            p.set_mode(0o600);
            let _ = f.set_permissions(p);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = f;
    }
}

#[cfg(unix)]
fn fill_random(buf: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(buf);
    }
}

#[cfg(windows)]
fn fill_random(buf: &mut [u8]) {
    use windows_sys::Win32::Security::Cryptography::BCryptGenRandom;
    unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            2, // BCRYPT_USE_SYSTEM_PREFERRED_RNG
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_lease_is_exclusive_and_released_on_drop() {
        let dir = std::env::temp_dir().join(format!("sfh-run-lease-{}", random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = try_run_lease(&dir).unwrap();
        assert!(matches!(try_run_lease(&dir), Err(RunLeaseError::Busy)));
        drop(first);
        let second = try_run_lease(&dir).unwrap();
        drop(second);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn private_atomic_write_replaces_an_existing_file() {
        let dir = std::env::temp_dir().join(format!("sfh-atomic-status-{}", random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("status.json");
        write_private_atomic(&path, b"{\"state\":\"running\"}").unwrap();
        write_private_atomic(&path, b"{\"state\":\"done\"}").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"state\":\"done\"}"
        );
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("status.json.tmp.")),
            "temporary files must be consumed by the atomic replace"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn private_atomic_writers_do_not_share_a_temporary_file() {
        use std::sync::{Arc, Barrier};

        let dir =
            std::env::temp_dir().join(format!("sfh-atomic-status-concurrent-{}", random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Arc::new(dir.join("status.json"));
        let barrier = Arc::new(Barrier::new(8));
        let mut writers = Vec::new();
        for writer in 0..8 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            writers.push(std::thread::spawn(move || {
                let payload = format!(
                    "{{\"writer\":{writer},\"padding\":\"{}\"}}",
                    "x".repeat(4096)
                );
                barrier.wait();
                write_private_atomic(&path, payload)
            }));
        }
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&*path).unwrap()).unwrap();
        assert!(value["writer"].as_u64().is_some());
        assert_eq!(value["padding"].as_str().unwrap().len(), 4096);
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("status.json.tmp.")),
            "temporary files must not leak after concurrent writes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn private_atomic_replacement_never_exposes_missing_or_partial_json() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Barrier};

        let dir = std::env::temp_dir().join(format!("sfh-atomic-reader-{}", random_nonce()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = Arc::new(dir.join("status.json"));
        let payload = |generation: u64| {
            format!(
                "{{\"generation\":{generation},\"padding\":\"{}\"}}",
                "x".repeat(8192)
            )
        };
        write_private_atomic(&path, payload(0)).unwrap();

        let start = Arc::new(Barrier::new(5));
        let finished = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        for _ in 0..4 {
            let path = Arc::clone(&path);
            let start = Arc::clone(&start);
            let finished = Arc::clone(&finished);
            readers.push(std::thread::spawn(move || {
                start.wait();
                let mut reads = 0usize;
                loop {
                    let text = std::fs::read_to_string(&*path)
                        .expect("an atomic replacement must keep the path readable");
                    let value: serde_json::Value =
                        serde_json::from_str(&text).expect("a reader must see one whole payload");
                    assert!(value["generation"].as_u64().is_some());
                    assert_eq!(value["padding"].as_str().map(str::len), Some(8192));
                    reads += 1;
                    if finished.load(Ordering::Acquire) && reads >= 2 {
                        break;
                    }
                }
                reads
            }));
        }

        start.wait();
        for generation in 1..=32 {
            write_private_atomic(&path, payload(generation)).unwrap();
        }
        finished.store(true, Ordering::Release);
        for reader in readers {
            assert!(reader.join().unwrap() >= 2);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_nonce_current_format() {
        let n = parse_nonce("12345 1753000000 abcdef0123456789abcdef01234567").unwrap();
        assert_eq!(
            n,
            Nonce::Bound {
                pid: 12345,
                start: Some(1753000000),
                nonce: "abcdef0123456789abcdef01234567".to_string()
            }
        );
    }

    #[test]
    fn parse_nonce_pid_only_format() {
        // The first pid-binding version wrote "<pid> <nonce>" with no start time.
        let n = parse_nonce("12345 abcdef0123456789abcdef01234567").unwrap();
        assert_eq!(
            n,
            Nonce::Bound {
                pid: 12345,
                start: None,
                nonce: "abcdef0123456789abcdef01234567".to_string()
            }
        );
    }

    #[test]
    fn parse_nonce_bare_format() {
        let n = parse_nonce("abcdef0123456789abcdef01234567").unwrap();
        assert_eq!(
            n,
            Nonce::Legacy {
                nonce: "abcdef0123456789abcdef01234567".to_string()
            }
        );
    }

    #[test]
    fn parse_nonce_trims_whitespace() {
        let n = parse_nonce("  999 1234 deadbeef  \n").unwrap();
        assert_eq!(
            n,
            Nonce::Bound {
                pid: 999,
                start: Some(1234),
                nonce: "deadbeef".to_string()
            }
        );
    }

    #[test]
    fn parse_nonce_rejects_malformed_content() {
        // rev_break #9: an unparseable pid must be an ERROR, not a silent
        // fallback that skips the pid check.
        assert!(parse_nonce("notapid deadbeef").is_err());
        assert!(parse_nonce("123 notanumber deadbeef").is_err());
        assert!(parse_nonce("").is_err());
        assert!(parse_nonce("   \n").is_err());
        assert!(parse_nonce("1 2 3 4").is_err());
        // A pid that does not fit u32 is malformed too (rev_break #9: the old
        // `as u32` truncation let a huge pid wrap to a real one).
        assert!(parse_nonce("4294967296 deadbeef").is_err());
    }

    // The strict form is Windows-only: on Unix a trailing space or dot is a
    // literal byte no consumer re-normalizes, so `"foo "` is a legal name there.
    #[cfg(windows)]
    #[test]
    fn hidden_traversal_catches_trailing_space_and_dot_forms() {
        // rev_break #1: `".. "` trims to `".."`, strips to `""`, and the old
        // `t == "." || t == ".."` test let it through. Any component that the
        // Win32 non-verbatim parser would re-normalize is a traversal risk.
        assert!(has_hidden_traversal(Path::new(r"C:\run\.. ")));
        assert!(has_hidden_traversal(Path::new(r"C:\run\..")));
        assert!(has_hidden_traversal(Path::new(r"C:\run\foo. ")));
        assert!(has_hidden_traversal(Path::new(r"C:\run\...")));
        assert!(!has_hidden_traversal(Path::new(r"C:\run\foo.txt")));
        assert!(!has_hidden_traversal(Path::new(r"C:\run\foo.bar\baz")));
    }

    #[cfg(unix)]
    #[test]
    fn hidden_traversal_allows_literal_unix_names() {
        assert!(has_hidden_traversal(Path::new("/run/..")));
        // Legal literal names on Unix: no consumer strips these.
        assert!(!has_hidden_traversal(Path::new("/run/foo. ")));
        assert!(!has_hidden_traversal(Path::new("/run/.. ")));
        assert!(!has_hidden_traversal(Path::new("/run/foo.txt")));
    }

    #[test]
    fn random_nonce_is_32_hex() {
        let n = random_nonce();
        assert_eq!(n.len(), 32);
        assert!(n.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(n, random_nonce());
    }

    #[test]
    fn validate_relative_rejects_absolute_and_traversal() {
        assert!(validate_relative("foo/bar.txt").is_ok());
        assert!(validate_relative("/etc/passwd").is_err());
        assert!(validate_relative("../secret").is_err());
        assert!(validate_relative("a/../../b").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn deverbatim_restores_parseable_paths() {
        assert_eq!(
            deverbatim(PathBuf::from(r"\\?\C:\x\y.txt")),
            PathBuf::from(r"C:\x\y.txt")
        );
        assert_eq!(
            deverbatim(PathBuf::from(r"\\?\UNC\server\share\f")),
            PathBuf::from(r"\\server\share\f")
        );
        assert_eq!(
            deverbatim(PathBuf::from(r"C:\plain\path")),
            PathBuf::from(r"C:\plain\path")
        );
    }

    #[cfg(windows)]
    #[test]
    fn contained_opt_returns_a_non_verbatim_path() {
        let base = std::env::temp_dir().join(format!("sfh-contain-{}", random_nonce()));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("f.txt"), b"x").unwrap();
        let got = contained_opt(&base, "f.txt").unwrap().unwrap();
        let s = got.to_str().unwrap_or_default();
        assert!(!s.starts_with(r"\\?\"), "{s}");
        assert!(got.ends_with("f.txt"), "{s}");
        let _ = std::fs::remove_dir_all(&base);
    }

    // Symlink containment (rev_complete S1-4, rev_break #4). Unix-gated: creating
    // a symlink on Windows needs a privilege the test runner may not have.
    #[cfg(unix)]
    #[test]
    fn contained_opt_rejects_an_outward_symlink() {
        let root = std::env::temp_dir().join(format!("sfh-contain-sym-{}", random_nonce()));
        let base = root.join("run");
        let outside = root.join("secret.txt");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(&outside, b"secret").unwrap();
        // A file inside the run dir that symlinks OUT must be refused, not read.
        std::os::unix::fs::symlink(&outside, base.join("link.txt")).unwrap();
        let e =
            contained_opt(&base, "link.txt").expect_err("an outward symlink must be a hard error");
        assert!(e.contains("outside"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn contained_opt_rejects_missing_path_under_outward_symlink() {
        // rev_break #4: run/escape -> outside, then ask for escape/missing. The
        // target is NotFound, but the PARENT resolves outside the run dir, so it
        // must be a hard error, not Ok(None).
        let root = std::env::temp_dir().join(format!("sfh-contain-miss-{}", random_nonce()));
        let base = root.join("run");
        let outside = root.join("outside");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("escape")).unwrap();
        let e = contained_opt(&base, "escape/missing")
            .expect_err("a missing file under an outward symlink must be a hard error");
        assert!(e.contains("outside"), "{e}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn contained_opt_missing_file_inside_is_none() {
        // The ordinary case: a genuinely absent file whose parent IS inside the
        // run dir still reads as Ok(None).
        let root = std::env::temp_dir().join(format!("sfh-contain-none-{}", random_nonce()));
        let base = root.join("run");
        std::fs::create_dir_all(&base).unwrap();
        assert!(contained_opt(&base, "not-there.txt").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn capped_read_stops_at_the_cap_and_keeps_absence_absent() {
        // F6: when_stderr_matches judges <id>.err.txt, so the read is bounded -
        // a child that writes an enormous stderr must not decide how much memory
        // a routing decision costs. A missing file is still Ok(None) (the
        // fail-closed "no evidence" case), not an error.
        let base = std::env::temp_dir().join(format!("sfh-contain-cap-{}", random_nonce()));
        std::fs::create_dir_all(&base).unwrap();
        let f = base.join("big.err.txt");
        std::fs::write(&f, "x".repeat(100)).unwrap();
        let got = read_contained_abs_capped(&base, &f, 10).unwrap().unwrap();
        assert_eq!(got, "x".repeat(10));
        let whole = read_contained_abs_capped(&base, &f, 4 * 1024 * 1024)
            .unwrap()
            .unwrap();
        assert_eq!(whole.len(), 100);
        assert!(
            read_contained_abs_capped(&base, &base.join("gone.err.txt"), 16)
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn capped_read_does_not_fail_on_a_cut_multibyte_character() {
        // The cap counts bytes, so it can land inside a UTF-8 sequence. That has
        // to degrade to a replacement character rather than failing the read and
        // taking the whole run down with it.
        let base = std::env::temp_dir().join(format!("sfh-contain-cut-{}", random_nonce()));
        std::fs::create_dir_all(&base).unwrap();
        let f = base.join("wide.err.txt");
        std::fs::write(&f, "日本語").unwrap();
        let got = read_contained_abs_capped(&base, &f, 4).unwrap().unwrap();
        assert!(got.starts_with('日'), "{got:?}");
        assert_eq!(got.chars().count(), 2, "{got:?}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
