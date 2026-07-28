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

/// Resolve `candidate` against `base` and verify it stays within `base`.
/// Rejects absolute paths and any `..` traversal that escapes the base.
/// Returns the canonicalized path on success.
pub fn contained(base: &Path, candidate: &str) -> Result<PathBuf, String> {
    validate_relative(candidate)?;
    let joined = base.join(candidate);
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
    Ok(canon)
}

/// Read a file that must be contained within `base`.
pub fn read_contained(base: &Path, candidate: &str) -> Result<String, String> {
    let path = contained(base, candidate)?;
    std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
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

/// create_dir_all plus 0700 on Unix for the final directory, so a run root or
/// run dir is not left world-traversable under a permissive umask.
pub fn mkdir_private(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::fs::metadata(path)?.permissions();
        p.set_mode(0o700);
        std::fs::set_permissions(path, p)?;
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
