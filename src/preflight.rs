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

use crate::{execute, flow, preset, state};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;

/// One tool the flow would actually launch, as preflight found it.
pub struct ToolReport {
    pub tool: String,
    pub program: String,
    pub resolved_path: Option<String>,
    pub version: Option<String>,
    pub info: Option<preset::AdapterInfo>,
    /// Required flags that this binary's `--help` did not mention. Empty when
    /// help could not be read at all (see `help_readable`).
    pub missing_flags: Vec<String>,
    pub help_readable: bool,
    /// Access levels this flow asks this tool for.
    pub requested_access: Vec<String>,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

/// One program a `cmd:` step would launch, as preflight found it.
///
/// Deliberately thinner than a `ToolReport`: preflight RESOLVES a custom command
/// and never runs it. `--help` and `--version` are safe to send to an adapter
/// sfh ships support for; they are not safe to send to an arbitrary program a
/// flow names, because `deploy.sh --help` may well deploy. Resolution answers
/// the question that actually bit - "which binary is this name?" - without
/// executing anything.
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
        json!({
            "sfh_version": self.sfh_version,
            "flow": self.flow_path,
            "flow_name": self.flow_name,
            "ok": self.ok(),
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
            "version": self.version,
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

/// Read a CLI's own `--help`. Bounded and never fatal: a tool that has no
/// `--help`, prints it to stderr, or wants to talk to a terminal is reported as
/// "help unreadable" rather than as a missing flag, because "sfh could not
/// check" and "the flag is gone" are different answers and only one of them
/// should stop a run.
fn read_help(program: &str) -> Option<String> {
    let out = execute::run_cmd(
        &execute::Invocation::Argv(vec![program.to_string(), "--help".to_string()]),
        None,
        None,
        Some(std::time::Duration::from_secs(15)),
        &[],
        &[],
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

/// Inspect one tool/program pair without calling a model.
///
/// `required` distinguishes the two questions preflight answers. A flow that
/// launches this tool cannot start without it, so a missing binary is a
/// blocker. A flowless survey is just asking what is installed, and "codex is
/// not on this machine" is an answer, not a failure.
fn probe(tool: &str, program: &str, requested_access: Vec<String>, required: bool) -> ToolReport {
    let info = preset::adapter_info(tool);
    let resolved_path = execute::which(program);
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    let mut missing_flags = Vec::new();
    let mut help_readable = false;
    let mut version = None;
    if resolved_path.is_none() {
        let msg = format!("'{program}' is not on PATH (install it, or set bin: to its full path)");
        if required {
            blockers.push(format!("{msg} - this flow cannot start without it"));
        } else {
            warnings.push(msg);
        }
    } else {
        version = execute::probe_version(program);
        if version.is_none() {
            warnings.push(format!(
                "'{program} --version' produced nothing usable, so sfh cannot record which build this run used"
            ));
        }
        match read_help(program) {
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
    ToolReport {
        tool: tool.to_string(),
        program: program.to_string(),
        resolved_path,
        version,
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
    let resolved_path = execute::which(program);
    match &resolved_path {
        None => blockers.push(format!(
            "'{program}' ({where_}) is not on PATH - this flow cannot start without it"
        )),
        Some(path) => {
            // Only for a BARE name: PATH made this choice, not the flow author.
            // A path or an explicit `wsl` invocation is a deliberate statement
            // and sfh does not second-guess it.
            let bare = !program.contains('/') && !program.contains('\\');
            if bare && execute::is_wsl_launcher(path) {
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
pub fn all_adapters() -> Report {
    let tools = flow::TOOLS
        .iter()
        .map(|t| probe(t, &preset::default_program(t), Vec::new(), false))
        .collect();
    Report {
        sfh_version: env!("CARGO_PKG_VERSION"),
        flow_path: None,
        flow_name: None,
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
pub fn for_flow(path: &Path, root: &state::StateRoot, overlays: &[std::path::PathBuf]) -> Report {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    let flow = match flow::load_with_overlays(path, overlays) {
        Ok(f) => f,
        Err(e) => {
            return Report {
                sfh_version: env!("CARGO_PKG_VERSION"),
                flow_path: Some(path.display().to_string()),
                flow_name: None,
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
    let mut by_program: std::collections::BTreeMap<(String, String), BTreeSet<String>> =
        Default::default();
    for r in &resolved {
        let program = r
            .bin
            .clone()
            .unwrap_or_else(|| preset::default_program(&r.tool));
        by_program
            .entry((r.tool.clone(), program))
            .or_default()
            .extend(r.access.iter().cloned());
    }
    let tools = by_program
        .into_iter()
        .map(|((tool, program), access)| probe(&tool, &program, access.into_iter().collect(), true))
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
        if let Some(v) = &t.version {
            println!("     version: {v}");
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
        if !t.help_readable && t.resolved_path.is_some() {
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

/// `sfh preflight [flow.yaml]`.
pub fn run(
    flow_path: Option<&Path>,
    root: &state::StateRoot,
    overlays: &[std::path::PathBuf],
    as_json: bool,
) -> i32 {
    let report = match flow_path {
        Some(p) => for_flow(p, root, overlays),
        None => all_adapters(),
    };
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
            // Everything preflight can block on is "this machine cannot provide
            // what the flow asks for": a missing binary, a CLI whose flags have
            // moved, an access level the tool has no headless equivalent for.
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
                .unwrap_or_else(|| "preflight found blockers".to_string());
            crate::machine::emit(&crate::machine::error_envelope(
                "preflight",
                crate::machine::ErrorCode::CapabilityUnavailable,
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
    fn a_flowless_preflight_covers_every_preset_and_calls_no_model() {
        let r = all_adapters();
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

    #[test]
    fn no_adapter_claims_a_minimum_version_it_never_verified() {
        for tool in flow::TOOLS {
            let i = preset::adapter_info(tool).expect("every preset has metadata");
            assert_eq!(
                i.minimum_version, None,
                "{tool} pins a floor that was not confirmed against the CLI's docs and a live probe"
            );
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
        );
        assert!(
            r.blockers.iter().any(|b| b.contains("access: write")),
            "an unsupported level must be a blocker: {:?}",
            r.blockers
        );
    }
}
