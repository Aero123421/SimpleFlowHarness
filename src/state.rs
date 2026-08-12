//! Where sfh keeps state that is not the flow.
//!
//! Run artifacts have always lived in `.sfh/runs` next to the flow, and they
//! still do when nothing says otherwise: that default is what every existing
//! flow, script and CI job depends on, and v1.2 does not move it.
//!
//! What v1.2 adds is a state ROOT for the things that should not live inside a
//! repository at all - a managed Git worktree, above all, which cannot be
//! created under the repository it is a worktree of. `--state-dir` (or
//! `SFH_STATE_DIR`) names one explicitly; a managed workspace with neither
//! falls back to the platform's user-state directory, and if the environment
//! does not name one it is an error rather than a silent write into the user's
//! repository.

use std::path::{Path, PathBuf};

/// The resolved locations a run may write to.
#[derive(Clone, Debug)]
pub struct StateRoot {
    /// Explicit `--state-dir` / `SFH_STATE_DIR`, if any.
    root: Option<PathBuf>,
    /// Explicit `--runs-dir`, which overrides runs and nothing else.
    runs_override: Option<PathBuf>,
}

/// The legacy, still-default run artifact root.
pub fn default_runs_dir() -> PathBuf {
    PathBuf::from(".sfh").join("runs")
}

impl StateRoot {
    /// `cli_state_dir` wins over `SFH_STATE_DIR`; `--runs-dir` is independent of
    /// both and continues to mean exactly "put run artifacts here".
    pub fn resolve(cli_state_dir: Option<&Path>, runs_override: Option<&Path>) -> StateRoot {
        let root = cli_state_dir.map(PathBuf::from).or_else(|| {
            std::env::var_os("SFH_STATE_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        });
        StateRoot {
            root,
            runs_override: runs_override.map(PathBuf::from),
        }
    }

    /// True when the caller named a state root. Nothing is inferred: the
    /// platform default is reached for only by the features that cannot work
    /// without a root at all (see `managed_root`).
    pub fn is_explicit(&self) -> bool {
        self.root.is_some()
    }

    pub fn explicit(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// Where run artifacts go. `--runs-dir` first, then `<state>/runs`, then
    /// the historical `.sfh/runs`.
    pub fn runs_dir(&self) -> PathBuf {
        if let Some(r) = &self.runs_override {
            return r.clone();
        }
        match &self.root {
            Some(root) => root.join("runs"),
            None => default_runs_dir(),
        }
    }

    /// Where `plan --save` writes.
    pub fn plans_dir(&self) -> Option<PathBuf> {
        self.root.as_ref().map(|r| r.join("plans"))
    }

    /// Where `doctor` gets its scratch cwd. Falls back to the OS temp dir,
    /// which is fine here: a doctor probe is disposable and short-lived.
    pub fn doctor_dir(&self) -> Option<PathBuf> {
        self.root.as_ref().map(|r| r.join("doctor"))
    }

    /// Where a managed workspace may be created.
    ///
    /// This is the one caller allowed to fall back to the platform user-state
    /// directory, because a managed Git worktree has nowhere else it could
    /// legally go: it must not be created inside the repository it branches
    /// from. When the environment names no such directory the answer is an
    /// error, never a path inside the user's project.
    pub fn managed_root(&self) -> Result<PathBuf, String> {
        if let Some(root) = &self.root {
            return Ok(root.join("workspaces"));
        }
        Ok(platform_state_dir()?.join("workspaces"))
    }
}

/// `$XDG_STATE_HOME/sfh`, `$HOME/.local/state/sfh`, or `%LOCALAPPDATA%\sfh`.
pub fn platform_state_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
            return Ok(PathBuf::from(local).join("sfh"));
        }
        Err(
            "no state directory: %LOCALAPPDATA% is unset, so sfh cannot pick a safe place for managed workspaces. Pass --state-dir <dir> or set SFH_STATE_DIR."
                .to_string(),
        )
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
            let xdg = PathBuf::from(xdg);
            // A relative XDG_STATE_HOME is undefined by the spec and would put
            // state wherever sfh happens to be running from - which for a
            // managed workspace is the repository this must stay out of.
            if xdg.is_absolute() {
                return Ok(xdg.join("sfh"));
            }
        }
        if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            let home = PathBuf::from(home);
            if home.is_absolute() {
                return Ok(home.join(".local").join("state").join("sfh"));
            }
        }
        Err(
            "no state directory: neither XDG_STATE_HOME nor HOME names an absolute path, so sfh cannot pick a safe place for managed workspaces. Pass --state-dir <dir> or set SFH_STATE_DIR."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_legacy_runs_default_is_untouched_when_nothing_is_asked_for() {
        // The single most important compatibility property of this module.
        let s = StateRoot {
            root: None,
            runs_override: None,
        };
        assert_eq!(s.runs_dir(), PathBuf::from(".sfh").join("runs"));
        assert!(!s.is_explicit());
        assert_eq!(s.plans_dir(), None);
    }

    #[test]
    fn a_state_root_moves_runs_but_an_explicit_runs_dir_still_wins() {
        let root = PathBuf::from("/state");
        let s = StateRoot {
            root: Some(root.clone()),
            runs_override: None,
        };
        assert_eq!(s.runs_dir(), root.join("runs"));
        assert_eq!(s.plans_dir(), Some(root.join("plans")));
        assert_eq!(s.doctor_dir(), Some(root.join("doctor")));
        assert_eq!(s.managed_root().unwrap(), root.join("workspaces"));
        let pinned = StateRoot {
            root: Some(root.clone()),
            runs_override: Some(PathBuf::from("/elsewhere/runs")),
        };
        assert_eq!(pinned.runs_dir(), PathBuf::from("/elsewhere/runs"));
        // --runs-dir moves runs and NOTHING else.
        assert_eq!(pinned.managed_root().unwrap(), root.join("workspaces"));
    }

    #[test]
    fn a_managed_workspace_never_silently_lands_in_the_project() {
        // With no state root the answer is either the platform user-state
        // directory or an error - never a relative path, which would resolve
        // inside whatever repository sfh was invoked from.
        let s = StateRoot {
            root: None,
            runs_override: None,
        };
        match s.managed_root() {
            Ok(p) => assert!(
                p.is_absolute(),
                "a managed workspace root must be absolute, got {}",
                p.display()
            ),
            Err(e) => assert!(
                e.contains("--state-dir"),
                "the error must say how to fix it: {e}"
            ),
        }
    }
}
