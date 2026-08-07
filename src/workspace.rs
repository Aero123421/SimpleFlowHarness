//! Managed workspaces: where a run's side effects live, and the rules for
//! creating, fingerprinting and removing one.
//!
//! A workspace is not a Git worktree by definition - it is "the working
//! environment this run's effects belong to". v1.2 implements four backends
//! (`current`, `directory`, `git-worktree`, `auto`) and leaves the interesting
//! ones (copy, container, remote) to a later release without painting them into
//! a corner.
//!
//! Two rules govern everything here, and neither may be relaxed for
//! convenience:
//!
//! 1. **sfh deletes only what sfh made.** A path is removable only when its own
//!    ownership marker and the run's manifest agree on a nonce sfh generated.
//!    A user's own worktree, a repository checkout, or any directory that fails
//!    that check is left alone - with a warning, never a deletion.
//! 2. **Uncommitted work is never discarded automatically.** A dirty workspace
//!    is kept whatever the run's outcome. `sfh workspaces remove --discard` is
//!    the only path that drops changes, and it is something a human types.

use crate::{contain, execute, sha256, state};
use serde_json::json;
use std::path::{Path, PathBuf};

/// The file sfh drops inside a workspace it created. Its presence is necessary
/// but not sufficient for removal: the nonce must also match the run manifest.
pub const MARKER: &str = ".sfh-workspace";
/// The manifest saved in the RUN directory.
pub const MANIFEST: &str = "workspace.json";

/// A live workspace, as the engine sees it.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub id: String,
    pub mode: crate::flow::WorkspaceMode,
    pub source_root: PathBuf,
    /// Where steps actually run.
    pub path: PathBuf,
    pub base_ref: Option<String>,
    pub base_commit: Option<String>,
    pub branch: Option<String>,
    /// True only for a path sfh created and may therefore remove.
    pub created_by_sfh: bool,
    pub ownership_nonce: Option<String>,
    pub cleanup: crate::flow::WorkspaceCleanup,
}

impl Workspace {
    pub fn to_json(&self, last_checkpoint: Option<&str>) -> serde_json::Value {
        json!({
            "schema_version": 1,
            "workspace_id": self.id,
            "mode": self.mode.as_str(),
            "source_root": self.source_root.display().to_string(),
            "path": self.path.display().to_string(),
            "base_ref": self.base_ref,
            "base_commit": self.base_commit,
            "branch": self.branch,
            "created_by_sfh": self.created_by_sfh,
            "ownership_nonce": self.ownership_nonce,
            "cleanup": self.cleanup.as_str(),
            "last_checkpoint": last_checkpoint,
        })
    }

    /// Read a manifest back. Used by resume and by `sfh workspaces`.
    pub fn from_manifest(v: &serde_json::Value) -> Option<Workspace> {
        use crate::flow::{WorkspaceCleanup, WorkspaceMode};
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
        Some(Workspace {
            id: s("workspace_id")?,
            mode: match v.get("mode").and_then(|x| x.as_str())? {
                "current" => WorkspaceMode::Current,
                "directory" => WorkspaceMode::Directory,
                "git-worktree" => WorkspaceMode::GitWorktree,
                "auto" => WorkspaceMode::Auto,
                _ => return None,
            },
            source_root: PathBuf::from(s("source_root")?),
            path: PathBuf::from(s("path")?),
            base_ref: s("base_ref"),
            base_commit: s("base_commit"),
            branch: s("branch"),
            created_by_sfh: v
                .get("created_by_sfh")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            ownership_nonce: s("ownership_nonce"),
            cleanup: match v.get("cleanup").and_then(|x| x.as_str()) {
                Some("keep") => WorkspaceCleanup::Keep,
                _ => WorkspaceCleanup::Auto,
            },
        })
    }
}

/// Run `git` with no shell, inside `cwd`. `Err` carries git's own stderr, which
/// is normally the actionable part.
fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().map(|s| (*s).to_string()));
    let out = execute::run_cmd(
        &execute::Invocation::Argv(argv),
        None,
        Some(cwd),
        Some(std::time::Duration::from_secs(120)),
        &[],
        // Keep git out of any interactive prompt: a managed workspace is
        // created on a background path where a credential prompt would hang
        // forever rather than fail.
        &[
            ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
            ("GIT_OPTIONAL_LOCKS".to_string(), "0".to_string()),
        ],
        execute::Observe::default(),
    )
    .map_err(|e| format!("cannot run git: {e}"))?;
    if out.timed_out {
        return Err(format!("git {} timed out", args.join(" ")));
    }
    if out.exit_code != 0 {
        return Err(format!(
            "git {} failed (exit {}): {}",
            args.join(" "),
            out.exit_code,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The repository `dir` belongs to, or `None` when it is not in one.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    git(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(PathBuf::from)
}

/// A stable, filesystem-safe id for a repository, so two checkouts of the same
/// project do not collide under the state root. The digest is of the canonical
/// path, and the readable prefix is there for a human browsing the directory.
fn repo_id(root: &Path) -> String {
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let digest = sha256::hex(canon.to_string_lossy().as_bytes());
    let name = canon
        .file_name()
        .map(|n| sanitize(&n.to_string_lossy()))
        .unwrap_or_else(|| "repo".to_string());
    format!("{name}-{}", &digest[..16])
}

/// Keep only characters that mean the same thing on all three operating
/// systems, so a flow name or repository name cannot produce a path that is
/// legal on one and rejected (or reinterpreted) on another.
pub fn sanitize(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    // A leading dot would hide the directory, and a leading dash makes the name
    // read as a flag to every CLI that is later handed it (git, above all).
    // A trailing dot or space is silently stripped by Windows, which would make
    // two different names collide on one directory.
    while out.starts_with('.') || out.starts_with('-') {
        out.remove(0);
    }
    while out.ends_with('.') || out.ends_with('-') {
        out.pop();
    }
    out.truncate(60);
    if out.is_empty() {
        out.push_str("unnamed");
    }
    out
}

/// The branch a managed worktree gets. Collisions are resolved by the caller
/// with a deterministic suffix.
pub fn branch_name(flow_name: &str, run_id: &str) -> String {
    format!("sfh/{}/{}", sanitize(flow_name), sanitize(run_id))
}

/// Create the one managed worktree this run will use.
///
/// It is deliberately created OUTSIDE the repository: a worktree inside its own
/// repository shows up in every `git status`, gets swept up by build tooling,
/// and would be a directory sfh deletes sitting inside a directory sfh must
/// never delete.
pub fn create_git_worktree(
    source_root: &Path,
    state_root: &state::StateRoot,
    flow_name: &str,
    run_id: &str,
    base: Option<&str>,
) -> Result<Workspace, String> {
    let repo = repo_root(source_root).ok_or_else(|| {
        format!(
            "{} is not inside a Git repository, so sfh cannot create a managed worktree for it. Use workspace.mode: directory with an explicit root, or workspace.mode: current.",
            source_root.display()
        )
    })?;
    let base_ref = base.map(String::from);
    // Resolve the base to a commit NOW: "main" can move while the run is in
    // flight, and the closure has to pin what this run actually branched from.
    let base_commit = git(&repo, &["rev-parse", base.unwrap_or("HEAD")]).map_err(|e| {
        format!(
            "cannot resolve the workspace base {}: {e}",
            base.unwrap_or("HEAD")
        )
    })?;
    let root = state_root.managed_root()?;
    let dir = root.join(repo_id(&repo)).join(sanitize(run_id));
    let path = dir.join("primary");
    contain::mkdir_private(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    if path.exists() {
        return Err(format!(
            "{} already exists; refusing to reuse a workspace path sfh did not just create",
            path.display()
        ));
    }
    // Deterministic collision suffix: a re-run of the same flow at the same
    // second must not fail, and must not silently land on someone else's
    // branch either.
    let mut branch = branch_name(flow_name, run_id);
    for n in 1..64 {
        if git(&repo, &["rev-parse", "--verify", "--quiet", &branch]).is_err() {
            break;
        }
        branch = format!("{}-{n}", branch_name(flow_name, run_id));
    }
    let nonce = contain::random_nonce();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            &path.to_string_lossy(),
            &base_commit,
        ],
    )
    .map_err(|e| format!("cannot create the managed worktree: {e}"))?;
    // The marker goes in AFTER the worktree exists, and its content is what
    // makes removal legal later.
    //
    // It sits in the worktree root, where it is visible to `git status`. That
    // is deliberate rather than accidental: Git resolves `info/exclude` through
    // the COMMON directory even for a linked worktree, so the only way to hide
    // it would be to edit an exclude file inside the user's own repository -
    // and sfh writing to a user's repository to tidy up after itself is a worse
    // trade than one visible, self-describing file that says what this
    // directory is. `is_dirty` and `fingerprint` both discount it, so it never
    // makes a clean workspace look dirty or a still workspace look drifted.
    write_marker(&path, &nonce, run_id)?;
    Ok(Workspace {
        id: "primary".to_string(),
        mode: crate::flow::WorkspaceMode::GitWorktree,
        source_root: repo,
        path,
        base_ref,
        base_commit: Some(base_commit),
        branch: Some(branch),
        created_by_sfh: true,
        ownership_nonce: Some(nonce),
        cleanup: crate::flow::WorkspaceCleanup::Auto,
    })
}

fn write_marker(path: &Path, nonce: &str, run_id: &str) -> Result<(), String> {
    let marker = path.join(MARKER);
    let body = json!({
        "schema_version": 1,
        "created_by": "sfh",
        "sfh_version": env!("CARGO_PKG_VERSION"),
        "run_id": run_id,
        "ownership_nonce": nonce,
    });
    contain::write_private_atomic(&marker, body.to_string())
        .map_err(|e| format!("cannot write the workspace ownership marker: {e}"))
}

/// Whether sfh may delete `path`.
///
/// Both halves have to agree: the marker inside the directory, and the nonce
/// the run recorded when it created it. A marker alone proves nothing - anyone
/// can write one - and a manifest alone proves nothing either, because the path
/// it names may since have become something else entirely.
pub fn verify_ownership(ws: &Workspace) -> Result<(), String> {
    let Some(expected) = &ws.ownership_nonce else {
        return Err("sfh has no ownership nonce for this workspace".into());
    };
    if !ws.created_by_sfh {
        return Err("this workspace was not created by sfh".into());
    }
    let marker = ws.path.join(MARKER);
    // No-follow: a symlink at the marker name would let an attacker point the
    // check at a marker they control while the directory is someone else's.
    match marker.symlink_metadata() {
        Ok(md) if md.file_type().is_symlink() => {
            return Err(format!(
                "{} is a symlink; refusing to treat this path as sfh-owned",
                marker.display()
            ))
        }
        Ok(md) if !md.is_file() => {
            return Err(format!("{} is not a regular file", marker.display()))
        }
        Ok(_) => {}
        Err(e) => {
            return Err(format!(
                "no usable ownership marker at {}: {e}",
                marker.display()
            ))
        }
    }
    let text = std::fs::read_to_string(&marker)
        .map_err(|e| format!("cannot read {}: {e}", marker.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("{} is not valid JSON: {e}", marker.display()))?;
    let found = v.get("ownership_nonce").and_then(|x| x.as_str());
    if found != Some(expected.as_str()) {
        return Err(format!(
            "the ownership marker in {} does not match this run's nonce; refusing to touch a path sfh cannot prove it created",
            ws.path.display()
        ));
    }
    Ok(())
}

/// A content fingerprint of a Git workspace: HEAD, staged and unstaged changes,
/// every untracked regular file, and submodule state.
///
/// Hashing is streamed, and a file that cannot be read makes the whole
/// fingerprint UNKNOWN rather than being skipped. "I could not read it" and "it
/// is unchanged" are different answers, and only one of them may let a resume
/// proceed as if nothing had happened.
pub fn fingerprint(path: &Path) -> Result<String, String> {
    let head = git(path, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "no-head".to_string());
    let index = git(path, &["diff", "--cached", "--full-index"])?;
    let worktree = git(path, &["diff", "--full-index"])?;
    let submodules = git(path, &["submodule", "status", "--recursive"]).unwrap_or_default();
    // -z so a filename containing a newline cannot forge an extra entry.
    let untracked_raw = git(path, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    // sfh's own ownership marker is not the user's work: counting it would make
    // every managed workspace differ from an unmanaged one for no reason a
    // reader could act on, and would report drift the moment sfh wrote it.
    let mut untracked: Vec<&str> = untracked_raw
        .split('\0')
        .filter(|s| !s.is_empty() && *s != MARKER)
        .collect();
    untracked.sort_unstable();
    let mut acc = String::new();
    acc.push_str("head\0");
    acc.push_str(&head);
    acc.push_str("\0index\0");
    acc.push_str(&sha256::hex(index.as_bytes()));
    acc.push_str("\0worktree\0");
    acc.push_str(&sha256::hex(worktree.as_bytes()));
    acc.push_str("\0submodules\0");
    acc.push_str(&sha256::hex(submodules.as_bytes()));
    for rel in untracked {
        let file = path.join(rel);
        // A directory symlink among the untracked entries must not be followed
        // out of the workspace, and an unreadable file must not read as absent.
        let md = file
            .symlink_metadata()
            .map_err(|e| format!("cannot stat untracked {rel}: {e}"))?;
        acc.push('\0');
        acc.push_str(rel);
        acc.push('\0');
        if md.file_type().is_symlink() {
            let target = std::fs::read_link(&file)
                .map_err(|e| format!("cannot read the link {rel}: {e}"))?;
            acc.push_str("symlink:");
            acc.push_str(&sha256::hex(target.to_string_lossy().as_bytes()));
        } else if md.is_file() {
            acc.push_str(&hash_file_streaming(&file)?);
        } else {
            acc.push_str("other");
        }
    }
    Ok(sha256::hex(acc.as_bytes()))
}

/// Hash a file without ever holding it in memory. A large untracked artifact is
/// a normal thing to find in a workspace, and it must be hashed rather than
/// waved through as "too big to check".
fn hash_file_streaming(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f =
        std::fs::File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut hasher = sha256::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finish_hex())
}

/// Whether a Git workspace has anything uncommitted. A dirty workspace is never
/// removed automatically, whatever the run's outcome.
pub fn is_dirty(path: &Path) -> Result<bool, String> {
    let status = git(path, &["status", "--porcelain", "--untracked-files=all"])?;
    // Same exclusion the fingerprint makes: sfh's own marker is not a change
    // the user made, and treating it as one would mean a clean, successful run
    // never qualified for cleanup - the workspace would always read as dirty.
    Ok(status
        .lines()
        .filter(|l| !l.trim().is_empty())
        .any(|l| l.get(3..).map(|p| p.trim()) != Some(MARKER)))
}

/// The outcome of an automatic cleanup attempt. A cleanup that declines - or
/// fails - never turns a successful run into a failed one; it leaves evidence.
pub enum Cleanup {
    Removed,
    KeptDirty,
    KeptState(String),
    KeptUnowned(String),
    Failed(String),
    NotApplicable,
}

impl Cleanup {
    pub fn as_json(&self) -> serde_json::Value {
        match self {
            Cleanup::Removed => json!({"action": "removed"}),
            Cleanup::KeptDirty => json!({
                "action": "kept",
                "reason": "the workspace has uncommitted changes; sfh never discards them automatically"
            }),
            Cleanup::KeptState(s) => json!({
                "action": "kept",
                "reason": format!("the run ended as '{s}', so its workspace is preserved for inspection")
            }),
            Cleanup::KeptUnowned(why) => json!({"action": "kept", "reason": why}),
            Cleanup::Failed(e) => json!({"action": "failed", "reason": e}),
            Cleanup::NotApplicable => json!({"action": "none"}),
        }
    }
}

/// Remove a managed worktree if - and only if - every condition holds:
/// cleanup is `auto`, the run finished cleanly, the workspace has no
/// uncommitted work, and sfh can prove it created the path.
///
/// The BRANCH is never deleted. It is the only remaining handle on what the run
/// produced, and it costs nothing to keep.
pub fn cleanup_auto(ws: &Workspace, run_state: &str) -> Cleanup {
    if ws.mode != crate::flow::WorkspaceMode::GitWorktree || !ws.created_by_sfh {
        return Cleanup::NotApplicable;
    }
    if ws.cleanup == crate::flow::WorkspaceCleanup::Keep {
        return Cleanup::KeptState("cleanup: keep".into());
    }
    if run_state != "done" {
        return Cleanup::KeptState(run_state.to_string());
    }
    if let Err(why) = verify_ownership(ws) {
        return Cleanup::KeptUnowned(why);
    }
    match is_dirty(&ws.path) {
        Ok(true) => return Cleanup::KeptDirty,
        Ok(false) => {}
        // Cannot tell: keep. The whole point of the check is that guessing
        // costs someone their work.
        Err(e) => {
            return Cleanup::KeptUnowned(format!("cannot determine whether it is dirty: {e}"))
        }
    }
    remove_worktree(ws).map_or_else(Cleanup::Failed, |_| Cleanup::Removed)
}

/// Ask Git to remove the worktree, then make sure the directory is gone.
///
/// Ownership is re-checked immediately before the removal rather than relying
/// on a check made earlier: between a decision and its execution the path can
/// be swapped, and this is the last moment at which that is detectable.
pub fn remove_worktree(ws: &Workspace) -> Result<(), String> {
    verify_ownership(ws)?;
    let path = ws
        .path
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", ws.path.display()))?;
    // The canonical path must still be the one the manifest names. A symlink
    // swapped in after the manifest was written would otherwise let a removal
    // land somewhere else entirely.
    if !contain::is_under(&ws.path, &path) {
        return Err(format!(
            "{} no longer resolves to itself; refusing to remove it",
            ws.path.display()
        ));
    }
    git(
        &ws.source_root,
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    )?;
    if path.exists() {
        return Err(format!(
            "git reported success but {} is still present",
            path.display()
        ));
    }
    Ok(())
}

/// What a resume found when it compared the workspace against its last durable
/// checkpoint.
pub enum Drift {
    /// Byte-identical to the checkpoint.
    None,
    /// Different. `unfinished` is true when a step was in flight when the run
    /// stopped, which makes the difference explainable rather than alarming.
    Changed {
        unfinished: bool,
    },
    /// The fingerprint could not be computed at all.
    Unknown(String),
    Missing,
}

/// Compare a workspace against its recorded checkpoint.
pub fn detect_drift(ws: &Workspace, checkpoint: Option<&str>, unfinished: bool) -> Drift {
    if !ws.path.is_dir() {
        return Drift::Missing;
    }
    let Some(expected) = checkpoint else {
        return Drift::None;
    };
    match fingerprint(&ws.path) {
        Ok(now) if now == expected => Drift::None,
        Ok(_) => Drift::Changed { unfinished },
        Err(e) => Drift::Unknown(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_produces_the_same_legal_name_on_every_os() {
        assert_eq!(sanitize("my flow"), "my-flow");
        // Path separators are gone, so what is left is one ordinary filename
        // that happens to contain dots - it can no longer traverse anywhere.
        assert_eq!(sanitize("a/../../b"), "a-..-..-b");
        // A leading dash would read as a flag to git, which is handed these.
        assert!(!sanitize("--force").starts_with('-'));
        assert!(!sanitize("../../etc/passwd").contains('/'));
        assert_eq!(sanitize(".."), "unnamed");
        assert_eq!(sanitize("/"), "unnamed");
        // Windows silently strips a trailing dot or space, which would make two
        // different names resolve to one directory.
        assert_eq!(sanitize("trailing."), "trailing");
        assert_eq!(sanitize("trailing "), "trailing");
        // A leading dot would hide the directory from a user looking for it.
        assert!(!sanitize(".hidden").starts_with('.'));
        assert_eq!(sanitize(""), "unnamed");
        assert!(sanitize(&"x".repeat(500)).len() <= 60);
        for s in ["日本語", "a:b", "a|b", "a*b", "a?b", "a\"b", "a<b>c"] {
            let out = sanitize(s);
            assert!(
                !out.contains(['/', '\\', ':', '|', '*', '?', '"', '<', '>']),
                "{s} -> {out} keeps a character Windows rejects"
            );
        }
    }

    #[test]
    fn a_branch_name_is_derived_from_the_flow_and_run_not_from_user_text() {
        let b = branch_name("../../evil", "20260808-120000-x");
        assert!(b.starts_with("sfh/"), "{b}");
        assert_eq!(b.matches('/').count(), 2, "no extra path components: {b}");
    }

    /// The rule that matters most in this module: sfh removes only what it can
    /// prove it created.
    #[test]
    fn ownership_is_refused_without_a_matching_marker() {
        let base = std::env::temp_dir().join(format!("sfh-ws-own-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let mut ws = Workspace {
            id: "primary".into(),
            mode: crate::flow::WorkspaceMode::GitWorktree,
            source_root: base.clone(),
            path: base.clone(),
            base_ref: None,
            base_commit: None,
            branch: None,
            created_by_sfh: true,
            ownership_nonce: Some("nonce-a".into()),
            cleanup: crate::flow::WorkspaceCleanup::Auto,
        };
        // No marker at all: a user's own directory.
        assert!(verify_ownership(&ws).is_err(), "no marker must not pass");
        // A marker with somebody else's nonce - e.g. a directory left behind by
        // a different run, or one an attacker planted.
        write_marker(&base, "nonce-b", "some-run").unwrap();
        let e = verify_ownership(&ws).unwrap_err();
        assert!(e.contains("does not match"), "{e}");
        // The matching nonce, which is the only thing that passes.
        write_marker(&base, "nonce-a", "this-run").unwrap();
        assert!(verify_ownership(&ws).is_ok());
        // A run that did not create the path can never remove it, marker or no.
        ws.created_by_sfh = false;
        assert!(verify_ownership(&ws).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn cleanup_keeps_everything_it_is_not_certain_about() {
        let ws = Workspace {
            id: "primary".into(),
            mode: crate::flow::WorkspaceMode::GitWorktree,
            source_root: PathBuf::from("/nope"),
            path: PathBuf::from("/nope/primary"),
            base_ref: None,
            base_commit: None,
            branch: None,
            created_by_sfh: true,
            ownership_nonce: Some("n".into()),
            cleanup: crate::flow::WorkspaceCleanup::Auto,
        };
        // A failed, stuck, stopped or dead run keeps its workspace: that is
        // where the evidence of what went wrong lives.
        for state in ["failed", "stuck", "stopped", "dead", "running"] {
            assert!(
                matches!(cleanup_auto(&ws, state), Cleanup::KeptState(_)),
                "a {state} run must keep its workspace"
            );
        }
        // `done` gets as far as the ownership check and stops there, because
        // this path is not sfh-owned.
        assert!(matches!(cleanup_auto(&ws, "done"), Cleanup::KeptUnowned(_)));
        let keep = Workspace {
            cleanup: crate::flow::WorkspaceCleanup::Keep,
            ..ws.clone()
        };
        assert!(matches!(cleanup_auto(&keep, "done"), Cleanup::KeptState(_)));
        // A non-managed workspace is never a cleanup candidate at all.
        let current = Workspace {
            mode: crate::flow::WorkspaceMode::Current,
            ..ws
        };
        assert!(matches!(
            cleanup_auto(&current, "done"),
            Cleanup::NotApplicable
        ));
    }

    #[test]
    fn a_manifest_round_trips() {
        let ws = Workspace {
            id: "primary".into(),
            mode: crate::flow::WorkspaceMode::GitWorktree,
            source_root: PathBuf::from("/repo"),
            path: PathBuf::from("/state/ws/primary"),
            base_ref: Some("main".into()),
            base_commit: Some("abc123".into()),
            branch: Some("sfh/f/r".into()),
            created_by_sfh: true,
            ownership_nonce: Some("n1".into()),
            cleanup: crate::flow::WorkspaceCleanup::Auto,
        };
        let back = Workspace::from_manifest(&ws.to_json(Some("fp"))).expect("round trip");
        assert_eq!(back.path, ws.path);
        assert_eq!(back.branch, ws.branch);
        assert_eq!(back.ownership_nonce, ws.ownership_nonce);
        assert!(back.created_by_sfh);
        // A manifest that claims ownership without a nonce cannot round-trip
        // into something removable.
        let mut forged = ws.to_json(None);
        forged["ownership_nonce"] = serde_json::Value::Null;
        let back = Workspace::from_manifest(&forged).unwrap();
        assert!(verify_ownership(&back).is_err());
    }
}
