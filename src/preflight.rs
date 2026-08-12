//! `sfh preflight` - what can be checked before spending anything.
//!
//! `doctor` is the expensive, honest check: it sends a real one-token prompt to
//! each tool and confirms sfh can still parse the answer. That is the only way
//! to catch a protocol that has drifted, and it costs money and time.
//!
//! Preflight is the other half. It makes NO model calls: it resolves the flow,
//! finds the binaries that flow would actually launch, reads their `--version`
//! and `--help`, and reports what sfh knows about each adapter's protocol,
//! session support, cost coverage and policy gaps - plus the workspace it would
//! create, the context it would build, and anything that would refuse the run.
//!
//! The split matters because the two answer different questions:
//!
//! ```text
//! preflight = will this flow even start, and under what policy?   (free)
//! doctor    = does the tool still speak the protocol sfh parses?  (paid)
//! ```
//!
//! "Makes NO model calls" is a claim about MODELS, not about every program a
//! flow can name. A `cmd:` step's program is resolved and never run, because
//! sfh did not write it and cannot know `deploy.sh --help` will not deploy
//! (see `CommandReport`). A preset tool's `bin:` override is the identical
//! danger wearing a trusted tool's name: sfh has verified that claude's,
//! codex's, and every other shipped adapter's OWN launcher is inert on
//! `--help`/`--version`, but `bin:` can point `tool: claude` at any program
//! the flow wants, and preflight has no way to tell "a newer claude" from "a
//! script that deploys". So a non-default `bin:` gets the same treatment as a
//! `cmd:` program - resolved, never run - unless the operator opts in with
//! `--probe-binaries`. See `ProbeState` for how the report says which
//! happened, for both a tool and a `cmd:` program.

use crate::{contain, execute, flow, leaf, preset, state};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One tool the flow would actually launch, as preflight found it.
pub struct ToolReport {
    pub tool: String,
    pub program: String,
    pub resolved_path: Option<String>,
    /// Whether `program` was actually run - see `ProbeState`. A `bin:`
    /// override is resolved but not probed unless `--probe-binaries` was
    /// given (P0-05); the tool's own default launcher is always probed.
    pub probe_state: ProbeState,
    /// `None` either because the CLI printed nothing usable on `--version`
    /// or because `probe_state` is not `Probed` - check that field before
    /// reading a `None` here as "something is wrong with the tool".
    pub version: Option<String>,
    /// Flow-level exact/range declarations applying to this program. Several
    /// steps can share a binary while declaring different compatible ranges.
    pub required_versions: Vec<String>,
    pub info: Option<preset::AdapterInfo>,
    /// Required flags that this binary's `--help` did not mention. Empty
    /// when help could not be read at all (see `help_readable`), or when
    /// `probe_state` is not `Probed` and `--help` was never sent.
    pub missing_flags: Vec<String>,
    pub help_readable: bool,
    /// Access levels this flow asks this tool for.
    pub requested_access: Vec<String>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

/// Whether, and why, preflight ran `<program> --help`/`--version` for one
/// tool/program pair.
///
/// Exists because a bare `None` version is ambiguous: it can mean "sfh ran
/// this and the CLI printed nothing usable" or "sfh never ran this at all",
/// and those are different answers an operator must not confuse - reading
/// the second as the first is exactly how a `bin:` override preflight
/// correctly refused to execute could be misread as one that was checked and
/// came back clean (P0-05).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProbeState {
    /// `<program> --help` and `--version` were actually run, in an isolated
    /// scratch directory. `version`, `help_readable` and `missing_flags`
    /// reflect a real invocation.
    Probed,
    /// The binary exists but preflight did not run it - either it is a
    /// `bin:` override and `--probe-binaries` was not given (the same
    /// reasoning `CommandReport` already applies to a `cmd:` step's
    /// program), or preflight could not create an isolated place to run it
    /// in. `version`, `help_readable` and `missing_flags` are all their
    /// empty defaults: absence of evidence, not evidence of absence.
    ResolvedNotProbed,
    /// Not found on PATH (or at the literal path given), so there was
    /// nothing to run.
    NotFound,
}

impl ProbeState {
    pub fn as_str(self) -> &'static str {
        match self {
            ProbeState::Probed => "probed",
            ProbeState::ResolvedNotProbed => "resolved_not_probed",
            ProbeState::NotFound => "not_found",
        }
    }
}

/// One program a `cmd:` step would launch, as preflight found it.
///
/// Deliberately thinner than a `ToolReport`: preflight RESOLVES a custom command
/// and never runs it. `--help` and `--version` are safe to send to an adapter
/// sfh ships support for; they are not safe to send to an arbitrary program a
/// flow names, because `deploy.sh --help` may well deploy. Resolution answers
/// the question that actually bit - "which binary is this name?" - without
/// executing anything.
///
/// A preset tool's non-default `bin:` override is the same danger wearing a
/// trusted tool's name; see `ProbeState::ResolvedNotProbed` on `ToolReport`
/// for where this exact reasoning was carried across (P0-05).
pub struct CommandReport {
    pub program: String,
    pub resolved_path: Option<String>,
    /// Step ids that launch this program, sorted.
    pub steps: Vec<String>,
    /// True when argv[0] carries a template placeholder, so no path can be
    /// resolved until the run renders it.
    pub templated: bool,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

impl CommandReport {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "program": self.program,
            "resolved_path": self.resolved_path,
            "steps": self.steps,
            "templated": self.templated,
            "blockers": self.blockers,
            "warnings": self.warnings,
        })
    }
}

/// Everything a preflight run concluded.
pub struct Report {
    pub sfh_version: &'static str,
    pub flow_path: Option<String>,
    pub flow_name: Option<String>,
    /// `None` for the flowless adapter survey, otherwise whether the flow
    /// loaded and passed static validation. This keeps a machine caller from
    /// confusing a broken flow with a valid flow whose local tools are absent.
    pub flow_valid: Option<bool>,
    /// Whether `--probe-binaries` was given, so a JSON consumer can tell "no
    /// bin: overrides existed" apart from "overrides existed but this run
    /// was not allowed to execute them" without re-deriving it by scanning
    /// every tool's `probe_state`.
    pub probe_binaries: bool,
    pub tools: Vec<ToolReport>,
    /// Programs launched by `cmd:` steps. Empty for a flowless survey.
    pub commands: Vec<CommandReport>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    /// Static, structural facts about the flow - filled in only when a flow was
    /// given.
    pub flow_facts: Option<serde_json::Value>,
}

impl Report {
    pub fn ok(&self) -> bool {
        self.blockers.is_empty()
            && self.tools.iter().all(|t| t.blockers.is_empty())
            && self.commands.iter().all(|c| c.blockers.is_empty())
    }

    pub fn to_json(&self) -> serde_json::Value {
        let failure_kind = if self.ok() {
            None
        } else if self.flow_valid == Some(false) {
            Some("flow_invalid")
        } else {
            Some("capability_unavailable")
        };
        json!({
            "sfh_version": self.sfh_version,
            "flow": self.flow_path,
            "flow_name": self.flow_name,
            "flow_valid": self.flow_valid,
            "ok": self.ok(),
            "failure_kind": failure_kind,
            "probe_binaries": self.probe_binaries,
            "tools": self.tools.iter().map(ToolReport::to_json).collect::<Vec<_>>(),
            "commands": self.commands.iter().map(CommandReport::to_json).collect::<Vec<_>>(),
            "flow_facts": self.flow_facts,
            "blockers": self.blockers,
            "warnings": self.warnings,
        })
    }
}

impl ToolReport {
    pub fn to_json(&self) -> serde_json::Value {
        let info = self.info.as_ref();
        json!({
            "tool": self.tool,
            "program": self.program,
            // What sfh would launch with no `bin:` override, so a report can be
            // read without knowing the adapter defaults by heart.
            "default_program": info.map(|i| i.default_program.clone()),
            "adapter": info.map(|i| i.tool),
            "resolved_path": self.resolved_path,
            // Distinguishes "ran, found nothing" from "never ran" - see
            // `ProbeState`. A machine caller must check this before reading
            // `version: null` as a problem with the tool.
            "probe_state": self.probe_state.as_str(),
            "version": self.version,
            "required_versions": self.required_versions,
            // Deliberately null: sfh does not pin a floor it has not verified
            // against the CLI's own documentation and a live probe.
            "minimum_version": info.and_then(|i| i.minimum_version),
            "last_verified": info.map(|i| i.last_verified),
            "protocol": info.map(|i| i.protocol),
            "supports_resume": info.map(|i| i.supports_resume),
            "supports_fork": info.map(|i| i.supports_fork),
            "cost_coverage": info.map(|i| i.cost_coverage.as_str()),
            // False means a certified-successful turn survives a non-zero exit
            // by default; every other adapter needs `exit_conflict:` to say so.
            "exit_code_trustworthy": info.map(|i| i.exit_code_trustworthy),
            "required_flags": info.map(|i| i.required_flags),
            "help_readable": self.help_readable,
            "missing_flags": self.missing_flags,
            "requested_access": self.requested_access,
            "access_enforcement": info.map(|i| {
                json!({
                    "read": i.enforcement(preset::Access::Read).as_str(),
                    "write": i.enforcement(preset::Access::Write).as_str(),
                    "full": i.enforcement(preset::Access::Full).as_str(),
                })
            }),
            "known_gaps": info.map(|i| i.known_gaps),
            "blockers": self.blockers,
            "warnings": self.warnings,
        })
    }
}

fn version_requirement_blockers(
    program: &str,
    probe_state: ProbeState,
    observed: Option<&str>,
    required_versions: &[String],
) -> Vec<String> {
    if required_versions.is_empty() {
        return Vec::new();
    }
    match (probe_state, observed) {
        (ProbeState::Probed, Some(observed)) => required_versions
            .iter()
            .filter_map(|requirement| match crate::version::satisfies(requirement, observed) {
                Ok(true) => None,
                Ok(false) => Some(format!(
                    "'{program}' reports {observed:?}, which does not satisfy require_version: {requirement}"
                )),
                Err(error) => Some(format!(
                    "'{program}' cannot be checked against require_version: {requirement}: {error}"
                )),
            })
            .collect(),
        (ProbeState::Probed, None) => vec![format!(
            "'{program}' produced no usable --version output, so require_version cannot be verified"
        )],
        (ProbeState::ResolvedNotProbed, _) => vec![format!(
            "require_version cannot be verified without running '{program} --version'; pass --probe-binaries to authorize this bin: override probe"
        )],
        (ProbeState::NotFound, _) => Vec::new(),
    }
}

/// Read a CLI's own `--help`. Bounded and never fatal: a tool that has no
/// `--help`, prints it to stderr, or wants to talk to a terminal is reported as
/// "help unreadable" rather than as a missing flag, because "sfh could not
/// check" and "the flag is gone" are different answers and only one of them
/// should stop a run.
///
/// Runs in `cwd`, never sfh's own working directory: every one of these CLIs
/// is documented (see `doctor::run_probe`) to read project instruction files
/// out of its working directory on a real run, and nothing here has verified
/// `--help` is an exception. `tool` looks up any probe-only hardening
/// (`preset::probe_hardening`, P3-02) - the table itself lives there, next to
/// the rest of each adapter's command-line facts, not here.
fn read_help(program: &str, tool: &str, cwd: &Path) -> Option<String> {
    let hardening = preset::probe_hardening(tool);
    let mut argv = vec![program.to_string(), "--help".to_string()];
    argv.extend(hardening.extra_args.iter().map(|s| s.to_string()));
    let env_set: Vec<(String, String)> = hardening
        .env_set
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let out = execute::run_cmd(
        &execute::Invocation::Argv(argv),
        None,
        Some(cwd),
        Some(std::time::Duration::from_secs(15)),
        &[],
        &env_set,
        execute::Observe::default(),
    )
    .ok()?;
    if out.timed_out {
        return None;
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (!text.trim().is_empty()).then_some(text)
}

/// `<program> --version`, isolated the same way `read_help` is and for the
/// same reason (see there). Bounded like `execute::probe_version` already
/// is: a CLI that hangs on `--version` (an auth prompt, a stuck update check)
/// must not stop preflight from finishing - this is metadata, not a
/// dependency.
///
/// Mirrors `execute::probe_version`'s own parsing exactly; duplicated rather
/// than shared because that function always runs in the CALLING process's
/// cwd, which for `sfh preflight` is the operator's own project - the one
/// place this probe must not run.
fn probe_version_isolated(program: &str, tool: &str, cwd: &Path) -> Option<String> {
    let hardening = preset::probe_hardening(tool);
    let mut argv = vec![program.to_string(), "--version".to_string()];
    argv.extend(hardening.extra_args.iter().map(|s| s.to_string()));
    let env_set: Vec<(String, String)> = hardening
        .env_set
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let out = execute::run_cmd(
        &execute::Invocation::Argv(argv),
        None,
        Some(cwd),
        Some(std::time::Duration::from_secs(15)),
        &[],
        &env_set,
        execute::Observe::default(),
    )
    .ok()?;
    if out.timed_out || out.interrupted || out.exit_code != 0 {
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

/// Inspect one tool/program pair without calling a model.
///
/// `required` distinguishes the two questions preflight answers. A flow that
/// launches this tool cannot start without it, so a missing binary is a
/// blocker. A flowless survey is just asking what is installed, and "codex is
/// not on this machine" is an answer, not a failure.
///
/// `probe_binaries` and `probe_dir` are the P0-05 fix. `program` is only ever
/// RUN when it is the tool's own default launcher (`program ==
/// preset::default_program(tool)`, resolved on PATH) - sfh ships that
/// adapter and has verified its `--help`/`--version` are inert. Anything else
/// is a `bin:` override the FLOW chose, and gets the identical treatment a
/// `cmd:` step's program already gets (see `CommandReport`'s doc comment):
/// resolved, never run, unless the operator passes `--probe-binaries`.
/// `probe_dir` is where an allowed probe actually runs - never sfh's own
/// working directory (see `read_help`) - and is `None` only when preflight
/// could not create one, in which case nothing is run rather than something
/// being run unisolated.
fn probe(
    tool: &str,
    program: &str,
    requested_access: Vec<String>,
    required: bool,
    probe_binaries: bool,
    probe_dir: Option<&Path>,
    required_versions: Vec<String>,
) -> ToolReport {
    let info = preset::adapter_info(tool);
    let resolved_path = execute::which(program);
    let is_default = program == preset::default_program(tool);
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    let mut missing_flags = Vec::new();
    let mut help_readable = false;
    let mut version = None;
    let mut probe_state = ProbeState::NotFound;
    if resolved_path.is_none() {
        let msg = format!("'{program}' is not on PATH (install it, or set bin: to its full path)");
        if required {
            blockers.push(format!("{msg} - this flow cannot start without it"));
        } else {
            warnings.push(msg);
        }
    } else if !is_default && !probe_binaries {
        probe_state = ProbeState::ResolvedNotProbed;
        warnings.push(format!(
            "'{program}' is this flow's bin: override for {tool}, not {tool}'s own launcher, so preflight only resolved it and did not run it. sfh has verified {tool}'s shipped launcher is inert on --help/--version; it has no idea what '{program}' is, and a cmd: step's program gets the identical treatment for the identical reason (\"deploy.sh --help\" may well deploy). No version was recorded and flags were not checked - do not read that silence as \"the tool is fine\". Pass --probe-binaries to actually run '{program} --version' and '--help'."
        ));
    } else {
        match probe_dir {
            None => {
                probe_state = ProbeState::ResolvedNotProbed;
                warnings.push(format!(
                    "'{program}' was not probed: no isolated scratch directory was available for this preflight run (see the top-level warning for why)"
                ));
            }
            Some(cwd) => {
                probe_state = ProbeState::Probed;
                version = probe_version_isolated(program, tool, cwd);
                if version.is_none() {
                    warnings.push(format!(
                        "'{program} --version' produced nothing usable, so sfh cannot record which build this run used"
                    ));
                }
                match read_help(program, tool, cwd) {
                    Some(help) => {
                        help_readable = true;
                        if let Some(i) = &info {
                            for flag in i.required_flags {
                                if !help.contains(flag) {
                                    missing_flags.push((*flag).to_string());
                                }
                            }
                        }
                        if !missing_flags.is_empty() {
                            blockers.push(format!(
                                "'{program} --help' does not mention {} - the installed CLI does not look like the one this adapter was built against (last verified {}). Run `sfh doctor` to see what it actually returns.",
                                missing_flags.join(", "),
                                info.as_ref().map(|i| i.last_verified).unwrap_or("unknown")
                            ));
                        }
                    }
                    None => warnings.push(format!(
                        "'{program} --help' could not be read, so sfh could not check that this adapter's flags still exist"
                    )),
                }
            }
        }
    }
    if let Some(i) = &info {
        // A floor sfh never verified is not reported as if it had been.
        if i.minimum_version.is_none() {
            warnings.push(format!(
                "sfh pins no minimum version for {tool}: the adapter was last verified on {} and reports the installed version rather than a floor it has not checked",
                i.last_verified
            ));
        }
        for level in &requested_access {
            let access = preset::Access::parse(Some(level)).unwrap_or(preset::Access::Read);
            match i.enforcement(access) {
                preset::Enforcement::Unsupported => blockers.push(format!(
                    "{tool} cannot enforce access: {level} headlessly at all"
                )),
                preset::Enforcement::BestEffort => warnings.push(format!(
                    "access: {level} on {tool} is best-effort: the tool's own config can widen it, and sfh's access: is never an OS sandbox"
                )),
                preset::Enforcement::Sandboxed | preset::Enforcement::Enforced => {}
            }
        }
        if i.cost_coverage != preset::Coverage::Cost {
            warnings.push(format!(
                "{tool} reports {} rather than a cost, so defaults.max_cost_usd cannot bound what this step spends",
                i.cost_coverage.as_str()
            ));
        }
    }
    blockers.extend(version_requirement_blockers(
        program,
        probe_state,
        version.as_deref(),
        &required_versions,
    ));
    ToolReport {
        tool: tool.to_string(),
        program: program.to_string(),
        resolved_path,
        probe_state,
        version,
        required_versions,
        info,
        missing_flags,
        help_readable,
        requested_access,
        blockers,
        warnings,
    }
}

/// Resolve one `cmd:` program. Runs nothing.
fn probe_command(program: &str, steps: BTreeSet<String>) -> CommandReport {
    let steps: Vec<String> = steps.into_iter().collect();
    let where_ = steps.join(", ");
    let mut blockers = Vec::new();
    let warnings = Vec::new();
    if program.contains("{{") {
        return CommandReport {
            program: program.to_string(),
            resolved_path: None,
            steps,
            templated: true,
            blockers,
            warnings: vec![format!(
                "the program in {where_} is built by a template, so preflight cannot say which binary it will be"
            )],
        };
    }
    let has_separator = program.contains('/') || program.contains('\\');
    // A RELATIVE path is resolved by the OS against the process's working
    // directory, and at run time that is the step's `cwd:` or the managed
    // workspace - neither of which exists yet, and neither of which is
    // preflight's own cwd. `./scripts/verify.sh` is perfectly correct for a
    // flow whose steps run in a worktree, so checking it here would block a
    // working flow for a fact preflight is in no position to know.
    if has_separator && !Path::new(program).is_absolute() {
        return CommandReport {
            program: program.to_string(),
            resolved_path: None,
            steps,
            templated: false,
            blockers,
            warnings: vec![format!(
                "'{program}' ({where_}) is relative, so it resolves against the step's working directory at run time - preflight cannot check it from here"
            )],
        };
    }
    let resolved_path = execute::which(program);
    match &resolved_path {
        None if has_separator => blockers.push(format!(
            "'{program}' ({where_}) does not exist - this flow cannot start without it"
        )),
        None => blockers.push(format!(
            "'{program}' ({where_}) is not on PATH - this flow cannot start without it"
        )),
        Some(path) => {
            // Only when PATH made the choice, and only for a name that asked
            // for a shell. Writing `wsl` is a deliberate statement about which
            // OS should run the command, and writing a full path is another;
            // sfh second-guesses neither. Writing `bash` is not a statement
            // about WSL at all - it is a request for a shell that PATH quietly
            // answered with a different operating system.
            let asked_for_a_shell = matches!(program, "bash" | "sh");
            if !has_separator && asked_for_a_shell && execute::is_wsl_launcher(path) {
                blockers.push(format!(
                    "'{program}' ({where_}) resolves to {path}, which starts WSL - a different \
                     operating system. It cannot read this checkout's Windows paths, and a git \
                     worktree's .git gitfile points somewhere that does not exist inside it, so \
                     these commands fail in seconds for a reason unrelated to the code. Name the \
                     shell you mean: \"C:\\\\Program Files\\\\Git\\\\bin\\\\bash.exe\" for Git for \
                     Windows, or the full path to {path} if WSL really is the target."
                ));
            }
        }
    }
    CommandReport {
        program: program.to_string(),
        resolved_path,
        steps,
        templated: false,
        blockers,
        warnings,
    }
}

/// Preflight with no flow: report every preset's local availability.
pub fn all_adapters(probe_dir: Option<&Path>, probe_binaries: bool) -> Report {
    let tools = flow::TOOLS
        .iter()
        .map(|t| {
            probe(
                t,
                &preset::default_program(t),
                Vec::new(),
                false,
                probe_binaries,
                probe_dir,
                Vec::new(),
            )
        })
        .collect();
    Report {
        sfh_version: env!("CARGO_PKG_VERSION"),
        flow_path: None,
        flow_name: None,
        flow_valid: None,
        probe_binaries,
        tools,
        commands: Vec::new(),
        blockers: Vec::new(),
        warnings: Vec::new(),
        flow_facts: None,
    }
}

/// Preflight a flow: only the tool/bin variants this flow would actually
/// launch. An unused profile's binary is never touched - the same rule the
/// `doctor` path already follows, and the reason a hostile unused profile
/// cannot get itself executed by a check.
pub fn for_flow(
    path: &Path,
    root: &state::StateRoot,
    overlays: &[std::path::PathBuf],
    probe_binaries: bool,
    probe_dir: Option<&Path>,
) -> Report {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    let flow = match flow::load_with_overlays(path, overlays) {
        Ok(f) => f,
        Err(e) => {
            return Report {
                sfh_version: env!("CARGO_PKG_VERSION"),
                flow_path: Some(path.display().to_string()),
                flow_name: None,
                flow_valid: Some(false),
                probe_binaries,
                tools: Vec::new(),
                commands: Vec::new(),
                blockers: vec![e],
                warnings: Vec::new(),
                flow_facts: None,
            }
        }
    };
    let resolved = flow.resolved_tools();
    // Group the access levels a flow asks of each (tool, bin) pair, so one
    // probe covers every step that shares a binary.
    let mut by_program: std::collections::BTreeMap<
        (String, String),
        (BTreeSet<String>, BTreeSet<String>),
    > = Default::default();
    for r in &resolved {
        let program = r
            .bin
            .clone()
            .unwrap_or_else(|| preset::default_program(&r.tool));
        let (access, requirements) = by_program.entry((r.tool.clone(), program)).or_default();
        access.extend(r.access.iter().cloned());
        if let Some(requirement) = &r.require_version {
            requirements.insert(requirement.clone());
        }
    }
    let tools = by_program
        .into_iter()
        .map(|((tool, program), (access, requirements))| {
            probe(
                &tool,
                &program,
                access.into_iter().collect(),
                true,
                probe_binaries,
                probe_dir,
                requirements.into_iter().collect(),
            )
        })
        .collect::<Vec<_>>();
    let commands = flow
        .resolved_commands()
        .into_iter()
        .map(|(program, steps)| probe_command(&program, steps))
        .collect::<Vec<_>>();

    // Structural facts that need no process at all.
    let workspace = flow.workspace_plan();
    match &workspace {
        Ok(plan) => {
            if plan.needs_state_root && !root.is_explicit() {
                if let Err(e) = root.managed_root() {
                    blockers.push(e);
                }
            }
            warnings.extend(plan.warnings.iter().cloned());
        }
        Err(e) => blockers.push(e.clone()),
    }
    let contexts = match flow.context_plan(path) {
        Ok(c) => Some(c),
        Err(e) => {
            blockers.push(e);
            None
        }
    };
    let facts = json!({
        "steps": flow.steps.len(),
        "static_max_leaves": flow.static_max_leaves(),
        "workspace": workspace.as_ref().ok().map(|p| p.to_json()),
        "contexts": contexts.as_ref().map(|c| c.to_json()),
        "replay": flow.replay_summary(),
        "unsafe_overrides": flow.unsafe_overrides(),
        "profile_overlays": overlays.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
    });
    Report {
        sfh_version: env!("CARGO_PKG_VERSION"),
        flow_path: Some(path.display().to_string()),
        flow_name: flow.name.clone(),
        flow_valid: Some(true),
        probe_binaries,
        tools,
        commands,
        blockers,
        warnings,
        flow_facts: Some(facts),
    }
}

/// Human-readable preflight. Progress and warnings go to stderr so that even
/// here stdout stays the report.
pub fn print_human(r: &Report) {
    println!("sfh {} preflight", r.sfh_version);
    if let Some(f) = &r.flow_path {
        println!("flow: {f}");
    }
    println!();
    for t in &r.tools {
        let where_ = t
            .resolved_path
            .clone()
            .unwrap_or_else(|| "NOT FOUND on PATH".to_string());
        println!("[{}] {} -> {}", t.tool, t.program, where_);
        match (t.probe_state, &t.version) {
            (ProbeState::Probed, Some(v)) => println!("     version: {v}"),
            (ProbeState::Probed, None) => {
                println!("     version: probed, but produced nothing usable")
            }
            (ProbeState::ResolvedNotProbed, _) => {
                println!("     version: not probed (see warning below)")
            }
            (ProbeState::NotFound, _) => {}
        }
        if !t.required_versions.is_empty() {
            println!("     required: {}", t.required_versions.join(", "));
        }
        if let Some(i) = &t.info {
            println!(
                "     protocol: {}   resume: {}   fork: {}   cost: {}",
                i.protocol,
                yn(i.supports_resume),
                yn(i.supports_fork),
                i.cost_coverage.as_str()
            );
            println!(
                "     adapter last verified {}, minimum version: {}",
                i.last_verified,
                i.minimum_version.unwrap_or("unknown (not pinned)")
            );
            if !i.exit_code_trustworthy {
                println!(
                    "     exit codes: documented as unreliable, so a certified terminal record wins"
                );
            }
            if !t.requested_access.is_empty() {
                let levels = t
                    .requested_access
                    .iter()
                    .map(|a| {
                        let acc = preset::Access::parse(Some(a)).unwrap_or(preset::Access::Read);
                        format!("{a}={}", i.enforcement(acc).as_str())
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("     access requested: {levels}");
            }
            for gap in i.known_gaps {
                println!("     gap: {gap}");
            }
        }
        if t.probe_state == ProbeState::Probed && !t.help_readable {
            println!("     help: unreadable (flags not checked)");
        }
        for b in &t.blockers {
            println!("     BLOCKER: {b}");
        }
        for w in &t.warnings {
            println!("     warning: {w}");
        }
        println!();
    }
    for c in &r.commands {
        let where_ = if c.templated {
            "built by a template, unresolvable before the run".to_string()
        } else {
            c.resolved_path
                .clone()
                .unwrap_or_else(|| "NOT FOUND on PATH".to_string())
        };
        println!("[cmd] {} -> {}", c.program, where_);
        println!("      steps: {}", c.steps.join(", "));
        println!("      resolved only; preflight never runs a custom command");
        for b in &c.blockers {
            println!("      BLOCKER: {b}");
        }
        for w in &c.warnings {
            println!("      warning: {w}");
        }
        println!();
    }
    if let Some(f) = &r.flow_facts {
        println!("{}", serde_json::to_string_pretty(f).unwrap_or_default());
        println!();
    }
    for b in &r.blockers {
        println!("BLOCKER: {b}");
    }
    for w in &r.warnings {
        println!("warning: {w}");
    }
    if r.tools
        .iter()
        .any(|t| t.probe_state == ProbeState::ResolvedNotProbed)
    {
        println!(
            "note: at least one binary above was resolved but not run; pass --probe-binaries to actually check it."
        );
    }
    println!(
        "{}",
        if r.ok() {
            "preflight: no blockers. This makes no model calls - run `sfh doctor` to check the protocols themselves."
        } else {
            "preflight: BLOCKED. Fix the blockers above before running this flow."
        }
    );
}

fn yn(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

/// A fresh, private scratch directory to probe binaries in - never the
/// operator's own working directory. Mirrors `doctor`'s isolation (see
/// `doctor::run_probe`) for the identical reason: some launchers read
/// project instruction files or write a cache on ANY invocation, and a free,
/// offline check must not do that to the caller's own repository. Unlike
/// doctor's, nothing here needs to outlive the command: `run` removes it once
/// every probe in this preflight is done.
fn probe_scratch_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("sfh-preflight-{}", leaf::gen_uuid()));
    contain::mkdir_private(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    dir.canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", dir.display()))
}

/// Run the same hardened, isolated version probe used by preflight, for the
/// execution gate. Unlike a standalone preflight, a real run is already
/// authorized to launch this exact program; only the harmless `--version`
/// invocation happens here, before any flow step or model process.
pub fn probe_version_for_run(tool: &str, program: &str) -> Result<Option<String>, String> {
    let dir = probe_scratch_dir()?;
    let version = probe_version_isolated(program, tool, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(version)
}

/// `sfh preflight [flow.yaml]`.
pub fn run(
    flow_path: Option<&Path>,
    root: &state::StateRoot,
    overlays: &[std::path::PathBuf],
    as_json: bool,
    probe_binaries: bool,
) -> i32 {
    let probe_dir = probe_scratch_dir();
    let mut report = match flow_path {
        Some(p) => for_flow(p, root, overlays, probe_binaries, probe_dir.as_deref().ok()),
        None => all_adapters(probe_dir.as_deref().ok(), probe_binaries),
    };
    if let Err(e) = &probe_dir {
        report.warnings.push(format!(
            "no tool binary below was probed: sfh could not create an isolated scratch directory to run --help/--version in ({e}); running them in sfh's own working directory instead is exactly what preflight must not do"
        ));
    }
    if let Ok(dir) = &probe_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    if as_json {
        let code = if report.ok() { 0 } else { 2 };
        if report.ok() {
            crate::machine::emit(&crate::machine::envelope(
                "preflight",
                true,
                0,
                report.to_json(),
            ));
        } else {
            let first = report
                .blockers
                .first()
                .cloned()
                .or_else(|| {
                    report
                        .tools
                        .iter()
                        .flat_map(|t| t.blockers.first().cloned())
                        .next()
                })
                .or_else(|| {
                    report
                        .commands
                        .iter()
                        .flat_map(|c| c.blockers.first().cloned())
                        .next()
                })
                .unwrap_or_else(|| "preflight found blockers".to_string());
            let error_code = if report.flow_valid == Some(false) {
                crate::machine::ErrorCode::FlowInvalid
            } else {
                crate::machine::ErrorCode::CapabilityUnavailable
            };
            crate::machine::emit(&crate::machine::error_envelope(
                "preflight",
                error_code,
                &first,
                code,
                report.to_json(),
            ));
        }
    } else {
        print_human(&report);
    }
    // A flowless preflight is a survey, not a gate: a missing tool the user
    // does not intend to run is not an error.
    if flow_path.is_some() && !report.ok() {
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_compares_required_and_observed_versions_fail_closed() {
        let required = vec![">=1.2, <2.0".to_string()];
        assert!(version_requirement_blockers(
            "tool",
            ProbeState::Probed,
            Some("tool 1.5.0"),
            &required,
        )
        .is_empty());
        assert!(version_requirement_blockers(
            "tool",
            ProbeState::Probed,
            Some("tool 2.0.0"),
            &required,
        )[0]
        .contains("does not satisfy"));
        assert!(
            version_requirement_blockers("tool", ProbeState::Probed, None, &required,)[0]
                .contains("cannot be verified")
        );
        assert!(version_requirement_blockers(
            "custom-tool",
            ProbeState::ResolvedNotProbed,
            None,
            &required,
        )[0]
        .contains("--probe-binaries"));
    }

    #[test]
    fn a_flowless_preflight_covers_every_preset_and_calls_no_model() {
        let r = all_adapters(None, false);
        assert_eq!(r.tools.len(), flow::TOOLS.len());
        for t in &r.tools {
            assert!(
                preset::adapter_info(&t.tool).is_some(),
                "{} has no adapter metadata",
                t.tool
            );
        }
        // A survey never gates.
        assert!(r.blockers.is_empty());
    }

    /// The rule this test has always been about is "preflight must not report
    /// a version floor sfh invented". It used to spell that as "every
    /// `minimum_version` is `None`", which was accurate only while no adapter
    /// had a documented floor to point at.
    ///
    /// P1-06 gave agy one: its changelog dates the `--output-format json`
    /// envelope that preset's whole parse path depends on to 1.1.8, so a
    /// build below that accepts the flags and then cannot answer. The
    /// underlying rule is unchanged - a pinned floor has to be something
    /// agy's authors put in writing - so the assertion moves to that rule
    /// rather than to whichever adapters happen to pin one today.
    /// `preset::tests::only_agy_pins_a_minimum_version_and_it_names_its_source`
    /// guards the same invariant next to the data; this one guards what
    /// preflight goes on to print.
    #[test]
    fn preflight_only_reports_a_version_floor_that_is_documented() {
        for tool in flow::TOOLS {
            let i = preset::adapter_info(tool).expect("every preset has metadata");
            match tool {
                "agy" => assert_eq!(
                    i.minimum_version,
                    Some("1.1.8"),
                    "agy's structured print output needs 1.1.8, and preflight reports that floor"
                ),
                _ => assert_eq!(
                    i.minimum_version, None,
                    "{tool} pins a floor that was not confirmed against the CLI's docs and a live probe"
                ),
            }
            // Not incidental to this test: preflight's `--help` drift check
            // can only look for flags that are listed, so an empty list makes
            // the check silently pass on any binary at all.
            assert!(
                !i.required_flags.is_empty(),
                "{tool} lists no required flags"
            );
        }
    }

    #[test]
    fn agy_is_the_only_adapter_whose_exit_code_is_not_believed() {
        // v1.2.0 hardcoded this as `matches!(p.parse, AgyJson)` in the
        // execution layer. Moving it into adapter metadata must not move the
        // line: every other preset still fails on a non-zero exit unless the
        // flow declares otherwise.
        for tool in flow::TOOLS {
            let i = preset::adapter_info(tool).unwrap();
            assert_eq!(
                i.exit_code_trustworthy,
                *tool != *"agy",
                "{tool}'s exit-code trust changed"
            );
            assert_eq!(i.exit_code_trustworthy, preset::exit_code_trustworthy(tool));
        }
        // A custom `cmd:` has no adapter and no protocol; it keeps the strict
        // reading, which is also the only safe answer for an unknown name.
        assert!(preset::exit_code_trustworthy(""));
        assert!(preset::exit_code_trustworthy(
            "some-tool-sfh-never-heard-of"
        ));
    }

    #[test]
    fn a_missing_cmd_program_blocks_the_flow_and_names_the_steps_that_want_it() {
        let c = probe_command(
            "definitely-not-a-real-binary-9d3f",
            ["verify".to_string(), "build".to_string()]
                .into_iter()
                .collect(),
        );
        assert!(c.resolved_path.is_none());
        assert_eq!(c.steps, vec!["build".to_string(), "verify".to_string()]);
        assert!(
            c.blockers
                .iter()
                .any(|b| b.contains("not on PATH") && b.contains("build, verify")),
            "{:?}",
            c.blockers
        );
    }

    #[test]
    fn a_relative_cmd_program_is_not_judged_against_preflights_own_directory() {
        // The step will run in its `cwd:` or in the managed workspace, neither
        // of which exists yet. Blocking here would refuse a correct flow for a
        // fact preflight is in no position to know.
        for p in ["./scripts/verify.sh", "..\\tools\\build.cmd"] {
            let c = probe_command(p, ["verify".to_string()].into_iter().collect());
            assert!(c.blockers.is_empty(), "{p}: {:?}", c.blockers);
            assert_eq!(c.warnings.len(), 1, "{p}: {:?}", c.warnings);
            assert!(c.warnings[0].contains("relative"), "{:?}", c.warnings);
        }
        // An absolute path CAN be checked from here, so a missing one blocks.
        let missing = if cfg!(windows) {
            r"C:\definitely\not\here-9d3f.exe"
        } else {
            "/definitely/not/here-9d3f"
        };
        let c = probe_command(missing, ["verify".to_string()].into_iter().collect());
        assert!(
            c.blockers.iter().any(|b| b.contains("does not exist")),
            "{:?}",
            c.blockers
        );
    }

    #[test]
    fn only_a_bare_shell_name_is_refused_for_landing_on_the_wsl_launcher() {
        // Asking for `wsl` is a deliberate statement about which OS should run
        // the command. Asking for `bash` is not - it is a request for a shell
        // that PATH quietly answered with a different operating system.
        assert!(execute::is_wsl_launcher(r"C:\Windows\System32\wsl.exe"));
        let c = probe_command("wsl", ["verify".to_string()].into_iter().collect());
        assert!(
            !c.blockers.iter().any(|b| b.contains("starts WSL")),
            "an explicit wsl invocation must not be second-guessed: {:?}",
            c.blockers
        );
    }

    #[test]
    fn a_templated_program_is_reported_as_unresolvable_not_guessed_at() {
        let c = probe_command(
            "{{vars.shell}}",
            ["verify".to_string()].into_iter().collect(),
        );
        assert!(c.templated);
        assert!(c.resolved_path.is_none());
        // Unknowable before the run is a warning, not a blocker: sfh has no
        // grounds to refuse something it simply cannot see yet.
        assert!(c.blockers.is_empty(), "{:?}", c.blockers);
        assert_eq!(c.warnings.len(), 1);
    }

    #[test]
    fn a_report_is_not_ok_while_any_command_is_blocked() {
        let r = Report {
            sfh_version: "test",
            flow_path: None,
            flow_name: None,
            flow_valid: None,
            probe_binaries: false,
            tools: Vec::new(),
            commands: vec![probe_command(
                "definitely-not-a-real-binary-9d3f",
                ["verify".to_string()].into_iter().collect(),
            )],
            blockers: Vec::new(),
            warnings: Vec::new(),
            flow_facts: None,
        };
        assert!(
            !r.ok(),
            "a cmd: step that cannot start must fail preflight like any other"
        );
        assert!(r.to_json()["commands"][0]["program"].is_string());
        assert_eq!(r.to_json()["failure_kind"], "capability_unavailable");
    }

    #[test]
    fn a_static_flow_error_is_distinct_from_a_missing_capability() {
        let r = Report {
            sfh_version: "test",
            flow_path: Some("broken.yaml".to_string()),
            flow_name: None,
            flow_valid: Some(false),
            probe_binaries: false,
            tools: Vec::new(),
            commands: Vec::new(),
            blockers: vec!["flow is invalid".to_string()],
            warnings: Vec::new(),
            flow_facts: None,
        };
        let json = r.to_json();
        assert_eq!(json["flow_valid"], false);
        assert_eq!(json["failure_kind"], "flow_invalid");
    }

    #[test]
    fn cursor_reports_write_as_unsupported_rather_than_silently_promoting_it() {
        let i = preset::adapter_info("cursor").unwrap();
        assert_eq!(
            i.enforcement(preset::Access::Write),
            preset::Enforcement::Unsupported
        );
        let r = probe(
            "cursor",
            "definitely-not-a-real-binary-9d3f",
            vec!["write".into()],
            true,
            false,
            None,
            Vec::new(),
        );
        assert!(
            r.blockers.iter().any(|b| b.contains("access: write")),
            "an unsupported level must be a blocker: {:?}",
            r.blockers
        );
    }

    // --- P0-05: a `bin:` override is resolved, never executed, unless the
    // operator opts in with `--probe-binaries`. The tool's own default
    // launcher is unaffected and keeps being probed unconditionally. -------

    /// A scratch dir for these tests, distinct from the one `run` creates for
    /// a real preflight: these drive `probe` directly and need to inspect
    /// what landed inside it.
    ///
    /// Gated the same way its callers are. Every test below spawns a real
    /// shell script to prove a binary did or did not run, which is a Unix
    /// fixture, so on Windows this helper has no callers at all and
    /// `-D warnings` fails the build on dead code rather than skipping a
    /// test.
    #[cfg(unix)]
    fn test_scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sfh-preflight-test-{label}-{}",
            contain::random_nonce()
        ));
        std::fs::create_dir_all(&dir).expect("create test scratch dir");
        dir
    }

    #[cfg(unix)]
    fn write_executable_script(path: &Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, contents).expect("write test fixture script");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod test fixture script");
    }

    #[test]
    #[cfg(unix)]
    fn a_bin_override_that_would_leave_a_marker_is_not_executed_by_a_default_preflight() {
        let dir = test_scratch_dir("no-probe");
        let marker = dir.join("ran.marker");
        let script = dir.join("fake-adapter.sh");
        write_executable_script(
            &script,
            &format!(
                "#!/bin/sh\ntouch \"{}\"\necho fake-version 1.0.0\n",
                marker.display()
            ),
        );
        let program = script.to_string_lossy().into_owned();

        // tool: claude, bin: <script> - a non-default override, probed by
        // neither the caller nor a required binary check.
        let r = probe(
            "claude",
            &program,
            Vec::new(),
            false,
            false,
            Some(&dir),
            Vec::new(),
        );

        assert!(
            !marker.exists(),
            "a default `sfh preflight` must not execute a bin: override"
        );
        assert_eq!(r.probe_state, ProbeState::ResolvedNotProbed);
        assert!(r.version.is_none());
        assert!(
            r.warnings.iter().any(|w| w.contains("--probe-binaries")),
            "the report must say how to actually check it: {:?}",
            r.warnings
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn probe_binaries_makes_the_same_override_actually_run_and_still_checks_its_flags() {
        // Omit exactly one of claude's real required flags from the fake
        // --help text (computed from live adapter metadata so this stays
        // correct if that list changes), so a genuine flag-drift blocker
        // must appear - proving the check still runs once something IS
        // probed, not just that the binary was invoked.
        let required = preset::adapter_info("claude").unwrap().required_flags;
        assert!(
            required.len() >= 2,
            "need at least two required flags to omit just one and keep the rest"
        );
        let (last, kept) = required.split_last().expect("claude has required flags");
        let omitted: &str = last;

        let dir = test_scratch_dir("probe");
        let marker = dir.join("ran.marker");
        let script = dir.join("fake-claude.sh");
        write_executable_script(
            &script,
            &format!(
                "#!/bin/sh\ntouch \"{}\"\nif [ \"$1\" = \"--version\" ]; then\n  echo fake-version 1.0.0\nelse\n  echo '{}'\nfi\n",
                marker.display(),
                kept.join(" "),
            ),
        );
        let program = script.to_string_lossy().into_owned();

        let r = probe(
            "claude",
            &program,
            Vec::new(),
            false,
            true,
            Some(&dir),
            Vec::new(),
        );

        assert!(
            marker.exists(),
            "--probe-binaries must actually execute a bin: override"
        );
        assert_eq!(r.probe_state, ProbeState::Probed);
        assert_eq!(r.version.as_deref(), Some("fake-version 1.0.0"));
        assert!(
            r.missing_flags.iter().any(|f| f.as_str() == omitted),
            "a probed override must still be checked for flag drift: {:?}",
            r.missing_flags
        );
        assert!(
            r.blockers.iter().any(|b| b.contains(omitted)),
            "{:?}",
            r.blockers
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn the_report_distinguishes_resolved_not_probed_from_probed_with_no_usable_version() {
        let dir = test_scratch_dir("silent");
        let script = dir.join("silent.sh");
        write_executable_script(&script, "#!/bin/sh\nexit 0\n");
        let program = script.to_string_lossy().into_owned();

        let not_probed = probe(
            "claude",
            &program,
            Vec::new(),
            false,
            false,
            Some(&dir),
            Vec::new(),
        );
        let probed = probe(
            "claude",
            &program,
            Vec::new(),
            false,
            true,
            Some(&dir),
            Vec::new(),
        );

        // Both report no version - the exact ambiguity P0-05 found - but they
        // must not be the same fact.
        assert!(not_probed.version.is_none());
        assert!(probed.version.is_none());
        assert_eq!(not_probed.probe_state, ProbeState::ResolvedNotProbed);
        assert_eq!(probed.probe_state, ProbeState::Probed);
        assert_ne!(
            not_probed.to_json()["probe_state"],
            probed.to_json()["probe_state"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn a_program_matching_its_tools_own_default_name_is_probed_even_without_probe_binaries() {
        // `default_program` returns its input verbatim for any tool it does
        // not special-case (only "cursor" maps elsewhere), so a program that
        // equals its own `tool` string IS that tool's default launcher by
        // the same rule `probe` itself uses - no bin: override involved,
        // even though the name here is a throwaway test path rather than a
        // real preset name.
        let dir = test_scratch_dir("default");
        let marker = dir.join("ran.marker");
        let script = dir.join("fake-tool.sh");
        write_executable_script(
            &script,
            &format!(
                "#!/bin/sh\ntouch \"{}\"\necho fake-version 1.0.0\n",
                marker.display()
            ),
        );
        let program = script.to_string_lossy().into_owned();
        assert_eq!(preset::default_program(&program), program);

        let r = probe(
            &program,
            &program,
            Vec::new(),
            false,
            false,
            Some(&dir),
            Vec::new(),
        );

        assert!(
            marker.exists(),
            "a tool's own default launcher must be probed without needing --probe-binaries"
        );
        assert_eq!(r.probe_state, ProbeState::Probed);
        assert_eq!(r.version.as_deref(), Some("fake-version 1.0.0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn a_probed_binary_runs_with_the_isolated_directory_as_its_cwd() {
        // The marker is written with a RELATIVE name, so it only lands
        // inside `dir` if the child's cwd was actually set there. If
        // isolation regressed, it would land in sfh's own working directory
        // instead and this assertion would fail.
        let dir = test_scratch_dir("isolated-cwd");
        let script = dir.join("relative-marker.sh");
        write_executable_script(
            &script,
            "#!/bin/sh\ntouch ran.marker\necho fake-version 1.0.0\n",
        );
        let program = script.to_string_lossy().into_owned();

        let r = probe(
            &program,
            &program,
            Vec::new(),
            false,
            false,
            Some(&dir),
            Vec::new(),
        );

        assert_eq!(r.probe_state, ProbeState::Probed);
        assert!(
            dir.join("ran.marker").exists(),
            "the probe must run with the isolated scratch directory as its cwd, not sfh's own"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
