use std::path::{Path, PathBuf};

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
pub fn contained_opt(base: &Path, candidate: &str) -> Result<Option<PathBuf>, String> {
    validate_relative(candidate)?;
    let joined = base.join(candidate);
    match joined.symlink_metadata() {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "cannot stat '{}' under {}: {e}",
                candidate,
                base.display()
            ))
        }
    }
    let canon_base = base
        .canonicalize()
        .map_err(|e| format!("cannot resolve run dir {}: {e}", base.display()))?;
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
    Ok(Some(canon))
}

/// Read a file that must be contained within `base`; a missing file reads as
/// `Ok(None)` (see `contained_opt`).
pub fn read_contained_opt(base: &Path, candidate: &str) -> Result<Option<String>, String> {
    match contained_opt(base, candidate)? {
        Some(p) => std::fs::read_to_string(&p)
            .map(Some)
            .map_err(|e| format!("cannot read {}: {e}", p.display())),
        None => Ok(None),
    }
}

/// Write the stop nonce for a run dir, binding the random token to the pid
/// that owns the run. `sfh stop` requires BOTH the token and the pid to match
/// status.json, so a run dir copied elsewhere - or a status.json rewritten to
/// point at somebody else's process - fails the check even though an attacker
/// who controls the directory can write both files.
pub fn write_nonce(dir: &Path, pid: u32, nonce: &str) -> std::io::Result<()> {
    write_private(&dir.join("sfh-nonce"), format!("{pid} {nonce}"))
}

/// Parse an sfh-nonce file: "<pid> <nonce>" (current format) or a bare
/// "<nonce>" (a nonce written before pid binding existed).
pub fn parse_nonce(raw: &str) -> (Option<u32>, String) {
    let t = raw.trim();
    match t.split_once(' ') {
        Some((p, n)) if !n.trim().is_empty() => match p.trim().parse::<u32>() {
            Ok(pid) => (Some(pid), n.trim().to_string()),
            Err(_) => (None, t.to_string()),
        },
        _ => (None, t.to_string()),
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
    let mut buf = [0u8; 16];
    fill_random(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
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

/// Write a file that must end up 0600 on Unix. Created with mode 0600 so the
/// plaintext never exists world-readable, then re-checked for pre-existing
/// files whose older permissions would otherwise survive the rewrite.
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
            .open(path)?;
        f.write_all(contents.as_ref())?;
        let mut p = f.metadata()?.permissions();
        if p.mode() & 0o777 != 0o600 {
            p.set_mode(0o600);
            f.set_permissions(p)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents.as_ref())
    }
}

/// Open (creating if needed) an append-only file that must be 0600 on Unix.
pub fn append_private(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        let mut p = f.metadata()?.permissions();
        if p.mode() & 0o777 != 0o600 {
            p.set_mode(0o600);
            f.set_permissions(p)?;
        }
        Ok(f)
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
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
    fn parse_nonce_current_format() {
        let (pid, nonce) = parse_nonce("12345 abcdef0123456789abcdef01234567");
        assert_eq!(pid, Some(12345));
        assert_eq!(nonce, "abcdef0123456789abcdef01234567");
    }

    #[test]
    fn parse_nonce_bare_format() {
        let (pid, nonce) = parse_nonce("abcdef0123456789abcdef01234567");
        assert_eq!(pid, None);
        assert_eq!(nonce, "abcdef0123456789abcdef01234567");
    }

    #[test]
    fn parse_nonce_trims_whitespace() {
        let (pid, nonce) = parse_nonce("  999 deadbeef  \n");
        assert_eq!(pid, Some(999));
        assert_eq!(nonce, "deadbeef");
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
}
