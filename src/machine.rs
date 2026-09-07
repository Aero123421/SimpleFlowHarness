//! The machine-readable surface: one envelope shape and a fixed set of error
//! codes.
//!
//! sfh is increasingly driven by another program - a script, a CI job, or an AI
//! agent that has to decide what to do next from what sfh just said. Prose is a
//! bad interface for that: the wording of a message is allowed to improve, and
//! a caller that greps for it breaks when it does. So `run`, `plan`, `wait`,
//! `stop`, `status`, `preflight` and `workspaces` (`list`/`show`/`clean`/
//! `remove`) answer `--json` with the header built here, and every failure
//! among them carries a code whose MEANING is fixed for as long as
//! `SCHEMA_VERSION` below does not change, even as its message changes.
//!
//! `validate --json` and `runs list|show|why --json` predate this module and
//! still print their own bare JSON instead: no `schema_version`, no
//! `command`, no `exit_code`, no code from `ErrorCode` at all. A caller has to
//! special-case those four rather than assume the header below is universal -
//! see docs/machine-api.md for the exact shape each one answers with and the
//! plan to unify them onto this envelope.
//!
//! Two rules keep the envelope usable:
//!
//! - In JSON mode, stdout carries JSON and nothing else. Progress, warnings and
//!   human notes go to stderr. A caller may `sfh ... --json | jq` safely.
//! - A configuration or usage error is still an envelope. "sfh printed prose
//!   and exited 2" is precisely the case a machine caller cannot parse, and it
//!   is the case most likely to happen when something is wrong.

use serde_json::json;

/// The stable failure vocabulary. Messages may be reworded; these may not be
/// re-pointed at a different meaning while `SCHEMA_VERSION` stays the same.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorCode {
    /// The command line itself was wrong.
    Usage,
    /// The flow file could not be loaded or failed static validation.
    FlowInvalid,
    /// A step failed at run time and the declared control flow ended the run.
    StepFailed,
    /// A structured tool protocol did not hold (see src/protocol.rs).
    ProtocolInvalid,
    /// A structured protocol ended without its documented terminal record.
    TerminalMissing,
    /// A resume or fork could not prove it landed in the expected session.
    SessionUnverified,
    /// The pinned execution inputs differ from the run being resumed.
    ExecutionClosureChanged,
    /// A managed workspace that should exist does not.
    WorkspaceMissing,
    /// A managed workspace changed underneath a resume.
    WorkspaceDrift,
    /// Another live run holds this workspace.
    WorkspaceBusy,
    /// Another live process owns this run directory.
    RunBusy,
    /// A path sfh was asked to manage is not one it created.
    WorkspaceUnowned,
    /// A replay policy refused to re-run an unfinished effect.
    ReplayRefused,
    /// A required durable artifact could not be written.
    PersistenceFailure,
    /// A capability the flow requires is not available here.
    CapabilityUnavailable,
    /// The flow deliberately stopped for a human decision.
    Stuck,
    /// The run was stopped or its owning process disappeared.
    Interrupted,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "SFH_USAGE",
            ErrorCode::FlowInvalid => "SFH_FLOW_INVALID",
            ErrorCode::StepFailed => "SFH_STEP_FAILED",
            ErrorCode::ProtocolInvalid => "SFH_PROTOCOL_INVALID",
            ErrorCode::TerminalMissing => "SFH_TERMINAL_MISSING",
            ErrorCode::SessionUnverified => "SFH_SESSION_UNVERIFIED",
            ErrorCode::ExecutionClosureChanged => "SFH_EXECUTION_CLOSURE_CHANGED",
            ErrorCode::WorkspaceMissing => "SFH_WORKSPACE_MISSING",
            ErrorCode::WorkspaceDrift => "SFH_WORKSPACE_DRIFT",
            ErrorCode::WorkspaceBusy => "SFH_WORKSPACE_BUSY",
            ErrorCode::RunBusy => "SFH_RUN_BUSY",
            ErrorCode::WorkspaceUnowned => "SFH_WORKSPACE_UNOWNED",
            ErrorCode::ReplayRefused => "SFH_REPLAY_REFUSED",
            ErrorCode::PersistenceFailure => "SFH_PERSISTENCE_FAILURE",
            ErrorCode::CapabilityUnavailable => "SFH_CAPABILITY_UNAVAILABLE",
            ErrorCode::Stuck => "SFH_STUCK",
            ErrorCode::Interrupted => "SFH_INTERRUPTED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Usage,
            Self::FlowInvalid,
            Self::StepFailed,
            Self::ProtocolInvalid,
            Self::TerminalMissing,
            Self::SessionUnverified,
            Self::ExecutionClosureChanged,
            Self::WorkspaceMissing,
            Self::WorkspaceDrift,
            Self::WorkspaceBusy,
            Self::RunBusy,
            Self::WorkspaceUnowned,
            Self::ReplayRefused,
            Self::PersistenceFailure,
            Self::CapabilityUnavailable,
            Self::Stuck,
            Self::Interrupted,
        ]
        .into_iter()
        .find(|code| code.as_str() == value)
    }
}

/// The version of the envelope shape itself, independent of sfh's version.
/// `ErrorCode` meanings are pinned to this too, not to a release number: a
/// caller's cached understanding of a code is safe to keep using for as long
/// as this stays 1, and a bump here - not a `sfh --version` bump - is the
/// signal that it may be stale.
pub const SCHEMA_VERSION: u32 = 1;

/// What a caller should or could do next. Always a runnable argv, never prose:
/// an agent can execute it without parsing an instruction.
pub fn next_action(kind: &str, argv: Vec<String>) -> serde_json::Value {
    json!({ "kind": kind, "argv": argv })
}

/// The common header every machine answer carries, merged with per-command
/// fields.
///
/// `body` is merged into the top level rather than nested, so a caller reads
/// `.run_dir` and `.tools` from the same object. Header keys always win: a
/// command cannot accidentally redefine `ok` or `exit_code`.
pub fn envelope(
    command: &str,
    ok: bool,
    exit_code: i32,
    body: serde_json::Value,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let serde_json::Value::Object(map) = body {
        out.extend(map);
    }
    let header = [
        ("schema_version", json!(SCHEMA_VERSION)),
        ("command", json!(command)),
        ("ok", json!(ok)),
        ("exit_code", json!(exit_code)),
        ("sfh_version", json!(env!("CARGO_PKG_VERSION"))),
    ];
    for (k, v) in header {
        out.insert(k.to_string(), v);
    }
    for key in ["error", "warnings", "next_actions"] {
        out.entry(key.to_string()).or_insert(match key {
            "error" => serde_json::Value::Null,
            _ => json!([]),
        });
    }
    serde_json::Value::Object(out)
}

/// A failure envelope. Used even for a usage error, so a machine caller never
/// has to parse prose to find out that sfh refused.
pub fn error_envelope(
    command: &str,
    code: ErrorCode,
    message: &str,
    exit_code: i32,
    body: serde_json::Value,
) -> serde_json::Value {
    let mut v = envelope(command, false, exit_code, body);
    if let Some(map) = v.as_object_mut() {
        map.insert(
            "error".into(),
            json!({ "code": code.as_str(), "message": message }),
        );
    }
    v
}

/// Print an envelope on stdout as the sole content of stdout.
pub fn emit(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_is_present_and_cannot_be_overwritten_by_a_command() {
        // A command that tries to set its own `ok`/`exit_code` must not be able
        // to lie to a caller about whether sfh succeeded.
        let v = envelope(
            "run",
            true,
            0,
            json!({"ok": false, "exit_code": 99, "run_dir": "/tmp/x"}),
        );
        assert_eq!(v["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(v["command"], json!("run"));
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["exit_code"], json!(0));
        assert_eq!(v["run_dir"], json!("/tmp/x"));
        assert_eq!(v["error"], serde_json::Value::Null);
        assert_eq!(v["warnings"], json!([]));
        assert_eq!(v["next_actions"], json!([]));
    }

    #[test]
    fn error_codes_are_unique_and_stable_looking() {
        use ErrorCode::*;
        let all = [
            Usage,
            FlowInvalid,
            StepFailed,
            ProtocolInvalid,
            TerminalMissing,
            SessionUnverified,
            ExecutionClosureChanged,
            WorkspaceMissing,
            WorkspaceDrift,
            WorkspaceBusy,
            RunBusy,
            WorkspaceUnowned,
            ReplayRefused,
            PersistenceFailure,
            CapabilityUnavailable,
            Stuck,
            Interrupted,
        ];
        let mut seen = std::collections::BTreeSet::new();
        for c in all {
            let s = c.as_str();
            assert!(
                s.starts_with("SFH_"),
                "{s} is not in the documented namespace"
            );
            assert!(seen.insert(s), "{s} is used by two codes");
        }
        // The set the spec fixes for this SCHEMA_VERSION. Adding one is fine;
        // changing what an existing one means is not, and this catches a
        // silent rename.
        for expected in [
            "SFH_USAGE",
            "SFH_FLOW_INVALID",
            "SFH_STEP_FAILED",
            "SFH_PROTOCOL_INVALID",
            "SFH_TERMINAL_MISSING",
            "SFH_SESSION_UNVERIFIED",
            "SFH_EXECUTION_CLOSURE_CHANGED",
            "SFH_WORKSPACE_MISSING",
            "SFH_WORKSPACE_DRIFT",
            "SFH_WORKSPACE_BUSY",
            "SFH_RUN_BUSY",
            "SFH_WORKSPACE_UNOWNED",
            "SFH_REPLAY_REFUSED",
            "SFH_PERSISTENCE_FAILURE",
            "SFH_CAPABILITY_UNAVAILABLE",
            "SFH_STUCK",
            "SFH_INTERRUPTED",
        ] {
            assert!(seen.contains(expected), "{expected} disappeared");
        }
    }

    #[test]
    fn a_failure_envelope_carries_a_code_and_a_message() {
        let v = error_envelope("run", ErrorCode::FlowInvalid, "bad flow", 2, json!({}));
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["exit_code"], json!(2));
        assert_eq!(v["error"]["code"], json!("SFH_FLOW_INVALID"));
        assert_eq!(v["error"]["message"], json!("bad flow"));
    }
}
