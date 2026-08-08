use serde::{Deserialize, Serialize};
use serde_yaml_ng as yaml;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    /// Public flow format version. Omitted files are read as version 1 for
    /// backwards compatibility; new examples write it explicitly.
    pub api_version: Option<u32>,
    pub name: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, yaml::Value>,
    #[serde(default)]
    pub defaults: Defaults,
    /// Named bundles of tool settings referenced by steps via `use:`.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    /// Where this run's side effects belong. Omitted (the default) keeps the
    /// long-standing behaviour exactly: every step runs in the caller's cwd, or
    /// in whatever `cwd:` resolves to, and sfh creates nothing.
    ///
    /// Every v1.2 key added to this file is `skip_serializing_if`-guarded. The
    /// effective-config fingerprint is a serialization of this struct, and it
    /// is what `--resume` compares; without the guard, merely UPGRADING sfh
    /// would change the fingerprint of every flow ever written and make every
    /// existing run dir unresumable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceConfig>,
    /// Named context sources a step can pull in by name. sfh does not interpret
    /// what a context MEANS - `task`, `review_rules`, `sources` are the flow
    /// author's words - it only pins where each one came from, in what order,
    /// and at what hash.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contexts: BTreeMap<String, ContextSource>,
    pub steps: Vec<Step>,
    /// Set only by `load_lenient`, never by the YAML - it is the loader saying
    /// "this flow was accepted under the pre-1.0 rules so a 0.x run could be
    /// resumed". The relaxation has to travel with the flow: validate warning
    /// and then letting `precheck` and `prepare_leaf` refuse the same thing a
    /// moment later meant old runs still could not be resumed, which is the
    /// whole point of the lenient path.
    #[serde(skip)]
    pub legacy_resume: bool,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    pub tool: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: Option<String>,
    pub cwd: Option<String>,
    pub timeout_sec: Option<u64>,
    pub max_visits: Option<u32>,
    pub max_total_steps: Option<u32>,
    /// Default concurrency for parallel: / foreach: steps (default 4).
    pub max_parallel: Option<u32>,
    /// Extra ceiling per tool name, e.g. {opencode: 1}. Applies across a fan-out.
    #[serde(default)]
    pub tool_max_parallel: BTreeMap<String, u32>,
    /// Fail before spawning if a rendered prompt exceeds this many chars.
    pub max_prompt_chars: Option<u64>,
    /// Hard ceiling on what sfh prints to stdout (default 200000).
    pub max_emit_chars: Option<u64>,
    /// Abort the flow once accumulated reported cost exceeds this (USD).
    pub max_cost_usd: Option<f64>,
    /// Abort the flow after this many wall-clock seconds.
    pub wall_clock_sec: Option<u64>,
    /// Where to jump when the run comes within `budget_reserve` of a ceiling,
    /// as `goto:<id>` (end/fail/stuck allowed). Unset keeps the old behaviour:
    /// the ceiling itself ends the run with an error and no wrap-up.
    pub on_budget: Option<String>,
    /// Headroom held back from each ceiling for the landing chain to spend.
    /// Omitted axes reserve nothing, so the landing fires at the ceiling.
    pub budget_reserve: Option<BudgetReserve>,
    pub retry: Option<Retry>,
    /// transient (default) | any | never
    pub retry_on: Option<String>,
    /// How long a step may be silent before a timeout counts as a hang rather
    /// than as honest overrun (default 300). Only `retry_on: transient` uses it.
    pub hang_after_sec: Option<u64>,
    /// Env applied to every child process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// When several steps fork the same parent session in one batch, run the
    /// first one alone so the provider's prompt cache is warm before the rest
    /// start. auto (default: only where it measurably pays) | always | never.
    pub fork_warmup: Option<String>,
    /// What a resume does with work that started but never recorded a durable
    /// end. Omitted keeps the historical behaviour (`rerun`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay: Option<Replay>,
    /// Refuse to spawn when a step's assembled context bundle exceeds this many
    /// characters. sfh never summarizes or silently drops context to fit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_context_chars: Option<u64>,
    /// What to do when a tool's own protocol certifies the turn as successful
    /// but the process exits non-zero. Omitted keeps the adapter's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_conflict: Option<ExitConflict>,
}

/// How to resolve a disagreement between a tool's exit code and its own
/// structured protocol.
///
/// This only ever applies where sfh has POSITIVE evidence of success:
/// `ProtocolEvidence::certifies_success`, meaning the documented terminal
/// record was found, it was well formed, and it said the turn succeeded. Raw
/// text, an unknown status, a malformed envelope or a missing terminal record
/// can never reach this decision, so `trust_protocol` cannot turn a usage error
/// printed on stdout into a successful step.
#[derive(Deserialize, Serialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ExitConflict {
    /// The exit code wins: the step fails. Correct wherever a CLI's exit status
    /// is trustworthy, which is most of them.
    #[default]
    Fail,
    /// The protocol wins: the step succeeds. Declare this for a CLI that is
    /// known to exit non-zero for reasons that do not invalidate the answer -
    /// for example one that returns 1 because some intermediate tool call
    /// failed, after producing and committing a complete final result.
    TrustProtocol,
}

impl ExitConflict {
    pub fn as_str(self) -> &'static str {
        match self {
            ExitConflict::Fail => "fail",
            ExitConflict::TrustProtocol => "trust_protocol",
        }
    }
}

/// What a particular exit code MEANS for this step.
///
/// An exit code carries two different facts at once and sfh could only read
/// one. "The process ended cleanly" is a transport fact. "The work is done" is
/// a semantic one, and only the flow author knows how this command spells it.
/// A gate that exits 2 for "ran fine, the acceptance criteria are not met yet"
/// was indistinguishable from one that exits 2 because it crashed - so sfh
/// failed the step, and `retry_on: transient` could then re-run an expensive
/// suite for a deliberate, correct, reproducible answer.
///
/// The vocabulary is deliberately tiny and domain-free. sfh never learns what
/// "acceptance" or "review" or "incomplete" mean; it learns only whether to
/// carry on, retry, or stop. Everything domain-shaped goes in `label`, which
/// sfh stores, exposes and routes on without ever interpreting.
#[derive(Deserialize, Serialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeResult {
    /// The work is done. The step succeeds however it exited.
    Complete,
    /// The step did its job and reports there is more to do. NOT a failure:
    /// `on_error` does not fire and no retry is considered. Route on the label.
    Continue,
    /// A failure worth another attempt, whatever the text says. Under
    /// `retry_on: transient` this retries; the declaration replaces the
    /// needle-matching guess rather than adding to it.
    Retryable,
    /// A failure that is final. Never retried under `retry_on: transient`.
    #[default]
    Fail,
}

impl OutcomeResult {
    pub fn as_str(self) -> &'static str {
        match self {
            OutcomeResult::Complete => "complete",
            OutcomeResult::Continue => "continue",
            OutcomeResult::Retryable => "retryable",
            OutcomeResult::Fail => "fail",
        }
    }

    /// Whether a step ending this way counts as succeeding.
    pub fn is_success(self) -> bool {
        matches!(self, OutcomeResult::Complete | OutcomeResult::Continue)
    }
}

/// One `exit code -> meaning` declaration.
#[derive(Deserialize, Serialize, Clone, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub result: OutcomeResult,
    /// A free-form name for this outcome. sfh stores it, exposes it as
    /// `{{steps.<id>.label}}`, routes on it with `when_label_is:`, and records
    /// it in the durable log - and never assigns it any meaning of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// How to treat a step whose effects may have happened even though sfh never
/// recorded that it finished.
///
/// This is NOT retry (another attempt at the same invocation), NOT fallback
/// (a different profile), NOT a route revisit, and NOT the reuse of a completed
/// step's durable result. It is the narrower question a crash leaves behind:
/// the step started, something may have happened out in the world, and no
/// record says what.
#[derive(Deserialize, Serialize, Default, Clone, Copy, PartialEq, Debug)]
#[serde(deny_unknown_fields)]
pub struct Replay {
    /// rerun (default) | stuck | fail
    pub unfinished: Option<ReplayPolicy>,
}

#[derive(Deserialize, Serialize, Default, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ReplayPolicy {
    /// Run it again. Correct for a pure computation; a gamble for anything that
    /// touched the world, which is why `validate --strict` says so.
    #[default]
    Rerun,
    /// Stop with exit 4, keeping the workspace and partial artifacts, so a
    /// human can decide. Nothing is launched.
    Stuck,
    /// Stop with exit 1. Nothing is launched.
    Fail,
}

impl ReplayPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ReplayPolicy::Rerun => "rerun",
            ReplayPolicy::Stuck => "stuck",
            ReplayPolicy::Fail => "fail",
        }
    }
}

/// What a step is declared to touch. A user declaration, not an inference sfh
/// makes about the work: it decides workspace selection, warnings and replay
/// policy, and nothing else.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Effects {
    /// Reads only. Safe to re-run, and needs no workspace of its own.
    Read,
    /// Writes inside the workspace.
    Workspace,
    /// Touches something outside it - a deploy, an API call, a message sent.
    External,
    /// Not declared and not inferable (the default for a custom `cmd:`).
    Unknown,
}

impl Effects {
    pub fn as_str(self) -> &'static str {
        match self {
            Effects::Read => "read",
            Effects::Workspace => "workspace",
            Effects::External => "external",
            Effects::Unknown => "unknown",
        }
    }

    /// Whether this step might change the workspace or the world. `unknown`
    /// counts: an undeclared custom command is treated as a potential writer,
    /// because assuming otherwise is the assumption that loses work.
    pub fn is_potential_writer(self) -> bool {
        !matches!(self, Effects::Read)
    }
}

/// Where a run's side effects live.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// current (default) | directory | git-worktree | auto
    pub mode: Option<WorkspaceMode>,
    /// The source directory or repository, resolved against the flow file.
    /// Omitted, a `git-worktree` workspace branches the repository the CALLER
    /// is standing in - which is where every step ran before v1.2 - and a
    /// `directory` workspace requires this key outright.
    pub root: Option<String>,
    /// git-worktree only: the ref to branch from. Omitted means the repository's
    /// HEAD at the moment the run starts.
    pub base: Option<String>,
    /// auto (default) | keep
    pub cleanup: Option<WorkspaceCleanup>,
    /// Allow a flow whose static shape lets two potential writers into the same
    /// workspace at once. Off by default, and recorded as an unsafe override.
    pub allow_concurrent_writers: Option<bool>,
    /// Compare the workspace against its last durable checkpoint on resume.
    /// Defaults to true.
    pub verify_on_resume: Option<bool>,
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceMode {
    /// The caller's cwd, exactly as every release before 1.2.
    #[default]
    Current,
    /// A directory the user names. sfh does not create or delete it.
    Directory,
    /// A Git worktree sfh creates, owns and may clean up.
    GitWorktree,
    /// Decide from the flow's static shape alone (see `workspace_plan`).
    Auto,
}

impl WorkspaceMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceMode::Current => "current",
            WorkspaceMode::Directory => "directory",
            WorkspaceMode::GitWorktree => "git-worktree",
            WorkspaceMode::Auto => "auto",
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceCleanup {
    /// Remove an sfh-owned worktree after a clean, successful run. Never
    /// discards uncommitted work, and never deletes the branch.
    #[default]
    Auto,
    /// Keep it whatever happened.
    Keep,
}

impl WorkspaceCleanup {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceCleanup::Auto => "auto",
            WorkspaceCleanup::Keep => "keep",
        }
    }
}

impl WorkspaceConfig {
    pub fn mode(&self) -> WorkspaceMode {
        self.mode.unwrap_or_default()
    }
    pub fn cleanup(&self) -> WorkspaceCleanup {
        self.cleanup.unwrap_or_default()
    }
    pub fn allow_concurrent_writers(&self) -> bool {
        self.allow_concurrent_writers.unwrap_or(false)
    }
    pub fn verify_on_resume(&self) -> bool {
        self.verify_on_resume.unwrap_or(true)
    }
}

/// One named context. Exactly one of `file`, `inline` or `template` must be
/// set: a source with two origins has no single answer to "where did this text
/// come from", which is the whole point of naming it.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ContextSource {
    /// A path, relative to the flow file unless absolute.
    pub file: Option<String>,
    /// Literal text written in the flow.
    pub inline: Option<String>,
    /// A template rendered with the same variables a prompt sees.
    pub template: Option<String>,
    /// Refuse this source if it exceeds this many characters.
    pub max_chars: Option<u64>,
    /// Explicit opt-in to reading a file outside the flow directory and the
    /// resolved workspace. Recorded as an unsafe override wherever it is used.
    pub allow_external: Option<bool>,
    /// Tolerate a missing file instead of failing the step.
    pub optional: Option<bool>,
}

impl ContextSource {
    /// `Err` when the source is not exactly one thing.
    pub fn kind(&self) -> Result<&'static str, String> {
        match (
            self.file.is_some(),
            self.inline.is_some(),
            self.template.is_some(),
        ) {
            (true, false, false) => Ok("file"),
            (false, true, false) => Ok("inline"),
            (false, false, true) => Ok("template"),
            (false, false, false) => {
                Err("needs exactly one of file:, inline: or template: (it has none)".into())
            }
            _ => Err(
                "needs exactly one of file:, inline: or template: (it has more than one)".into(),
            ),
        }
    }
}

/// How a step's assembled context reaches the tool.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContextDelivery {
    /// sfh puts a deterministic, delimited bundle in front of the prompt.
    #[default]
    Prepend,
    /// The prompt is untouched; the bundle is a file the prompt can point at
    /// with `{{context_file}}`.
    File,
}

impl ContextDelivery {
    pub fn as_str(self) -> &'static str {
        match self {
            ContextDelivery::Prepend => "prepend",
            ContextDelivery::File => "file",
        }
    }
}

/// What `workspace:` resolves to for a given flow, decided without running
/// anything. `plan`, `preflight` and the engine all read the same answer.
#[derive(Clone, Debug)]
pub struct WorkspacePlan {
    /// What the flow asked for, before `auto` was resolved.
    pub declared: WorkspaceMode,
    /// What it resolved to.
    pub resolved: WorkspaceMode,
    pub root: Option<String>,
    pub base: Option<String>,
    pub cleanup: WorkspaceCleanup,
    pub verify_on_resume: bool,
    pub allow_concurrent_writers: bool,
    /// True when a state root is required, because sfh would create something.
    pub needs_state_root: bool,
    pub potential_writers: Vec<String>,
    pub warnings: Vec<String>,
}

impl WorkspacePlan {
    /// How many workspaces sfh will create for the whole run. One per run at
    /// most in v1.2 - not one per step, and not one per visit.
    pub fn managed_count(&self) -> u32 {
        u32::from(self.resolved == WorkspaceMode::GitWorktree)
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "declared_mode": self.declared.as_str(),
            "mode": self.resolved.as_str(),
            "root": self.root,
            "base": self.base,
            "cleanup": self.cleanup.as_str(),
            "verify_on_resume": self.verify_on_resume,
            "allow_concurrent_writers": self.allow_concurrent_writers,
            "managed_workspaces": self.managed_count(),
            "potential_writers": self.potential_writers,
            "warnings": self.warnings,
        })
    }
}

/// What each step would be handed, before anything runs.
#[derive(Clone, Debug, Default)]
pub struct ContextPlan {
    /// (step id, context names, delivery)
    pub steps: Vec<(String, Vec<String>, ContextDelivery)>,
    /// (name, kind, source description, static size if knowable)
    pub sources: Vec<(String, String, String, Option<u64>)>,
    pub max_context_chars: Option<u64>,
}

impl ContextPlan {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "max_context_chars": self.max_context_chars,
            "sources": self.sources.iter().map(|(name, kind, source, chars)| serde_json::json!({
                "name": name, "kind": kind, "source": source, "chars": chars,
            })).collect::<Vec<_>>(),
            "steps": self.steps.iter().map(|(id, names, delivery)| serde_json::json!({
                "step": id, "context": names, "context_delivery": delivery.as_str(),
            })).collect::<Vec<_>>(),
        })
    }
}

/// How much of each ceiling `on_budget` keeps back for the landing chain. The
/// landing threshold is `ceiling - reserve` on each axis INDEPENDENTLY: cost
/// and wall-clock never borrow from one another.
#[derive(Deserialize, Serialize, Default, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct BudgetReserve {
    pub cost_usd: Option<f64>,
    pub wall_clock_sec: Option<u64>,
}

impl Defaults {
    /// The step (or terminal) `on_budget` lands on, with the `goto:` prefix
    /// removed. None when the flow declared no landing - validate has already
    /// refused any other spelling, so an unprefixed value cannot reach here.
    pub fn budget_goto(&self) -> Option<&str> {
        self.on_budget.as_deref()?.strip_prefix("goto:")
    }

    /// USD the landing chain may still spend after it fires (0 when unset).
    pub fn budget_reserve_usd(&self) -> f64 {
        self.budget_reserve
            .and_then(|r| r.cost_usd)
            .unwrap_or(0.0)
            .max(0.0)
    }

    /// Seconds the landing chain may still take after it fires (0 when unset).
    pub fn budget_reserve_sec(&self) -> u64 {
        self.budget_reserve
            .and_then(|r| r.wall_clock_sec)
            .unwrap_or(0)
    }
}

#[derive(Deserialize, Serialize, Default, Clone)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub tool: Option<String>,
    pub bin: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: Option<String>,
    pub agent: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_sec: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    /// Extra attempts after the first (0 = no retry).
    pub max: u32,
    /// First backoff delay; doubles each attempt (default 5).
    pub backoff_sec: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    /// Profile name from `profiles:` supplying default tool settings.
    #[serde(rename = "use")]
    pub use_: Option<String>,
    /// Preset tool: codex | claude | opencode | grok | agy | pi. Omit to use `cmd`.
    pub tool: Option<String>,
    /// Executable path override for the preset tool (e.g. a specific codex.exe).
    pub bin: Option<String>,
    pub model: Option<String>,
    /// Reasoning effort. codex: model_reasoning_effort, claude: --effort,
    /// opencode: --variant, grok: --reasoning-effort, agy: --effort,
    /// pi: --thinking.
    pub effort: Option<String>,
    /// read | write | full. Mandatory for AI steps (step, profile or defaults);
    /// there is no implicit default.
    pub access: Option<String>,
    /// Explicit escape hatch: allow args: that would override the declared
    /// access level, and continue_from/fork_from at a higher access than the
    /// original session. Both are refused by default (fail-closed).
    pub allow_access_override: Option<bool>,
    /// Agent name (opencode/claude/grok/agy --agent).
    pub agent: Option<String>,
    /// Extra raw args appended to the preset command line.
    #[serde(default)]
    pub args: Vec<String>,
    /// Custom command instead of a preset. Array = spawned directly (no shell),
    /// String = run through cmd /C (Windows) or sh -c (Unix).
    pub cmd: Option<Cmd>,
    /// Explicit escape hatch: allow template expansion inside a string-form
    /// cmd (and inside the shell text of an argv form that wraps a shell, e.g.
    /// ["sh","-c","..."]). Off by default, because a substituted value is
    /// re-parsed by the shell and a metacharacter blacklist is not a security
    /// boundary (a hostile value can be a dangerous OPTION to the target
    /// program without containing any shell metacharacter at all).
    pub unsafe_shell_template: Option<bool>,
    /// Explicit escape hatch: allow run-derived templates (step output, notes,
    /// foreach items, vars restored from a resumed run dir) in executed-
    /// privileged fields: bin, cwd and argv[0] of a custom cmd. Off by default,
    /// because those fields are executed with sfh's own OS rights and untrusted
    /// values there select an arbitrary binary or working directory
    /// (rev_break #12). A trusted judge step producing the next step's cwd is
    /// the legitimate use; setting this accepts the risk for this step.
    pub allow_dynamic_exec_paths: Option<bool>,
    pub prompt: Option<String>,
    /// For custom `cmd` only: "prompt" pipes the rendered prompt to stdin.
    pub stdin: Option<String>,
    pub cwd: Option<String>,
    pub timeout_sec: Option<u64>,
    pub max_visits: Option<u32>,
    /// fail (default) | continue | goto:<id> - on non-zero exit or timeout.
    pub on_error: Option<String>,
    /// fail (default) | continue | goto:<id> - when max_visits is exhausted.
    pub on_max_visits: Option<String>,
    #[serde(default)]
    pub route: Vec<Route>,
    /// Run these child steps concurrently; the step's output is the aggregation.
    pub parallel: Option<Vec<Step>>,
    /// Fan the step out over a list of items rendered from a template.
    pub foreach: Option<Foreach>,
    /// Concurrency cap for parallel:/foreach: (default: defaults.max_parallel or 4).
    pub max_parallel: Option<u32>,
    /// "append": append this step's chain output to {{notes}} (run_dir/notes.md).
    pub notes: Option<String>,
    pub max_prompt_chars: Option<u64>,
    /// Auto-compress the chain output with a cheap model when it exceeds a threshold.
    pub compact: Option<Compact>,
    /// Resume the session of a previously executed preset step instead of starting fresh.
    pub continue_from: Option<String>,
    /// Branch off a previously executed step's session: the child inherits the
    /// parent's history but gets its own session, so several steps can diverge
    /// from one context concurrently. claude/opencode/grok/pi only.
    pub fork_from: Option<String>,
    pub retry: Option<Retry>,
    pub retry_on: Option<String>,
    /// Silence (seconds) after which this step's timeout is classified as a
    /// hang, which `retry_on: transient` does retry. Overrides defaults.
    pub hang_after_sec: Option<u64>,
    /// Profiles to try (in order) if the step still fails after its retries.
    #[serde(default)]
    pub fallback: Vec<String>,
    /// Accept an empty final message instead of failing the step.
    pub allow_empty: Option<bool>,
    /// What this step touches: read | workspace | external | unknown. Omitted,
    /// it is inferred from `access:` for a preset step and is `unknown` for a
    /// custom `cmd:` - the conservative direction, since an undeclared command
    /// may well write.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effects: Option<Effects>,
    /// Names from the flow's `contexts:` map, in the order they should appear.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    /// prepend (default) | file
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_delivery: Option<ContextDelivery>,
    /// Overrides `defaults.replay` for this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay: Option<Replay>,
    /// Overrides `defaults.exit_conflict` for this step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_conflict: Option<ExitConflict>,
    /// What this step's own exit codes mean. An exit code with no entry here
    /// keeps its historical reading exactly, so declaring one code says
    /// nothing about the others.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outcomes: BTreeMap<i32, Outcome>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub env_remove: Vec<String>,
}

impl Step {
    /// The declared effects, or the inference the spec fixes for an omitted
    /// declaration. sfh never guesses from the prompt or the command text -
    /// only from what the flow already says.
    pub fn effects(&self, flow: &Flow) -> Effects {
        if let Some(e) = self.effects {
            return e;
        }
        // A group is exactly as effectful as its most effectful member.
        if let Some(children) = &self.parallel {
            return children
                .iter()
                .map(|c| c.effects(flow))
                .max_by_key(|e| match e {
                    Effects::Read => 0,
                    Effects::Workspace => 1,
                    Effects::External => 2,
                    Effects::Unknown => 3,
                })
                .unwrap_or(Effects::Read);
        }
        if self.cmd.is_some() {
            // An undeclared command is a potential writer. Assuming otherwise
            // is the assumption that silently loses somebody's work.
            return Effects::Unknown;
        }
        match crate::leaf::effective(flow, self).map(|e| e.access) {
            Ok(crate::preset::Access::Read) => Effects::Read,
            Ok(_) => Effects::Workspace,
            Err(_) => Effects::Unknown,
        }
    }

    /// The replay policy in force for this step.
    pub fn replay_policy(&self, flow: &Flow) -> ReplayPolicy {
        self.replay
            .and_then(|r| r.unfinished)
            .or_else(|| flow.defaults.replay.and_then(|r| r.unfinished))
            .unwrap_or_default()
    }

    pub fn context_delivery(&self) -> ContextDelivery {
        self.context_delivery.unwrap_or_default()
    }

    /// What this step declares about exit-code/protocol disagreement, or `None`
    /// when it declares nothing and the adapter's own default should stand.
    /// Kept as an Option so "the flow said nothing" and "the flow said fail"
    /// are distinguishable: only the latter overrides an adapter that is
    /// documented to get its exit codes wrong.
    pub fn exit_conflict(&self, flow: &Flow) -> Option<ExitConflict> {
        self.exit_conflict.or(flow.defaults.exit_conflict)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Foreach {
    /// Template rendered, then split into items.
    pub from: String,
    /// lines (default) | json | separator:<sep>
    pub split: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Compact {
    /// Compress when chain output exceeds this many chars.
    pub when_over: u64,
    /// Profile to run the summarizer with (or set tool/bin/model/effort below).
    #[serde(rename = "use")]
    pub use_: Option<String>,
    pub tool: Option<String>,
    pub bin: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Custom instruction; the original text is appended after it.
    pub instruction: Option<String>,
    /// Target size hint used in the default instruction (default: when_over / 2).
    pub target_chars: Option<u64>,
    /// Never feed the summarizer more than this many chars (default 120000);
    /// larger inputs are head+tail sampled first.
    pub max_input_chars: Option<u64>,
    pub timeout_sec: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub enum Cmd {
    Shell(String),
    Argv(Vec<String>),
}

/// One `route:` rule. Every `when_*` present in a rule must hold (AND); a rule
/// with none is the catch-all. Adding one here means adding it to
/// schema/flow.schema.json, `Route::is_catch_all`, any exclusivity check that
/// enumerates predicates, and - if its text is templated -
/// `engine::precheck`'s route-condition list. The precheck one is the easiest
/// to miss and the most expensive: without it a template typo survives validate
/// and dry-run and only kills the run after the guarded step has been billed
/// (that is exactly what happened to `when_stderr_matches`).
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub when_contains: Option<String>,
    pub when_matches: Option<String>,
    /// Same as when_contains but only the LAST non-empty line is searched -
    /// the deterministic way to read a "VERDICT: OK" trailer.
    pub when_last_line_contains: Option<String>,
    /// Exact match against the trimmed last non-empty line.
    pub when_last_line_is: Option<String>,
    pub when_last_line_matches: Option<String>,
    /// Equality against the step's OWN normalized exit code - the same number
    /// `{{steps.<id>.exit}}` exposes, after sfh has folded in-band failure
    /// reports and session validation into it. A fan-out group has no exit of
    /// its own and compares against the composite sfh records (1 when the group
    /// hard-failed, 0 otherwise). Mainly for `on_error: continue` probes that
    /// have to prove a step failed for THE reason under test.
    pub when_exit: Option<i32>,
    /// Rust regex over the step's cleaned stderr (`<id>.err.txt`). Read from the
    /// file in both live and resumed runs, so the two cannot disagree; a stderr
    /// file that is missing (or a step that has none, like a fan-out group)
    /// never matches.
    pub when_stderr_matches: Option<String>,
    /// Exact match against the label an `outcomes:` entry gave this step.
    ///
    /// The deterministic alternative to reading a verdict out of prose. A
    /// `when_last_line_is: PASS` rule depends on the model ending its answer
    /// on exactly that token and nothing else - one trailing space, one closing
    /// remark, and the run goes to stuck for a formatting reason. A label comes
    /// from the flow's own exit-code table, so it is whatever the flow said it
    /// was. A step with no matching outcome has no label and never matches.
    pub when_label_is: Option<String>,
    /// Equality against the outcome class sfh recorded: complete | continue |
    /// retryable | fail.
    pub when_outcome_is: Option<OutcomeResult>,
    /// Count the members of THIS step's fan-out that reported a given verdict.
    /// Only on a `parallel:`/`foreach:` step, and only alone in its rule (see
    /// validate). See `WhenMembers`.
    pub when_members: Option<WhenMembers>,
    /// Step id, or one of the three terminals: "end" (finish, success),
    /// "fail" (finish, failure) or "stuck" (finish, needs a human - exit 4).
    pub goto: String,
}

impl Route {
    pub fn is_catch_all(&self) -> bool {
        self.when_contains.is_none()
            && self.when_matches.is_none()
            && self.when_last_line_contains.is_none()
            && self.when_last_line_is.is_none()
            && self.when_last_line_matches.is_none()
            && self.when_exit.is_none()
            && self.when_stderr_matches.is_none()
            && self.when_label_is.is_none()
            && self.when_outcome_is.is_none()
            && self.when_members.is_none()
    }
}

/// "How many of the fan-out's members ended cleanly AND signed off with exactly
/// this line" - the deterministic way to hold a vote among N independent
/// judges.
///
/// It exists because counting the verdicts out of the group's aggregated text
/// cannot be made correct. That text is the members' output concatenated with
/// no per-member marking of success: a member that printed the winning line and
/// then exited 1 is, in the text, indistinguishable from one that passed. The
/// count therefore comes from sfh's own per-member record, never from a string
/// search - and the member's OWN last line is what counts, so a needle quoted
/// mid-answer is not a vote.
///
/// Exactly one quantifier: `at_least: <n>` (n >= 1) or `all: true`. There is no
/// `contains`/regex variant on purpose - a vote is an exact word or it is not a
/// vote.
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct WhenMembers {
    /// The exact line a member must end on to be counted. Templated like the
    /// other route conditions, then compared for equality against the member's
    /// trimmed last non-empty line.
    pub last_line_is: String,
    /// Match when at least this many members voted. Mutually exclusive with
    /// `all`.
    pub at_least: Option<u32>,
    /// Match when EVERY member voted. A fan-out with no members never matches:
    /// "all of nothing" is true in logic and would turn a foreach that produced
    /// zero workers into unanimous agreement.
    pub all: Option<bool>,
}

/// Goto targets that end the flow instead of naming a step.
///
/// All terminal names are refused as step ids (case-insensitively) so a route
/// target can never silently shadow a real node.
pub const TERMINALS: [&str; 3] = ["end", "fail", "stuck"];

pub const TOOLS: [&str; 7] = ["codex", "claude", "opencode", "grok", "agy", "pi", "cursor"];

/// Concurrency ceiling for a fan-out when neither the step nor defaults says.
/// Mirrors the value the engine applies; kept here so static analysis and
/// execution cannot drift apart about how many writers can be in flight.
pub const DEFAULT_MAX_PARALLEL: u32 = 4;
/// Times one step may be entered when neither the step nor defaults says.
pub const DEFAULT_MAX_VISITS: u32 = 5;

/// One concrete way a flow can launch a preset tool, as collected by
/// `Flow::resolved_tools`. Ordered so a BTreeSet dedupes and sorts it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedTool {
    pub tool: String,
    pub bin: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// The access level this launch asks for. Part of the identity because the
    /// same tool at read and at full is two different things to report on, and
    /// preflight has to name both.
    pub access: Vec<String>,
}

pub fn load(path: &Path) -> Result<Flow, String> {
    load_with_overlays(path, &[])
}

/// One profile as an overlay file writes it.
///
/// This is NOT `Profile`. An overlay has to distinguish "the file did not
/// mention args" from "the file set args to the empty list", and a `Vec` with
/// `#[serde(default)]` cannot: both deserialize to `vec![]`, so an overlay that
/// said nothing about `args` would silently erase the flow's own. Every field
/// here is an `Option`, and only the ones actually present are applied.
#[derive(Deserialize, Serialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ProfileOverlay {
    pub tool: Option<String>,
    pub bin: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub access: Option<String>,
    pub agent: Option<String>,
    /// Present: replaces the profile's args entirely. Absent: keeps them.
    pub args: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub timeout_sec: Option<u64>,
    /// Merged key by key. Removing a key is out of scope for v1.2.
    pub env: Option<BTreeMap<String, String>>,
}

impl ProfileOverlay {
    fn apply_to(&self, p: &mut Profile) {
        macro_rules! scalar {
            ($($f:ident),*) => { $( if let Some(v) = &self.$f { p.$f = Some(v.clone()); } )* };
        }
        scalar!(tool, bin, model, effort, access, agent, cwd);
        if let Some(v) = self.timeout_sec {
            p.timeout_sec = Some(v);
        }
        if let Some(args) = &self.args {
            p.args = args.clone();
        }
        if let Some(env) = &self.env {
            for (k, v) in env {
                p.env.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Load a flow, then apply profile overlay files in order.
///
/// The point is a flow that stays portable: a shared flow says `use: judge`,
/// and whoever runs it decides which CLI and which model a `judge` is, without
/// editing (and therefore re-fingerprinting) the flow. Writing `tool:` straight
/// into a step keeps working exactly as before - an overlay file is never
/// required.
///
/// Precedence, highest first:
///
/// ```text
/// step field  >  --profiles overlay  >  flow inline profile  >  ~/.sfh/profiles.yaml  >  defaults
/// ```
///
/// Later `--profiles` files win over earlier ones, so a caller can layer a
/// machine-local file on top of a team-shared one.
pub fn load_with_overlays(path: &Path, overlays: &[std::path::PathBuf]) -> Result<Flow, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut flow: Flow = yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    merge_global_profiles(&mut flow)?;
    apply_profile_overlays(&mut flow, overlays)?;
    validate(&flow, false)?;
    Ok(flow)
}

/// Read and apply overlay files. An overlay naming a profile the flow does not
/// have creates it: that is how a flow can declare `use: judge` with no inline
/// definition at all and let the caller supply one.
pub fn apply_profile_overlays(
    flow: &mut Flow,
    overlays: &[std::path::PathBuf],
) -> Result<(), String> {
    for file in overlays {
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("cannot read profile overlay {}: {e}", file.display()))?;
        let parsed = yaml::from_str::<OverlayFile>(&text)
            .map_err(|e| format!("{}: invalid profile overlay: {e}", file.display()))?;
        for (name, overlay) in parsed.profiles {
            overlay.apply_to(flow.profiles.entry(name).or_default());
        }
    }
    Ok(())
}

/// An overlay file. `profiles:` may be the top level or nested under a
/// `profiles:` key, because both spellings are what people actually write.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OverlayFile {
    #[serde(default)]
    profiles: BTreeMap<String, ProfileOverlay>,
}

/// Load a flow for a --resume of an UNCHANGED run created by sfh 0.x, which
/// predates two rules the current validator enforces:
/// - `access:` was optional and defaulted to write; the strict validator
///   rejects a step without it (rev_regression R-2),
/// - a string-form cmd containing a template was allowed; the strict validator
///   rejects the expansion unless unsafe_shell_template is set (rev_regression:
///   an unchanged old flow with `cmd: "echo {{steps.x.output}}"` could not be
///   resumed at all, and fixing the flow changed its fingerprint, so the
///   suggested --force-resume was the only way through).
///
/// `legacy` downgrades BOTH to warnings that restore the old behaviour. The
/// engine only uses this variant when the resumed run's meta.json records an
/// sfh 0.x version (rev_break #14 - the old code used it for ANY resume whose
/// strict load failed, so a crafted run dir could execute a flow a fresh run
/// rejects). The flow fingerprint check still runs afterwards, so a flow that
/// actually CHANGED is refused without --force-resume; this only lets an
/// unchanged legacy flow back in. A fresh run always goes through `load`.
pub fn load_lenient(path: &Path) -> Result<Flow, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut flow: Flow = yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    merge_global_profiles(&mut flow)?;
    validate(&flow, true)?;
    flow.legacy_resume = true;
    Ok(flow)
}

/// Merge machine-level profiles from ~/.sfh/profiles.yaml (a bare name->profile map).
/// Flow-level profiles win on name conflicts. This keeps flow files portable while
/// machine-specific things (bin: paths, provider/model choices) live outside the repo.
fn merge_global_profiles(flow: &mut Flow) -> Result<(), String> {
    let Some(p) = global_profiles_path() else {
        return Ok(());
    };
    let text = match std::fs::read_to_string(&p) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!("cannot read global profiles {}: {e}", p.display())),
    };
    let globals = yaml::from_str::<BTreeMap<String, Profile>>(&text)
        .map_err(|e| format!("{}: invalid global profiles: {e}", p.display()))?;
    for (k, v) in globals {
        flow.profiles.entry(k).or_insert(v);
    }
    Ok(())
}

pub fn global_profiles_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(
        std::path::PathBuf::from(home)
            .join(".sfh")
            .join("profiles.yaml"),
    )
}

impl Flow {
    /// Canonical execution-relevant representation after global profiles have
    /// been merged. Profiles that no step can select are omitted: changing an
    /// unrelated machine-local profile must not block resume of this flow.
    /// Inline profile edits remain covered by the separate raw-flow
    /// fingerprint, whether referenced or not.
    pub fn effective_config_json(&self) -> Result<String, String> {
        let mut value = serde_json::to_value(self)
            .map_err(|e| format!("cannot serialize effective flow configuration: {e}"))?;
        let referenced = self.referenced_profiles();
        if let Some(profiles) = value.get_mut("profiles").and_then(|v| v.as_object_mut()) {
            profiles.retain(|name, _| referenced.contains(name));
        }
        serde_json::to_string(&value)
            .map_err(|e| format!("cannot serialize effective flow configuration: {e}"))
    }

    /// Human-facing merged configuration. Unlike the fingerprint projection,
    /// this deliberately includes unused profiles so `sfh config show` can be
    /// used to diagnose the complete merge result.
    pub fn effective_config_json_pretty(&self, show_secrets: bool) -> Result<String, String> {
        fn redact_env(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    for (key, child) in object {
                        if key == "env" {
                            if let serde_json::Value::Object(env) = child {
                                for value in env.values_mut() {
                                    *value = serde_json::Value::String("<redacted>".to_string());
                                }
                            }
                        } else {
                            redact_env(child);
                        }
                    }
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        redact_env(value);
                    }
                }
                _ => {}
            }
        }

        let mut value = serde_json::to_value(self)
            .map_err(|e| format!("cannot serialize effective flow configuration: {e}"))?;
        if !show_secrets {
            redact_env(&mut value);
        }
        serde_json::to_string_pretty(&value)
            .map_err(|e| format!("cannot serialize effective flow configuration: {e}"))
    }

    fn referenced_profiles(&self) -> BTreeSet<String> {
        fn collect(step: &Step, out: &mut BTreeSet<String>) {
            if let Some(profile) = &step.use_ {
                out.insert(profile.clone());
            }
            out.extend(step.fallback.iter().cloned());
            if let Some(compact) = &step.compact {
                if let Some(profile) = &compact.use_ {
                    out.insert(profile.clone());
                }
            }
            if let Some(children) = &step.parallel {
                for child in children {
                    collect(child, out);
                }
            }
        }

        let mut profiles = BTreeSet::new();
        for step in &self.steps {
            collect(step, &mut profiles);
        }
        profiles
    }

    pub fn vars_string_map(&self) -> Result<BTreeMap<String, String>, String> {
        let mut out = BTreeMap::new();
        for (k, v) in &self.vars {
            let s = match v {
                yaml::Value::String(s) => s.clone(),
                yaml::Value::Number(n) => n.to_string(),
                yaml::Value::Bool(b) => b.to_string(),
                yaml::Value::Null => String::new(),
                _ => return Err(format!("var '{k}' must be a scalar")),
            };
            out.insert(k.clone(), s);
        }
        Ok(out)
    }

    /// The workspace decision, made from the flow's static shape alone.
    ///
    /// `auto` never reasons about what the work MEANS. It counts declared
    /// effects: a flow whose every step only reads needs no workspace of its
    /// own, and a flow with any potential writer gets exactly one managed
    /// worktree for the whole run - not one per step, not one per visit.
    pub fn workspace_plan(&self) -> Result<WorkspacePlan, String> {
        let cfg = self.workspace.as_ref();
        let declared = cfg.map(|c| c.mode()).unwrap_or(WorkspaceMode::Current);
        let mut warnings = Vec::new();
        let writers = self.potential_writers();
        let resolved = match declared {
            WorkspaceMode::Auto => {
                if writers.is_empty() {
                    // Nothing can write, so nothing needs isolating.
                    WorkspaceMode::Current
                } else {
                    WorkspaceMode::GitWorktree
                }
            }
            other => other,
        };
        // Two potential writers that can be in flight at once share one
        // workspace, and sfh has no way to reconcile what they each did. The
        // default is to refuse rather than to interleave them and hope.
        //
        // ONLY for a flow that opted into a workspace. A flow with no
        // `workspace:` key gets no workspace from sfh at all - every step runs
        // in the caller's cwd exactly as it did before v1.2 - so sfh has no
        // basis to start refusing flows that have always worked. Introducing a
        // new refusal for existing files is precisely what the compatibility
        // rule forbids.
        let concurrent = if cfg.is_some() {
            self.concurrent_writer_groups()
        } else {
            Vec::new()
        };
        let allow_concurrent = cfg.map(|c| c.allow_concurrent_writers()).unwrap_or(false);
        if !concurrent.is_empty() {
            if allow_concurrent {
                warnings.push(format!(
                    "workspace.allow_concurrent_writers is set: {} run potential writers concurrently in ONE workspace, and sfh cannot tell their changes apart",
                    concurrent.join(", ")
                ));
            } else {
                return Err(format!(
                    "step(s) {} fan out potential writers that would share one workspace at the same time. Declare `effects: read` on the members that only read, split the writers into sequential steps, or set `workspace.allow_concurrent_writers: true` to accept that sfh cannot separate their changes.",
                    concurrent.join(", ")
                ));
            }
        }
        if resolved == WorkspaceMode::Directory && cfg.and_then(|c| c.root.as_ref()).is_none() {
            return Err(
                "workspace.mode: directory needs workspace.root to say which directory".into(),
            );
        }
        if declared == WorkspaceMode::Auto && !writers.is_empty() {
            warnings.push(format!(
                "workspace.mode: auto resolved to one managed git worktree because {} may write",
                writers.join(", ")
            ));
        }
        Ok(WorkspacePlan {
            declared,
            resolved,
            root: cfg.and_then(|c| c.root.clone()),
            base: cfg.and_then(|c| c.base.clone()),
            cleanup: cfg.map(|c| c.cleanup()).unwrap_or_default(),
            verify_on_resume: cfg.map(|c| c.verify_on_resume()).unwrap_or(true),
            allow_concurrent_writers: allow_concurrent,
            needs_state_root: resolved == WorkspaceMode::GitWorktree,
            potential_writers: writers,
            warnings,
        })
    }

    /// What every step's context resolves to, without reading a single byte of
    /// a step's OUTPUT: only sources that are statically knowable are sized
    /// here, so `plan` and `preflight` stay side-effect free.
    pub fn context_plan(&self, flow_path: &Path) -> Result<ContextPlan, String> {
        let flow_dir = flow_path.parent().unwrap_or(Path::new("."));
        let mut plan = ContextPlan {
            max_context_chars: self.defaults.max_context_chars,
            ..Default::default()
        };
        for (name, source) in &self.contexts {
            let kind = source
                .kind()
                .map_err(|e| format!("contexts.{name}: {e}"))?
                .to_string();
            let (described, chars) = match kind.as_str() {
                "file" => {
                    let raw = source.file.clone().unwrap_or_default();
                    let resolved = crate::context::resolve_source_path(flow_dir, &raw);
                    let size = std::fs::metadata(&resolved).ok().map(|m| m.len());
                    (raw, size)
                }
                "inline" => (
                    "<inline>".to_string(),
                    source.inline.as_ref().map(|t| t.chars().count() as u64),
                ),
                // A template's size depends on run data that does not exist yet.
                _ => ("<template>".to_string(), None),
            };
            plan.sources.push((name.clone(), kind, described, chars));
        }
        let mut record = |s: &Step| -> Result<(), String> {
            for name in &s.context {
                if !self.contexts.contains_key(name) {
                    return Err(format!(
                        "step '{}' asks for context '{name}', which is not defined in contexts:",
                        s.id
                    ));
                }
            }
            if !s.context.is_empty() || s.context_delivery.is_some() {
                plan.steps
                    .push((s.id.clone(), s.context.clone(), s.context_delivery()));
            }
            Ok(())
        };
        for s in &self.steps {
            record(s)?;
            if let Some(children) = &s.parallel {
                for c in children {
                    record(c)?;
                }
            }
        }
        Ok(plan)
    }

    /// Step ids that may change the workspace or the world.
    pub fn potential_writers(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.steps {
            match &s.parallel {
                Some(children) => {
                    for c in children {
                        if c.effects(self).is_potential_writer() {
                            out.push(format!("{}.{}", s.id, c.id));
                        }
                    }
                }
                None => {
                    if s.effects(self).is_potential_writer() {
                        out.push(s.id.clone());
                    }
                }
            }
        }
        out
    }

    /// Fan-out steps that would put more than one potential writer in flight at
    /// once. A `foreach:` over a writing body counts whenever its concurrency
    /// ceiling is above one, because the item list is not known statically.
    fn concurrent_writer_groups(&self) -> Vec<String> {
        let cap = |s: &Step| {
            s.max_parallel
                .or(self.defaults.max_parallel)
                .unwrap_or(DEFAULT_MAX_PARALLEL)
        };
        let mut out = Vec::new();
        for s in &self.steps {
            if let Some(children) = &s.parallel {
                let writers = children
                    .iter()
                    .filter(|c| c.effects(self).is_potential_writer())
                    .count();
                if writers > 1 && cap(s) > 1 {
                    out.push(s.id.clone());
                }
            } else if s.foreach.is_some() && s.effects(self).is_potential_writer() && cap(s) > 1 {
                out.push(s.id.clone());
            }
        }
        out
    }

    /// A per-step view of the replay policy, plus the cases `validate --strict`
    /// and `plan` should point at: an effect that reaches outside the workspace
    /// (or is not declared at all) is the one where re-running after a crash is
    /// a real decision rather than a formality.
    pub fn replay_summary(&self) -> serde_json::Value {
        let mut steps = Vec::new();
        for s in &self.steps {
            let effects = s.effects(self);
            let policy = s.replay_policy(self);
            steps.push(serde_json::json!({
                "step": s.id,
                "effects": effects.as_str(),
                "unfinished": policy.as_str(),
                "risky": policy == ReplayPolicy::Rerun
                    && matches!(effects, Effects::External | Effects::Unknown),
            }));
        }
        serde_json::json!({
            "default": self
                .defaults
                .replay
                .and_then(|r| r.unfinished)
                .unwrap_or_default()
                .as_str(),
            "steps": steps,
        })
    }

    /// Warnings `validate --strict` emits about replay choices. Each names a
    /// step whose unfinished work would be re-run even though the flow says it
    /// may have already reached outside the workspace.
    pub fn replay_warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.steps {
            let effects = s.effects(self);
            if s.replay_policy(self) == ReplayPolicy::Rerun
                && matches!(effects, Effects::External | Effects::Unknown)
            {
                out.push(format!(
                    "step '{}' declares effects: {} but keeps replay.unfinished: rerun, so a resume after a crash mid-step will run it again without knowing whether its effects already happened. Set replay.unfinished: stuck (or fail) if that is not safe.",
                    s.id,
                    effects.as_str()
                ));
            }
        }
        out
    }

    /// Every explicitly accepted risk in this flow, by name. Recorded in the
    /// plan, the execution closure and the run's meta so "we turned that off on
    /// purpose" is visible after the fact.
    pub fn unsafe_overrides(&self) -> Vec<String> {
        let mut out = BTreeSet::new();
        if self
            .workspace
            .as_ref()
            .map(|w| w.allow_concurrent_writers())
            .unwrap_or(false)
        {
            out.insert("workspace.allow_concurrent_writers".to_string());
        }
        for (name, c) in &self.contexts {
            if c.allow_external.unwrap_or(false) {
                out.insert(format!("contexts.{name}.allow_external"));
            }
        }
        if self.defaults.exit_conflict == Some(ExitConflict::TrustProtocol) {
            out.insert(format!(
                "defaults.exit_conflict={}",
                ExitConflict::TrustProtocol.as_str()
            ));
        }
        let mut note = |s: &Step| {
            // Believing a protocol over a non-zero exit is narrow and evidence-
            // gated, but it is still sfh overriding what the OS reported. It
            // belongs in the same list a reviewer reads before a paid run.
            if s.exit_conflict == Some(ExitConflict::TrustProtocol) {
                out.insert(format!(
                    "steps.{}.exit_conflict={}",
                    s.id,
                    ExitConflict::TrustProtocol.as_str()
                ));
            }
            if s.unsafe_shell_template.unwrap_or(false) {
                out.insert(format!("steps.{}.unsafe_shell_template", s.id));
            }
            if s.allow_dynamic_exec_paths.unwrap_or(false) {
                out.insert(format!("steps.{}.allow_dynamic_exec_paths", s.id));
            }
            if s.allow_access_override.unwrap_or(false) {
                out.insert(format!("steps.{}.allow_access_override", s.id));
            }
        };
        for s in &self.steps {
            note(s);
            if let Some(children) = &s.parallel {
                for c in children {
                    note(c);
                }
            }
        }
        out.into_iter().collect()
    }

    /// Every program a `cmd:` step would launch, mapped to the steps that
    /// launch it.
    ///
    /// `resolved_tools` deliberately skips `cmd:` steps, because tool/fallback
    /// resolution is a preset concept. That left the programs a flow leans on
    /// hardest - the verification shell, the build, the test runner - as the
    /// only ones preflight never looked at, so a `bash` that resolved to
    /// something entirely different was reported as "no blockers".
    ///
    /// argv[0] carrying a `{{...}}` placeholder is not resolvable before the
    /// run, and is reported as such rather than guessed at.
    pub fn resolved_commands(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut note = |s: &Step| {
            let program = match &s.cmd {
                Some(Cmd::Argv(argv)) => argv.first().cloned(),
                // A `cmd:` string is handed to the platform shell, never to a
                // shell the flow chose. Naming it is the point: a flow that
                // writes bash syntax into a `cmd:` string is running it under
                // `sh` (or `cmd`), and that is worth seeing before the run.
                Some(Cmd::Shell(_)) => Some(if cfg!(windows) { "cmd" } else { "sh" }.to_string()),
                None => None,
            };
            if let Some(program) = program {
                out.entry(program).or_default().insert(s.id.clone());
            }
        };
        for s in &self.steps {
            note(s);
            if let Some(children) = &s.parallel {
                for c in children {
                    note(c);
                }
            }
        }
        out
    }

    /// An upper bound on leaves this flow can execute, from its static shape.
    ///
    /// Deliberately a BOUND, not a prediction: a `foreach:` list is data, not
    /// structure, so its item count is unknown here and is reported as such by
    /// the `foreach_unbounded` flag rather than guessed at.
    pub fn static_max_leaves(&self) -> serde_json::Value {
        let visits = self.defaults.max_visits.unwrap_or(DEFAULT_MAX_VISITS);
        let mut leaves_per_visit = 0u64;
        let mut unbounded = Vec::new();
        for s in &self.steps {
            let per = match (&s.parallel, &s.foreach) {
                (Some(children), _) => children.len() as u64,
                (None, Some(_)) => {
                    unbounded.push(s.id.clone());
                    1
                }
                _ => 1,
            };
            // Each step may be revisited up to its own ceiling.
            let step_visits = s.max_visits.unwrap_or(visits) as u64;
            leaves_per_visit = leaves_per_visit.saturating_add(per.saturating_mul(step_visits));
        }
        serde_json::json!({
            "max_leaves": self
                .defaults
                .max_total_steps
                .map(u64::from)
                .unwrap_or(leaves_per_visit)
                .min(leaves_per_visit),
            "bounded_by_max_total_steps": self.defaults.max_total_steps,
            "foreach_unbounded": unbounded,
        })
    }

    /// All ids addressable from templates: top-level steps plus parallel children.
    pub fn step_ids(&self) -> HashSet<String> {
        let mut ids = HashSet::new();
        for s in &self.steps {
            ids.insert(s.id.clone());
            if let Some(children) = &s.parallel {
                for c in children {
                    ids.insert(c.id.clone());
                }
            }
        }
        ids
    }

    /// Top-level ids only (valid goto targets).
    pub fn top_ids(&self) -> HashSet<String> {
        self.steps.iter().map(|s| s.id.clone()).collect()
    }

    pub fn find_step(&self, id: &str) -> Option<&Step> {
        for s in &self.steps {
            if s.id == id {
                return Some(s);
            }
            if let Some(children) = &s.parallel {
                for c in children {
                    if c.id == id {
                        return Some(c);
                    }
                }
            }
        }
        None
    }

    fn step_tool(&self, s: &Step) -> Option<String> {
        if s.cmd.is_some() {
            return None;
        }
        s.tool
            .clone()
            .or_else(|| {
                s.use_
                    .as_ref()
                    .and_then(|u| self.profiles.get(u))
                    .and_then(|p| p.tool.clone())
            })
            .or_else(|| self.defaults.tool.clone())
    }

    /// Every (tool, bin, model, effort) tuple this flow can actually launch,
    /// resolved exactly the way the engine resolves a step (step > profile
    /// (use:) > defaults), plus fallback profiles and compact summarizers.
    /// Profiles no step references never appear here, so their bins are never
    /// version-probed and never launched: a `profiles:` entry is configuration
    /// data, not an instruction to run anything.
    pub fn resolved_tools(&self) -> BTreeSet<ResolvedTool> {
        let mut out = BTreeSet::new();
        let mut note = |s: &Step| {
            // A step with cmd: launches only its command; tool/fallback
            // resolution applies to preset steps. A compact: summarizer is a
            // real preset launch even on a cmd: step, so it is collected
            // either way.
            if s.cmd.is_none() {
                if let Ok(e) = crate::leaf::effective(self, s) {
                    if let Some(tool) = e.tool {
                        out.insert(ResolvedTool {
                            tool,
                            bin: e.bin,
                            model: e.model,
                            effort: e.effort,
                            access: vec![e.access.as_str().to_string()],
                        });
                    }
                }
                for fb in &s.fallback {
                    if let Ok(e) = crate::leaf::effective_with(self, s, Some(fb)) {
                        if let Some(tool) = e.tool {
                            out.insert(ResolvedTool {
                                tool,
                                bin: e.bin,
                                model: e.model,
                                effort: e.effort,
                                access: vec![e.access.as_str().to_string()],
                            });
                        }
                    }
                }
            }
            if let Some(c) = &s.compact {
                // Same merge run_compact does: compact fields win over its
                // profile; step/defaults are not consulted for the summarizer.
                let prof = c.use_.as_ref().and_then(|u| self.profiles.get(u));
                if let Some(tool) = c.tool.clone().or_else(|| prof.and_then(|p| p.tool.clone())) {
                    out.insert(ResolvedTool {
                        bin: c.bin.clone().or_else(|| prof.and_then(|p| p.bin.clone())),
                        model: c
                            .model
                            .clone()
                            .or_else(|| prof.and_then(|p| p.model.clone())),
                        effort: c
                            .effort
                            .clone()
                            .or_else(|| prof.and_then(|p| p.effort.clone())),
                        tool,
                        // A compact summarizer only reads the text it was
                        // handed; it never runs at a step's access level.
                        access: vec![crate::preset::Access::Read.as_str().to_string()],
                    });
                }
            }
        };
        for s in &self.steps {
            note(s);
            if let Some(children) = &s.parallel {
                for c in children {
                    note(c);
                }
            }
        }
        out
    }
}

impl Step {
    pub fn is_group(&self) -> bool {
        self.parallel.is_some()
    }
    pub fn is_foreach(&self) -> bool {
        self.foreach.is_some()
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
}

/// The flow name becomes part of the run directory name, so the ONLY forbidden
/// characters are ones that would change or break that path: path separators,
/// NUL and other control characters, the characters Windows cannot use in a
/// name, and the reserved names "." / "..". Unicode, spaces and dots are fine
/// ("研究 2026.07" is a perfectly good directory name on all three platforms).
/// Lives here (not in the engine) so `sfh validate` and `sfh run` agree.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("flow name must not be empty".into());
    }
    if name == "." || name == ".." {
        return Err(format!(
            "flow name '{name}' is a reserved directory name (it would escape the runs root)"
        ));
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err(format!(
            "flow name '{name}' must not end with a space or a dot (Windows strips them from directory names, which would silently move the run dir)"
        ));
    }
    for c in name.chars() {
        let forbidden = c == '/'
            || c == '\\'
            || c == '\0'
            || (c as u32) < 0x20
            || c as u32 == 0x7f
            || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*');
        if forbidden {
            return Err(format!(
                "flow name '{name}' cannot be used as a directory name: only path separators, control characters and <>:\"|?* are forbidden (spaces, dots and Unicode are fine)"
            ));
        }
    }
    Ok(())
}

fn validate(flow: &Flow, legacy: bool) -> Result<(), String> {
    if let Some(version) = flow.api_version {
        if version != 1 {
            return Err(format!(
                "unsupported api_version {version}; this sfh supports api_version: 1"
            ));
        }
    }
    if flow.steps.is_empty() {
        return Err("flow has no steps".into());
    }
    if let Some(n) = &flow.name {
        validate_name(n)?;
    }
    let mut seen = HashSet::new();
    // Compared ignoring case for the same reason ids are: terminal names and
    // artifact names must have one meaning on every supported filesystem.
    let reserved_id = |id: &str| -> Result<(), String> {
        if TERMINALS.iter().any(|t| id.eq_ignore_ascii_case(t)) {
            return Err(format!(
                "step id '{id}' is reserved: end/fail/stuck are terminal goto targets, so they cannot also name steps (compared ignoring case). Rename the step"
            ));
        }
        Ok(())
    };
    for s in &flow.steps {
        if !valid_id(&s.id) {
            return Err(format!(
                "step id '{}' must be non-empty and use only [A-Za-z0-9_-]",
                s.id
            ));
        }
        reserved_id(&s.id)?;
        // Case-INSENSITIVELY unique. Step ids become artifact file names, and
        // `A` and `a` are the same file on Windows and on a stock macOS while
        // being two files on Linux - so the same flow restores different
        // outputs per platform, and two parallel members ids apart only by case
        // race for one file. Rejecting the pair makes the failure the same
        // everywhere, which is the only answer that keeps a flow portable.
        if !seen.insert(s.id.to_lowercase()) {
            return Err(format!(
                "duplicate step id '{}' (ids are compared ignoring case: they become file names, and two ids differing only in case are one file on Windows and macOS)",
                s.id
            ));
        }
        if let Some(children) = &s.parallel {
            for c in children {
                if !valid_id(&c.id) {
                    return Err(format!(
                        "step id '{}' must be non-empty and use only [A-Za-z0-9_-]",
                        c.id
                    ));
                }
                reserved_id(&c.id)?;
                if !seen.insert(c.id.to_lowercase()) {
                    return Err(format!(
                        "duplicate step id '{}' (ids are compared ignoring case: they become file names, and two ids differing only in case are one file on Windows and macOS)",
                        c.id
                    ));
                }
            }
        }
    }
    let top_ids = flow.top_ids();
    let check_goto = |ctx: &str, g: &str| -> Result<(), String> {
        if TERMINALS.contains(&g) || top_ids.contains(g) {
            Ok(())
        } else {
            Err(format!(
                "{ctx}: goto target '{g}' is not a top-level step id (or end/fail/stuck)"
            ))
        }
    };
    let mut profile_names = HashSet::new();
    for (name, p) in &flow.profiles {
        if !valid_id(name) {
            return Err(format!("profile name '{name}' must use only [A-Za-z0-9_-]"));
        }
        if !profile_names.insert(name.to_ascii_lowercase()) {
            return Err(format!(
                "duplicate profile name '{name}' (profile names are compared ignoring case because fallback artifact names must not collide on Windows or macOS)"
            ));
        }
        if let Some(t) = &p.tool {
            if !TOOLS.contains(&t.as_str()) {
                return Err(format!("profile '{name}': unknown tool '{t}'"));
            }
        }
        if let Some(a) = &p.access {
            if !["read", "write", "full"].contains(&a.as_str()) {
                return Err(format!("profile '{name}': access must be read/write/full"));
            }
        }
        positive_u64(&format!("profile '{name}'.timeout_sec"), p.timeout_sec)?;
    }
    if let Some(t) = &flow.defaults.tool {
        if !TOOLS.contains(&t.as_str()) {
            return Err(format!(
                "defaults.tool: unknown tool '{t}' (use one of {})",
                TOOLS.join("/")
            ));
        }
    }
    if let Some(a) = &flow.defaults.access {
        if !["read", "write", "full"].contains(&a.as_str()) {
            return Err(format!(
                "defaults.access must be read/write/full, got '{a}'"
            ));
        }
    }
    positive_u64("defaults.timeout_sec", flow.defaults.timeout_sec)?;
    positive_u32("defaults.max_visits", flow.defaults.max_visits)?;
    positive_u32("defaults.max_total_steps", flow.defaults.max_total_steps)?;
    positive_u32("defaults.max_parallel", flow.defaults.max_parallel)?;
    positive_u64("defaults.max_prompt_chars", flow.defaults.max_prompt_chars)?;
    positive_u64("defaults.max_emit_chars", flow.defaults.max_emit_chars)?;
    positive_u64("defaults.wall_clock_sec", flow.defaults.wall_clock_sec)?;
    if let Some(cost) = flow.defaults.max_cost_usd {
        if !(cost.is_finite() && cost >= 0.0) {
            return Err(format!(
                "defaults.max_cost_usd must be a finite number >= 0 (got {cost})"
            ));
        }
    }
    for (tool, limit) in &flow.defaults.tool_max_parallel {
        if !TOOLS.contains(&tool.as_str()) {
            return Err(format!(
                "defaults.tool_max_parallel.{tool}: unknown preset tool"
            ));
        }
        if *limit == 0 {
            return Err(format!(
                "defaults.tool_max_parallel.{tool} must be >= 1 (0 would block that tool forever)"
            ));
        }
    }
    if let Some(r) = &flow.defaults.retry_on {
        check_retry_on("defaults", r)?;
    }
    if let Some(w) = &flow.defaults.fork_warmup {
        if !["auto", "always", "never"].contains(&w.as_str()) {
            return Err("defaults.fork_warmup must be auto/always/never".into());
        }
    }
    // on_budget and budget_reserve are two halves of one mechanism and are
    // useless apart, so neither is accepted alone: a reserve with nowhere to
    // land is a number that does nothing, and a landing with no ceiling above
    // it can never fire. Both would be silent, which is exactly the failure
    // mode a budget guard must not have.
    if flow.defaults.on_budget.is_none() && flow.defaults.budget_reserve.is_some() {
        return Err(
            "defaults.budget_reserve is set but defaults.on_budget is not: the reserve only decides how EARLY the landing fires, so on its own it changes nothing. Add on_budget: goto:<id>"
                .into(),
        );
    }
    if let Some(action) = &flow.defaults.on_budget {
        if flow.defaults.max_cost_usd.is_none() && flow.defaults.wall_clock_sec.is_none() {
            return Err(
                "defaults.on_budget is set but neither defaults.max_cost_usd nor defaults.wall_clock_sec is: the landing threshold is ceiling minus reserve, and with no ceiling there is nothing to land before"
                    .into(),
            );
        }
        let target = action.strip_prefix("goto:").ok_or_else(|| {
            format!("defaults.on_budget must be written as goto:<id> (got '{action}'); end, fail and stuck are allowed targets")
        })?;
        check_goto("defaults.on_budget", target)?;
    }
    if let Some(r) = &flow.defaults.budget_reserve {
        // A negative or NaN reserve would push the landing threshold ABOVE the
        // ceiling, so the landing could never fire and the flow would quietly
        // get the old hard error it was written to avoid.
        if let Some(c) = r.cost_usd {
            if !(c.is_finite() && c >= 0.0) {
                return Err(format!(
                    "defaults.budget_reserve.cost_usd must be a finite number >= 0 (got {c})"
                ));
            }
        }
    }
    // A reserve of ZERO on an axis that has a ceiling is the same failure in
    // slower motion. The threshold is ceiling minus reserve, so it lands exactly
    // ON the ceiling: the landing fires, writes its event, prints "-> goto
    // wrap", and the ceiling check at the top of the very next iteration ends
    // the run with the old hard error before the landing chain has run a single
    // step. The flow validates, dry-run advertises the landing, and the feature
    // does nothing - which is the silence the two checks above exist to prevent,
    // reached by a third route. Refused per axis, because the axes are
    // independent: reserving cost does not buy the wall-clock landing a second.
    if flow.defaults.on_budget.is_some() {
        for (ceiling, key, unreserved) in [
            (
                "max_cost_usd",
                "budget_reserve.cost_usd",
                flow.defaults.max_cost_usd.is_some() && flow.defaults.budget_reserve_usd() <= 0.0,
            ),
            (
                "wall_clock_sec",
                "budget_reserve.wall_clock_sec",
                flow.defaults.wall_clock_sec.is_some() && flow.defaults.budget_reserve_sec() == 0,
            ),
        ] {
            if unreserved {
                return Err(format!(
                    "defaults.on_budget is set and defaults.{ceiling} holds nothing back: the landing threshold is {ceiling} minus reserve, so a reserve of 0 lands ON the ceiling and the run still dies there with the landing chain unrun. Set defaults.{key} to at least the longest single step plus the whole landing chain"
                ));
            }
        }
    }
    let action = |what: &str, s: &Step, v: &Option<String>, is_child: bool| -> Result<(), String> {
        let Some(oe) = v else { return Ok(()) };
        if let Some(g) = oe.strip_prefix("goto:") {
            // is_child FIRST, terminals included. A member's on_error is only
            // ever asked whether it says "continue"; every other spelling makes
            // the group hard-fail and the run exit 1. Accepting `goto:stuck`
            // here and then ignoring it handed back exit 1 ("the plumbing broke,
            // retry") where the author had asked for exit 4 ("work saved, a
            // human should look") - the exact confusion the third terminal was
            // added to end. `goto:end` and `goto:fail` were quietly inert the
            // same way.
            if is_child {
                return Err(format!(
                    "step '{}': {what} goto is not allowed inside parallel: (use fail or continue; the group's own {what} decides where the run goes)",
                    s.id
                ));
            }
            if TERMINALS.contains(&g) {
                return Ok(());
            }
            return check_goto(&format!("step '{}' {what}", s.id), g);
        }
        if oe != "fail" && oe != "continue" {
            return Err(format!(
                "step '{}': {what} must be fail/continue/goto:<id>",
                s.id
            ));
        }
        Ok(())
    };
    // continue_from and fork_from share every target rule; they differ only in
    // whether siblings may aim at the same target (fork: yes - that is the point).
    let check_ref = |s: &Step, what: &str, id: &Option<String>| -> Result<(), String> {
        let Some(cf) = id else {
            return Ok(());
        };
        if cf == &s.id {
            return Err(format!("step '{}': {what} cannot reference itself", s.id));
        }
        let target = flow
            .find_step(cf)
            .ok_or_else(|| format!("step '{}': {what} target '{cf}' is not a step id", s.id))?;
        if target.is_group() {
            return Err(format!(
                "step '{}': {what} '{cf}' targets a parallel group (sessions belong to its children; target one of them)",
                s.id
            ));
        }
        if target.is_foreach() {
            return Err(format!(
                "step '{}': {what} '{cf}' targets a foreach step (it runs many sessions; there is no single one to reuse)",
                s.id
            ));
        }
        if target.cmd.is_some() {
            return Err(format!(
                "step '{}': {what} '{cf}' targets a cmd: step, which has no session",
                s.id
            ));
        }
        Ok(())
    };
    let check_continue_from = |s: &Step| -> Result<(), String> {
        if s.continue_from.is_some() && s.fork_from.is_some() {
            return Err(format!(
                "step '{}': continue_from and fork_from are mutually exclusive (continue = the same session, fork = a branch of it)",
                s.id
            ));
        }
        check_ref(s, "continue_from", &s.continue_from)?;
        check_ref(s, "fork_from", &s.fork_from)
    };
    for s in &flow.steps {
        validate_step(flow, s, false, legacy)?;
        action("on_error", s, &s.on_error, false)?;
        action("on_max_visits", s, &s.on_max_visits, false)?;
        check_continue_from(s)?;
        if let Some(children) = &s.parallel {
            if children.is_empty() {
                return Err(format!(
                    "step '{}': parallel: needs at least one child",
                    s.id
                ));
            }
            let child_ids: HashSet<&String> = children.iter().map(|c| &c.id).collect();
            let mut targets_seen: std::collections::HashMap<&String, &String> =
                std::collections::HashMap::new();
            for c in children {
                validate_step(flow, c, true, legacy)?;
                action("on_error", c, &c.on_error, true)?;
                check_continue_from(c)?;
                for (what, target) in [
                    ("continue_from", &c.continue_from),
                    ("fork_from", &c.fork_from),
                ] {
                    let Some(cf) = target else { continue };
                    if child_ids.contains(cf) {
                        return Err(format!(
                            "step '{}': {what} '{cf}' targets a sibling in the same parallel group (it has not run yet)",
                            c.id
                        ));
                    }
                }
                // Two children resuming ONE session would interleave writes into
                // it; two children FORKING one session is exactly the point.
                if let Some(cf) = &c.continue_from {
                    if let Some(prev) = targets_seen.insert(cf, &c.id) {
                        return Err(format!(
                            "steps '{prev}' and '{}' both continue_from '{cf}' inside one parallel group - two concurrent resumes of the same session (use fork_from to branch instead)",
                            c.id
                        ));
                    }
                }
            }
        }
        check_outcomes(s)?;
        for (i, r) in s.route.iter().enumerate() {
            check_goto(&format!("step '{}' route[{i}]", s.id), &r.goto)?;
            check_when_members(s, i, r)?;
            // A rule that can never match is not a smaller version of one that
            // can - it is a branch the author believes in and will never take.
            // Catching it here costs nothing; catching it at run time costs
            // whatever the guarded step was about to spend.
            // A fan-out step's own recorded result is the GROUP composite: one
            // exit for the whole lap, and no label, because the group ran no
            // command. An `outcomes:` table on a foreach describes each ITEM's
            // launch, which is useful and stays allowed - but a label rule
            // reading the group would simply never match. Members are judged
            // with when_members.
            if (s.is_group() || s.is_foreach())
                && (r.when_label_is.is_some() || r.when_outcome_is.is_some())
            {
                return Err(format!(
                    "step '{}' route[{i}]: when_label_is/when_outcome_is read this step's OWN outcome, and a parallel:/foreach: step has none - it records the group composite. Judge the members with when_members, or put the rule on a single step",
                    s.id
                ));
            }
            if let Some(want) = &r.when_label_is {
                if !s
                    .outcomes
                    .values()
                    .any(|o| o.label.as_deref() == Some(want))
                    && !want.contains("{{")
                {
                    return Err(format!(
                        "step '{}' route[{i}]: when_label_is '{want}' can never match - no outcomes: entry on this step carries that label",
                        s.id
                    ));
                }
            }
            if let Some(want) = r.when_outcome_is {
                if !s.outcomes.values().any(|o| o.result == want) {
                    return Err(format!(
                        "step '{}' route[{i}]: when_outcome_is '{}' can never match - no outcomes: entry on this step declares that result",
                        s.id,
                        want.as_str()
                    ));
                }
            }
            for rx in [
                &r.when_matches,
                &r.when_last_line_matches,
                &r.when_stderr_matches,
            ]
            .into_iter()
            .flatten()
            {
                if !rx.contains("{{") {
                    regex::Regex::new(rx)
                        .map_err(|e| format!("step '{}' route[{i}]: bad regex: {e}", s.id))?;
                }
            }
        }
        if let Some(catch_all) = s.route.iter().position(Route::is_catch_all) {
            if catch_all + 1 != s.route.len() {
                return Err(format!(
                    "step '{}': route[{catch_all}] is unconditional, so the {} rule(s) after it can never match; put the catch-all last",
                    s.id,
                    s.route.len() - catch_all - 1
                ));
            }
        }
        for left in 0..s.route.len() {
            let Some(a) = &s.route[left].when_last_line_contains else {
                continue;
            };
            for right in left + 1..s.route.len() {
                let Some(b) = &s.route[right].when_last_line_contains else {
                    continue;
                };
                if !a.contains(b) && !b.contains(a) {
                    continue;
                }
                return Err(format!(
                    "step '{}' route[{left}] and route[{right}]: when_last_line_contains phrases {a:?} and {b:?} overlap (one is a substring of the other), so rule order can silently select the wrong branch. Use when_last_line_is for exact verdicts, or choose non-overlapping phrases",
                    s.id
                ));
            }
        }
    }
    validate_session_dominance(flow)?;
    Ok(())
}

fn positive_u64(ctx: &str, value: Option<u64>) -> Result<(), String> {
    if value == Some(0) {
        Err(format!("{ctx} must be >= 1"))
    } else {
        Ok(())
    }
}

fn positive_u32(ctx: &str, value: Option<u32>) -> Result<(), String> {
    if value == Some(0) {
        Err(format!("{ctx} must be >= 1"))
    } else {
        Ok(())
    }
}

/// Everything about an `outcomes:` table that can be settled without running
/// anything. All of it fails the flow: an exit-code table is the one place a
/// typo silently changes what "done" means, and a run is exactly the wrong
/// moment to discover that.
fn check_outcomes(s: &Step) -> Result<(), String> {
    for (code, o) in &s.outcomes {
        let at = format!("step '{}' outcomes[{code}]", s.id);
        if let Some(label) = &o.label {
            if label.trim().is_empty() {
                return Err(format!(
                    "{at}: label is empty - omit it, or give it a name a route can match"
                ));
            }
            if label.contains('\n') || label.contains('\r') {
                return Err(format!(
                    "{at}: label must be a single line, because it is compared for equality and recorded as one field"
                ));
            }
        }
        // Retrying a success is not a policy, it is a contradiction: the step
        // would be re-run precisely because it worked.
        if *code == 0 && o.result == OutcomeResult::Retryable {
            return Err(format!(
                "{at}: exit 0 cannot be retryable - a retry answers a failure, and this says the step succeeded"
            ));
        }
        // sfh kills a step it decided to stop; -1 is its own marker for "no
        // process produced this", not something a command can exit with.
        if *code < 0 {
            return Err(format!(
                "{at}: exit codes below 0 are sfh's own markers for a step that never ran, so a flow cannot declare what they mean"
            ));
        }
    }
    Ok(())
}

/// Everything about a `when_members` rule that can be settled without running
/// anything. All of it fails the flow rather than warning: a route rule that
/// cannot match is not a smaller version of one that can, it is a branch the
/// author believes in and will never take.
fn check_when_members(s: &Step, i: usize, r: &Route) -> Result<(), String> {
    let Some(wm) = &r.when_members else {
        return Ok(());
    };
    let at = format!("step '{}' route[{i}]", s.id);
    if s.parallel.is_none() && s.foreach.is_none() {
        return Err(format!(
            "{at}: when_members counts the members of a fan-out, so it needs parallel: or foreach: on the same step. For a single step's own verdict use when_last_line_is"
        ));
    }
    // No AND with the other predicates. The text ones read the members' output
    // glued together, when_members reads each member's record separately, and a
    // rule that mixed them would answer two different questions about two
    // different texts under one goto - with no way to see which half failed.
    // when_exit / when_stderr_matches are excluded for the same reason at a
    // different granularity: on a fan-out step they judge the GROUP (the
    // composite exit, and a stderr file the group does not even have), never the
    // members being counted here.
    for (name, present) in [
        ("when_contains", r.when_contains.is_some()),
        ("when_matches", r.when_matches.is_some()),
        (
            "when_last_line_contains",
            r.when_last_line_contains.is_some(),
        ),
        ("when_last_line_is", r.when_last_line_is.is_some()),
        ("when_last_line_matches", r.when_last_line_matches.is_some()),
        ("when_exit", r.when_exit.is_some()),
        ("when_stderr_matches", r.when_stderr_matches.is_some()),
        ("when_label_is", r.when_label_is.is_some()),
        ("when_outcome_is", r.when_outcome_is.is_some()),
    ] {
        if present {
            return Err(format!(
                "{at}: when_members cannot share a rule with {name} - one counts members, the other judges the group as a whole. Give each its own rule"
            ));
        }
    }
    match (wm.at_least, wm.all) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "{at}: when_members takes at_least: <n> or all: true, not both"
            ))
        }
        (None, None) => {
            return Err(format!(
                "{at}: when_members needs a quantifier - at_least: <n> or all: true"
            ))
        }
        (Some(0), None) => {
            return Err(format!(
                "{at}: when_members at_least must be 1 or more - at_least: 0 would match a fan-out in which nobody agreed"
            ))
        }
        // Nothing sensible to do with it: the only meaning would be "match when
        // not all agreed", which is what the catch-all rule after it already is.
        (None, Some(false)) => {
            return Err(format!(
                "{at}: when_members all: false can never match - use all: true, or let the next rule catch the disagreement"
            ))
        }
        _ => {}
    }
    // A parallel group's size is written in the flow, so asking for more votes
    // than it can ever cast is a typo the author should hear about now. A
    // foreach's size is only known once the previous step has spoken, so the
    // same mistake there can only fail closed at run time (documented).
    if let (Some(n), Some(children)) = (wm.at_least, &s.parallel) {
        if n as usize > children.len() {
            return Err(format!(
                "{at}: when_members at_least is {n} but the parallel group has {} members, so the rule can never match",
                children.len()
            ));
        }
    }
    // The recorded verdict line is cut to this length, and the comparison uses
    // the cut form so that a live decision and a resumed one cannot disagree.
    // A longer needle therefore could not match anything; say so now instead of
    // quietly never firing. A templated one is only known at run time (also
    // documented).
    if !wm.last_line_is.contains("{{")
        && wm.last_line_is.chars().count() > crate::engine::ROUTE_LINE_CHARS
    {
        return Err(format!(
            "{at}: when_members last_line_is is longer than {} characters, which is where the recorded verdict line is cut, so it could never match",
            crate::engine::ROUTE_LINE_CHARS
        ));
    }
    Ok(())
}

/// Warnings that matter when a flow is executed, rather than only under
/// `validate --strict`. Callers decide whether their output mode should show
/// them; validation itself stays side-effect free so `run -q` can actually be
/// quiet and machine-readable commands are not polluted on stderr.
pub fn runtime_warnings(flow: &Flow) -> Vec<String> {
    let indices: std::collections::HashMap<&str, usize> = flow
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| (step.id.as_str(), index))
        .collect();
    let mut warnings = Vec::new();

    for source in &flow.steps {
        let mut targets: Vec<usize> = source
            .route
            .iter()
            .filter_map(|route| indices.get(route.goto.as_str()).copied())
            .collect();
        targets.sort_unstable();
        targets.dedup();
        if targets.len() < 2
            || !targets
                .windows(2)
                .all(|pair| pair[1] == pair[0].saturating_add(1))
        {
            continue;
        }

        for pair in targets.windows(2) {
            let branch = &flow.steps[pair[0]];
            let next = &flow.steps[pair[1]];
            let has_error_goto = branch
                .on_error
                .as_deref()
                .is_some_and(|action| action.starts_with("goto:"));
            if branch.route.is_empty() && !has_error_goto {
                warnings.push(format!(
                    "step '{}' is a consecutive branch destination of step '{}' but has neither route: nor on_error: goto:, so successful execution falls through to step '{}'. Add an explicit route (for example route: [{{goto: end}}]) if this branch should terminate",
                    branch.id, source.id, next.id
                ));
            }
        }
    }

    warnings
}

pub fn strict_warnings(flow: &Flow) -> Vec<String> {
    let mut warnings = runtime_warnings(flow);
    if flow.api_version.is_none() {
        warnings.push(
            "api_version is omitted; add `api_version: 1` so future format migrations are explicit"
                .into(),
        );
    }
    for step in &flow.steps {
        if !step.route.is_empty() && !step.route.iter().any(Route::is_catch_all) {
            warnings.push(format!(
                "step '{}': route has no catch-all and therefore falls through implicitly when no condition matches; add an explicit final goto",
                step.id
            ));
        }
    }

    let n = flow.steps.len();
    let index: HashMap<&str, usize> = flow
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let mut edges: Vec<HashSet<usize>> = (0..n).map(|_| HashSet::new()).collect();
    for (i, step) in flow.steps.iter().enumerate() {
        for route in &step.route {
            if let Some(&to) = index.get(route.goto.as_str()) {
                edges[i].insert(to);
            }
        }
        if !step.route.iter().any(Route::is_catch_all) && i + 1 < n {
            edges[i].insert(i + 1);
        }
        for action in [step.on_error.as_deref(), step.on_max_visits.as_deref()] {
            if let Some(target) = action.and_then(|a| a.strip_prefix("goto:")) {
                if let Some(&to) = index.get(target) {
                    edges[i].insert(to);
                }
            }
        }
        if step.on_error.as_deref() == Some("continue") && i + 1 < n {
            edges[i].insert(i + 1);
        }
        if step.on_max_visits.as_deref() == Some("continue") && i + 1 < n {
            edges[i].insert(i + 1);
        }
    }
    let mut reachable = HashSet::new();
    let mut stack = Vec::new();
    if n > 0 {
        reachable.insert(0);
        stack.push(0);
    }
    if let Some(target) = flow.defaults.budget_goto() {
        if let Some(&to) = index.get(target) {
            if reachable.insert(to) {
                stack.push(to);
            }
        }
    }
    while let Some(node) = stack.pop() {
        for &next in &edges[node] {
            if reachable.insert(next) {
                stack.push(next);
            }
        }
    }
    for (i, step) in flow.steps.iter().enumerate() {
        if !reachable.contains(&i) {
            warnings.push(format!(
                "step '{}' is unreachable from the flow entry, routes, error actions and on_budget",
                step.id
            ));
        }
    }
    warnings.sort();
    warnings.dedup();
    warnings
}

/// Prove that every session source is guaranteed to have executed before its
/// consumer. Existence checks alone accept forward references and branch joins
/// that can reach a consumer without ever creating the requested session.
fn validate_session_dominance(flow: &Flow) -> Result<(), String> {
    let n = flow.steps.len();
    let entry = n;
    let top_index: HashMap<&str, usize> = flow
        .steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    let mut owner: HashMap<&str, usize> = HashMap::new();
    let mut source: HashMap<&str, &Step> = HashMap::new();
    for (i, step) in flow.steps.iter().enumerate() {
        owner.insert(step.id.as_str(), i);
        source.insert(step.id.as_str(), step);
        if let Some(children) = &step.parallel {
            for child in children {
                owner.insert(child.id.as_str(), i);
                source.insert(child.id.as_str(), child);
            }
        }
    }

    let mut edges: Vec<HashSet<usize>> = (0..=n).map(|_| HashSet::new()).collect();
    if n > 0 {
        edges[entry].insert(0);
    }
    if let Some(target) = flow.defaults.budget_goto() {
        if let Some(&idx) = top_index.get(target) {
            // on_budget may land before the normal predecessor has executed.
            edges[entry].insert(idx);
        }
    }
    for (i, step) in flow.steps.iter().enumerate() {
        for route in &step.route {
            if let Some(&to) = top_index.get(route.goto.as_str()) {
                edges[i].insert(to);
            }
        }
        if !step.route.iter().any(Route::is_catch_all) && i + 1 < n {
            edges[i].insert(i + 1);
        }
        for action in [step.on_error.as_deref(), step.on_max_visits.as_deref()] {
            if let Some(target) = action.and_then(|a| a.strip_prefix("goto:")) {
                if let Some(&to) = top_index.get(target) {
                    edges[i].insert(to);
                }
            }
        }
        if step.on_error.as_deref() == Some("continue") && i + 1 < n {
            edges[i].insert(i + 1);
        }
        if step.on_max_visits.as_deref() == Some("continue") && i + 1 < n {
            edges[i].insert(i + 1);
        }
    }

    let mut reachable = HashSet::from([entry]);
    let mut stack = vec![entry];
    while let Some(node) = stack.pop() {
        for &next in &edges[node] {
            if reachable.insert(next) {
                stack.push(next);
            }
        }
    }
    let universe = reachable.clone();
    let mut dom: Vec<HashSet<usize>> = (0..=n)
        .map(|node| {
            if node == entry {
                HashSet::from([entry])
            } else if reachable.contains(&node) {
                universe.clone()
            } else {
                HashSet::new()
            }
        })
        .collect();
    let mut predecessors: Vec<Vec<usize>> = (0..=n).map(|_| Vec::new()).collect();
    for (from, nexts) in edges.iter().enumerate() {
        for &to in nexts {
            predecessors[to].push(from);
        }
    }
    loop {
        let mut changed = false;
        for node in 0..n {
            if !reachable.contains(&node) {
                continue;
            }
            let mut incoming = predecessors[node]
                .iter()
                .filter(|p| reachable.contains(p))
                .map(|p| dom[*p].clone());
            let mut next = incoming.next().unwrap_or_default();
            for other in incoming {
                next.retain(|candidate| other.contains(candidate));
            }
            next.insert(node);
            if next != dom[node] {
                dom[node] = next;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let source_can_fail_open = |step: &Step| {
        matches!(step.on_error.as_deref(), Some("continue"))
            || step
                .on_error
                .as_deref()
                .and_then(|a| a.strip_prefix("goto:"))
                .is_some_and(|target| !TERMINALS.contains(&target))
    };
    let check = |consumer: &Step, consumer_owner: usize| -> Result<(), String> {
        for (kind, target) in [
            ("continue_from", consumer.continue_from.as_deref()),
            ("fork_from", consumer.fork_from.as_deref()),
        ] {
            let Some(target) = target else { continue };
            let Some(&target_owner) = owner.get(target) else {
                continue; // existence is reported by the earlier validation.
            };
            if reachable.contains(&consumer_owner) && !dom[consumer_owner].contains(&target_owner) {
                return Err(format!(
                    "step '{}': {kind} target '{target}' is not guaranteed to run before this step on every control-flow path",
                    consumer.id
                ));
            }
            if let Some(target_step) = source.get(target) {
                if source_can_fail_open(target_step) {
                    return Err(format!(
                        "step '{}': {kind} target '{target}' may fail and continue without creating a session; make the source fail closed or route failures to a terminal",
                        consumer.id
                    ));
                }
                // A parallel child can be fail-closed itself while its owning
                // group deliberately continues or jumps after that child
                // fails. In that case the group dominates this consumer but
                // the requested child session does not exist.
                let owner_step = &flow.steps[target_owner];
                if target_step.id != owner_step.id && source_can_fail_open(owner_step) {
                    return Err(format!(
                        "step '{}': {kind} target '{target}' is a child of parallel group '{}', whose on_error can continue after the child failed without creating a session; make the group fail closed or route failures to a terminal",
                        consumer.id, owner_step.id
                    ));
                }
                // A source with cross-provider fallbacks has no stable session
                // type. The downstream step cannot know whether it is being
                // handed (for example) a Claude or Codex session, so accepting
                // this would defer a deterministic configuration error until
                // after the fallback has already spent money.
                let mut session_tools = BTreeSet::new();
                for override_profile in std::iter::once(None)
                    .chain(target_step.fallback.iter().map(|name| Some(name.as_str())))
                {
                    if let Ok(effective) =
                        crate::leaf::effective_with(flow, target_step, override_profile)
                    {
                        if let Some(tool) = effective.tool {
                            session_tools.insert(tool);
                        }
                    }
                }
                if session_tools.len() > 1 {
                    return Err(format!(
                        "step '{}': {kind} target '{target}' can finish under different tools through fallback ({}) and therefore has no stable session provider; keep all source fallbacks on one tool or do not reuse its session",
                        consumer.id,
                        session_tools.into_iter().collect::<Vec<_>>().join("/")
                    ));
                }
            }
        }
        Ok(())
    };
    for (i, step) in flow.steps.iter().enumerate() {
        check(step, i)?;
        if let Some(children) = &step.parallel {
            for child in children {
                check(child, i)?;
            }
        }
    }

    let check_templates = |consumer: &Step,
                           consumer_owner: usize,
                           is_top: bool|
     -> Result<(), String> {
        for (label, text) in template_fields(flow, consumer) {
            for dependency in crate::template::step_refs(&text) {
                if dependency.optional {
                    continue;
                }
                let Some(&target_owner) = owner.get(dependency.id.as_str()) else {
                    continue; // unknown ids are reported by precheck.
                };
                if target_owner == consumer_owner {
                    let after_current = label == "route" || label == "compact.instruction";
                    let target_is_own_child = is_top
                        && consumer
                            .parallel
                            .as_ref()
                            .is_some_and(|children| children.iter().any(|c| c.id == dependency.id));
                    let target_is_self = consumer.id == dependency.id;
                    if (target_is_self && after_current)
                        || (target_is_own_child && label == "route")
                    {
                        continue;
                    }
                    return Err(format!(
                            "step '{}': {label} references steps.{} before that output is guaranteed to exist; add `| optional`/`| default:...` only if the missing branch is intentional",
                            consumer.id, dependency.id
                        ));
                }
                if reachable.contains(&consumer_owner)
                    && !dom[consumer_owner].contains(&target_owner)
                {
                    return Err(format!(
                            "step '{}': {label} references steps.{} but that step does not dominate this consumer on every control-flow path; mark the reference `| optional`/`| default:...` or fix the routing",
                            consumer.id, dependency.id
                        ));
                }
            }
        }
        Ok(())
    };
    for (i, step) in flow.steps.iter().enumerate() {
        check_templates(step, i, true)?;
        if let Some(children) = &step.parallel {
            for child in children {
                check_templates(child, i, false)?;
            }
        }
    }
    Ok(())
}

fn template_fields(flow: &Flow, step: &Step) -> Vec<(&'static str, String)> {
    fn push_opt(
        fields: &mut Vec<(&'static str, String)>,
        label: &'static str,
        value: Option<&String>,
    ) {
        if let Some(value) = value {
            fields.push((label, value.clone()));
        }
    }
    let mut fields = Vec::new();
    push_opt(&mut fields, "prompt", step.prompt.as_ref());
    push_opt(&mut fields, "bin", step.bin.as_ref());
    push_opt(&mut fields, "model", step.model.as_ref());
    push_opt(&mut fields, "effort", step.effort.as_ref());
    push_opt(&mut fields, "agent", step.agent.as_ref());
    push_opt(&mut fields, "cwd", step.cwd.as_ref());
    for arg in &step.args {
        fields.push(("args", arg.clone()));
    }
    match &step.cmd {
        Some(Cmd::Shell(command)) => fields.push(("cmd", command.clone())),
        Some(Cmd::Argv(args)) => fields.extend(args.iter().cloned().map(|arg| ("cmd", arg))),
        None => {}
    }
    for value in step.env.values() {
        fields.push(("env", value.clone()));
    }
    if let Some(foreach) = &step.foreach {
        fields.push(("foreach.from", foreach.from.clone()));
    }
    for route in &step.route {
        for value in [
            &route.when_contains,
            &route.when_matches,
            &route.when_last_line_contains,
            &route.when_last_line_is,
            &route.when_last_line_matches,
            &route.when_stderr_matches,
        ]
        .into_iter()
        .flatten()
        {
            fields.push(("route", value.clone()));
        }
        if let Some(members) = &route.when_members {
            fields.push(("route", members.last_line_is.clone()));
        }
    }
    if let Some(compact) = &step.compact {
        push_opt(
            &mut fields,
            "compact.instruction",
            compact.instruction.as_ref(),
        );
    }
    if !step.is_group() {
        // Match prepare_leaf/precheck exactly: only values that survive the
        // step > profile > defaults merge are runtime dependencies. Checking
        // every raw layer rejects valid flows when, for example, a step's cwd
        // safely overrides a profile/default cwd that references another
        // branch. Fallbacks are separate executable variants and must all be
        // included.
        let variants =
            std::iter::once(None).chain(step.fallback.iter().map(|name| Some(name.as_str())));
        for profile_override in variants {
            if let Ok(effective) = crate::leaf::effective_with(flow, step, profile_override) {
                for (label, value) in [
                    ("bin", &effective.bin),
                    ("model", &effective.model),
                    ("effort", &effective.effort),
                    ("agent", &effective.agent),
                    ("cwd", &effective.cwd),
                ] {
                    push_opt(&mut fields, label, value.as_ref());
                }
                for arg in &effective.args {
                    fields.push(("args", arg.clone()));
                }
                for value in effective.env.values() {
                    fields.push(("env", value.clone()));
                }
            }
        }
    }
    fields
}

fn check_retry_on(ctx: &str, v: &str) -> Result<(), String> {
    if ["transient", "any", "never"].contains(&v) {
        Ok(())
    } else {
        Err(format!("{ctx}: retry_on must be transient/any/never"))
    }
}

/// access after merging step > profile (use:) > defaults, as declared in YAML.
fn resolved_access<'a>(flow: &'a Flow, s: &'a Step) -> Option<&'a str> {
    s.access
        .as_deref()
        .or_else(|| {
            s.use_
                .as_ref()
                .and_then(|u| flow.profiles.get(u))
                .and_then(|p| p.access.as_deref())
        })
        .or(flow.defaults.access.as_deref())
}

fn validate_step(flow: &Flow, s: &Step, is_child: bool, legacy: bool) -> Result<(), String> {
    // Fresh runs (and resumes of 1.x runs) require access: on AI steps; a
    // legacy-era resume restores the pre-1.0 write default instead (load_lenient).
    let require_access = !legacy;
    let sid = &s.id;
    positive_u64(&format!("step '{sid}'.timeout_sec"), s.timeout_sec)?;
    positive_u32(&format!("step '{sid}'.max_visits"), s.max_visits)?;
    positive_u64(
        &format!("step '{sid}'.max_prompt_chars"),
        s.max_prompt_chars,
    )?;
    if is_child {
        if !s.route.is_empty() {
            return Err(format!(
                "step '{sid}': route: is not allowed inside parallel: (put it on the group)"
            ));
        }
        if s.parallel.is_some() || s.foreach.is_some() {
            return Err(format!("step '{sid}': parallel/foreach cannot be nested"));
        }
        if s.notes.is_some()
            || s.compact.is_some()
            || s.max_visits.is_some()
            || s.on_max_visits.is_some()
        {
            return Err(format!(
                "step '{sid}': notes/compact/max_visits/on_max_visits are not supported inside parallel: (put them on the group)"
            ));
        }
    }
    if s.is_group() {
        if s.tool.is_some()
            || s.cmd.is_some()
            || s.prompt.is_some()
            || s.foreach.is_some()
            || s.continue_from.is_some()
            || s.fork_from.is_some()
            || s.use_.is_some()
            || s.model.is_some()
            || s.stdin.is_some()
            || s.agent.is_some()
            || s.compact.is_some()
            || s.bin.is_some()
            || s.effort.is_some()
            || s.access.is_some()
            || s.allow_access_override.is_some()
            || s.unsafe_shell_template.is_some()
            || s.allow_dynamic_exec_paths.is_some()
            || !s.args.is_empty()
            || s.cwd.is_some()
            || s.timeout_sec.is_some()
            || s.max_prompt_chars.is_some()
            || s.retry.is_some()
            || s.retry_on.is_some()
            || s.hang_after_sec.is_some()
            || !s.fallback.is_empty()
            || !s.env.is_empty()
            || !s.env_remove.is_empty()
            || s.allow_empty.is_some()
            // A group runs no command of its own, so it has no exit code for a
            // table to describe. Accepting one silently would leave a
            // `when_label_is:` rule that validate approves - the label really
            // is declared - and that can never match at run time, which is the
            // worst of both answers.
            || !s.outcomes.is_empty()
        {
            return Err(format!(
                "step '{sid}': a parallel: group carries only id/max_parallel/route/on_error/max_visits/on_max_visits/notes (tool settings go on the children)"
            ));
        }
        return group_common(s);
    }
    // leaf (possibly foreach) checks
    if let Some(u) = &s.use_ {
        if !flow.profiles.contains_key(u) {
            return Err(format!("step '{sid}': unknown profile '{u}' in use:"));
        }
    }
    for f in &s.fallback {
        if !flow.profiles.contains_key(f) {
            return Err(format!("step '{sid}': unknown profile '{f}' in fallback:"));
        }
    }
    let mut fallback_names = HashSet::new();
    for fallback in &s.fallback {
        if !fallback_names.insert(fallback.to_ascii_lowercase()) {
            return Err(format!(
                "step '{sid}': fallback profile '{fallback}' is listed more than once (compared ignoring case); repeated fallbacks would overwrite each other's durable artifacts"
            ));
        }
    }
    if let Some(r) = &s.retry_on {
        check_retry_on(&format!("step '{sid}'"), r)?;
    }
    if let Some(t) = &s.tool {
        if !TOOLS.contains(&t.as_str()) {
            return Err(format!(
                "step '{sid}': unknown tool '{t}' (use one of {} or a custom cmd:)",
                TOOLS.join("/")
            ));
        }
    }
    let profile_tool = s
        .use_
        .as_ref()
        .and_then(|u| flow.profiles.get(u))
        .and_then(|p| p.tool.clone());
    if s.tool.is_none() && s.cmd.is_none() && profile_tool.is_none() && flow.defaults.tool.is_none()
    {
        return Err(format!(
            "step '{sid}': needs 'tool' or 'cmd' (or a profile/defaults tool)"
        ));
    }
    if let Some(a) = &s.access {
        if !["read", "write", "full"].contains(&a.as_str()) {
            return Err(format!(
                "step '{sid}': access must be read/write/full, got '{a}'"
            ));
        }
    }
    if s.cmd.is_none() {
        // "write" means a different thing per tool, so there is no safe implicit
        // default: an AI step must say which tier it wants (cmd: steps are exempt).
        // The one exception is a --resume of an unchanged legacy run (load_lenient),
        // where sfh <= 0.9 legitimately defaulted a missing access to write; there
        // we restore that default with a warning instead of blocking the resume
        // (rev_regression R-2). require_access is true everywhere else.
        let acc = match resolved_access(flow, s) {
            Some(a) => a,
            None if require_access => {
                return Err(format!(
                    "step '{sid}': access is required for steps that call an AI tool - set access: read, write, or full on the step, its profile, or defaults (cmd: steps are exempt)"
                ));
            }
            None => {
                eprintln!(
                    "sfh: warning: step '{sid}' has no access: level; defaulting to 'write' (the pre-1.0 default) so this legacy run can be resumed - add access: to the flow to make it explicit"
                );
                "write"
            }
        };
        // headless cursor is binary (deny-all or --force), so "write" would
        // silently mean "full". Refuse it; the step must pick read or full.
        if flow.step_tool(s).as_deref() == Some("cursor") && acc == "write" {
            return Err(format!(
                "step '{sid}': cursor headless has only two permission tiers (read = deny-all, full = approve-all); access: write is not supported - pick read or full"
            ));
        }
        // `use:` and `fallback:` are independent profile selections. Access
        // from the primary profile must not leak into a fallback that never
        // declared it: effective_with would otherwise parse the missing value
        // as the historical implicit `write`, bypassing the mandatory-access
        // contract only on the recovery path.
        for fallback in &s.fallback {
            let profile = &flow.profiles[fallback];
            let fallback_access = s
                .access
                .as_deref()
                .or(profile.access.as_deref())
                .or(flow.defaults.access.as_deref());
            let fallback_access = match fallback_access {
                Some(access) => access,
                None if require_access => {
                    return Err(format!(
                        "step '{sid}': fallback profile '{fallback}' has no resolved access level; set access: read/write/full on the step, that fallback profile, or defaults"
                    ));
                }
                None => {
                    eprintln!(
                        "sfh: warning: step '{sid}' fallback profile '{fallback}' has no access level; defaulting to 'write' only for this legacy resume"
                    );
                    "write"
                }
            };
            let fallback_tool = profile
                .tool
                .as_deref()
                .or(s.tool.as_deref())
                .or(flow.defaults.tool.as_deref());
            if fallback_tool == Some("cursor") && fallback_access == "write" {
                return Err(format!(
                    "step '{sid}': fallback profile '{fallback}' resolves cursor with access: write, but cursor headless supports only read or full"
                ));
            }
        }
        // Args that WIDEN the declared access are a validation error unless the
        // step explicitly opts in. The check reads the arg VALUES, so an arg
        // that narrows the permission (sandbox_mode=read-only on a read step)
        // passes. Args containing templates are re-checked after rendering,
        // both in the precheck and right before each spawn. Fallback-profile
        // args are covered by the same runtime check.
        if acc != "full" && !s.allow_access_override.unwrap_or(false) {
            if let Some(t) = flow.step_tool(s) {
                let access = crate::preset::Access::parse(Some(acc))
                    .map_err(|e| format!("step '{sid}': {e}"))?;
                let prof_args = s
                    .use_
                    .as_ref()
                    .and_then(|u| flow.profiles.get(u))
                    .map(|p| p.args.as_slice())
                    .unwrap_or(&[]);
                let literal: Vec<String> = prof_args
                    .iter()
                    .chain(s.args.iter())
                    .filter(|a| !a.contains("{{"))
                    .cloned()
                    .collect();
                if let Some(e) = crate::preset::find_escalation(&t, access, &literal) {
                    return Err(crate::preset::escalation_error(sid, access, &e));
                }
            }
        }
    }
    if let Some(st) = &s.stdin {
        if !["prompt", "none"].contains(&st.as_str()) {
            return Err(format!("step '{sid}': stdin must be 'prompt' or 'none'"));
        }
    }
    if let Some(n) = &s.notes {
        if n != "append" {
            return Err(format!("step '{sid}': notes must be \"append\""));
        }
    }
    if let Some(Cmd::Argv(v)) = &s.cmd {
        if v.is_empty() {
            return Err(format!("step '{sid}': cmd array is empty"));
        }
        // argv[0] is the program sfh executes: an executed-privileged sink, so
        // the same run-derived-template refusal the runtime applies is checked
        // here statically (rev_break #12, rev_regression: validate must reject
        // what run rejects, instead of failing only after upstream steps ran).
        // At load time no var is tainted yet (vars come from the flow / --var,
        // both user-controlled), so an empty set reproduces the static subset.
        if !s.allow_dynamic_exec_paths.unwrap_or(false) {
            let no_tainted: HashSet<String> = HashSet::new();
            crate::template::check_keys(&v[0], |key| {
                crate::leaf::exec_path_key_check(key, &no_tainted)
            })
            .map_err(|e| format!("step '{sid}': cmd[0]: {e}"))?;
        }
        // An argv form that wraps a shell (["sh","-c","..."]) re-parses its
        // script text in that shell, so a template in the script is exactly as
        // dangerous as one in a string-form cmd and gets the same refusal -
        // the old code saw only "the argv branch" and skipped every shell
        // defence (rev_break #13). legacy flows predate the rule (load_lenient).
        if !legacy && !s.unsafe_shell_template.unwrap_or(false) {
            if let Some(span) = crate::leaf::shell_script_span(v) {
                for x in &v[span.start.min(v.len())..span.end.min(v.len())] {
                    if crate::template::contains_template(x) {
                        return Err(format!(
                            "step '{sid}': cmd wraps a shell and its shell text contains a template, which is disabled by default for the same reason as a string-form cmd (the value is re-parsed by the shell). Pass it as an argument instead of splicing it into the script - arguments after the script become $1, $2 ... inside it and are never re-parsed:\n  cmd: [\"sh\", \"-c\", \"grep -- \\\"$1\\\" file\", \"{sid}\", \"{{{{steps.x.output}}}}\"]\nor, if no shell is needed at all:\n  cmd: [\"program\", \"--flag\", \"{{{{steps.x.output}}}}\"]\nSetting unsafe_shell_template: true accepts shell templating, with only a metacharacter filter that a hostile value can still get past"
                        ));
                    }
                }
            }
        }
    }
    if let Some(Cmd::Shell(c)) = &s.cmd {
        if c.trim().is_empty() {
            return Err(format!("step '{sid}': cmd string is empty"));
        }
        if !s.unsafe_shell_template.unwrap_or(false) && crate::template::contains_template(c) {
            if legacy {
                // Pre-1.0 flows legitimately templated string cmds; load_lenient
                // restores that instead of wedging the resume (rev_regression:
                // an unchanged old flow could not be resumed at all).
                eprintln!(
                    "sfh: warning: step '{sid}': string-form cmd expands a template (allowed for this legacy resume; add unsafe_shell_template: true to make it explicit)"
                );
            } else {
                return Err(format!(
                    "step '{sid}': string-form cmd contains a template, but template expansion in a string cmd is disabled by default: the substituted value is handed to a shell (cmd /C | sh -c), and a metacharacter blacklist cannot make that safe (a hostile value can be a dangerous option to the target program without any shell metacharacters). Use the array form, which spawns without a shell:\n  cmd: [\"program\", \"--flag\", \"{{{{steps.x.output}}}}\"]\nor set unsafe_shell_template: true on this step to accept shell templating (substituted values are then only checked for shell metacharacters)"
                ));
            }
        }
        validate_shell_portability(sid, c)?;
    }
    // bin / cwd are executed-privileged (argv[0] and the write base): reject
    // step-output / run-derived templates statically, mirroring the runtime
    // check so `sfh validate` fails where `sfh run` would (rev_regression: the
    // validator used to pass flows that always failed at runtime, after upstream
    // steps had already run and spent). Merged values (step > profile > defaults)
    // are checked, matching what the runtime renders.
    if !s.allow_dynamic_exec_paths.unwrap_or(false) {
        let no_tainted: HashSet<String> = HashSet::new();
        // Every profile this step can run under, not just its primary. A
        // fallback's bin/cwd is executed exactly like the primary's, and the
        // run-time precheck already enumerates all of them - so a flow with
        // `bin: "{{steps.a.output}}"` on a FALLBACK passed `sfh validate` and
        // then died at `sfh run`, after the upstream steps had been paid for.
        // That is the precise failure F-5 exists to prevent.
        let mut effs = vec![crate::leaf::effective(flow, s)];
        for fb in &s.fallback {
            effs.push(crate::leaf::effective_with(flow, s, Some(fb)));
        }
        for eff in effs.into_iter().flatten() {
            for (label, val) in [("bin", &eff.bin), ("cwd", &eff.cwd)] {
                if let Some(t) = val {
                    crate::template::check_keys(t, |key| {
                        crate::leaf::exec_path_key_check(key, &no_tainted)
                    })
                    .map_err(|e| format!("step '{sid}': {label}: {e}"))?;
                }
            }
        }
    }
    if s.cmd.is_some() && !s.fallback.is_empty() {
        return Err(format!(
            "step '{sid}': fallback: works only with preset tools"
        ));
    }
    if (s.continue_from.is_some() || s.fork_from.is_some()) && s.cmd.is_some() {
        return Err(format!(
            "step '{sid}': continue_from/fork_from work only with preset tools, not cmd:"
        ));
    }
    if (s.continue_from.is_some() || s.fork_from.is_some()) && !s.fallback.is_empty() {
        return Err(format!(
            "step '{sid}': fallback: cannot be combined with continue_from/fork_from (a session belongs to one tool)"
        ));
    }
    // Fork support is per-tool; catch it at load time instead of mid-flow.
    if s.fork_from.is_some() {
        let tool = s
            .tool
            .clone()
            .or_else(|| {
                s.use_
                    .as_ref()
                    .and_then(|u| flow.profiles.get(u))
                    .and_then(|p| p.tool.clone())
            })
            .or_else(|| flow.defaults.tool.clone());
        if let Some(t) = tool {
            if !crate::preset::supports_fork(&t) {
                return Err(format!(
                    "step '{sid}': tool '{t}' cannot fork a session headlessly (only claude/opencode/grok/pi can); use continue_from to chain this step serially, or drop fork_from and give it its own context"
                ));
            }
        }
    }
    if let Some(f) = &s.foreach {
        if let Some(sp) = &f.split {
            let ok = sp == "lines" || sp == "json" || sp.starts_with("separator:");
            if !ok {
                return Err(format!(
                    "step '{sid}': foreach.split must be lines | json | separator:<sep>"
                ));
            }
        }
        if s.continue_from.is_some() {
            return Err(format!(
                "step '{sid}': continue_from cannot be combined with foreach (every item would resume the same session)"
            ));
        }
    }
    if let Some(c) = &s.compact {
        if c.when_over == 0 {
            return Err(format!("step '{sid}': compact.when_over must be > 0"));
        }
        if let Some(u) = &c.use_ {
            if !flow.profiles.contains_key(u) {
                return Err(format!("step '{sid}': compact uses unknown profile '{u}'"));
            }
        }
        positive_u64(
            &format!("step '{sid}'.compact.target_chars"),
            c.target_chars,
        )?;
        positive_u64(
            &format!("step '{sid}'.compact.max_input_chars"),
            c.max_input_chars,
        )?;
        positive_u64(&format!("step '{sid}'.compact.timeout_sec"), c.timeout_sec)?;
        let has_tool = c.tool.is_some()
            || c.use_
                .as_ref()
                .and_then(|u| flow.profiles.get(u))
                .and_then(|p| p.tool.as_ref())
                .is_some();
        if !has_tool {
            return Err(format!(
                "step '{sid}': compact needs use: <profile> or tool:"
            ));
        }
        let compact_tool = c.tool.as_ref().or_else(|| {
            c.use_
                .as_ref()
                .and_then(|u| flow.profiles.get(u))
                .and_then(|p| p.tool.as_ref())
        });
        if let Some(t) = compact_tool {
            if !TOOLS.contains(&t.as_str()) {
                return Err(format!("step '{sid}': compact resolves unknown tool '{t}'"));
            }
        }
    }
    group_common(s)
}

fn validate_shell_portability(step_id: &str, command: &str) -> Result<(), String> {
    let Some((syntax, shell, reason)) = shell_portability_issue(command) else {
        return Ok(());
    };
    let explicit = if shell == "sh" {
        r#"cmd: ["sh", "-c", "..."]"#
    } else {
        r#"cmd: ["cmd", "/C", "..."]"#
    };
    Err(format!(
        "step '{step_id}': `{syntax}` {reason}, so this string-form cmd has different meanings on Windows and Unix. Pin the shell with array form:\n  {explicit}\nOr avoid a shell (recommended):\n  cmd: [\"cargo\", \"test\"]"
    ))
}

fn shell_portability_issue(command: &str) -> Option<(&'static str, &'static str, &'static str)> {
    #[derive(Clone, Copy, PartialEq)]
    enum Quote {
        Single,
        Double,
    }

    let bytes = command.as_bytes();
    let mut quote = None;
    let mut semicolon = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' if quote != Some(Quote::Double) => {
                quote = if quote == Some(Quote::Single) {
                    None
                } else {
                    Some(Quote::Single)
                };
            }
            b'"' if quote != Some(Quote::Single) => {
                quote = if quote == Some(Quote::Double) {
                    None
                } else {
                    Some(Quote::Double)
                };
            }
            b'$' if quote != Some(Quote::Single) && index + 1 < bytes.len() => {
                let issue = match bytes[index + 1] {
                    b'?' => Some(("$?", "sh", "is sh-only exit-status syntax")),
                    b'(' => Some(("$(", "sh", "starts sh-only command substitution")),
                    b'{' => Some(("${", "sh", "starts sh-only parameter expansion")),
                    _ => None,
                };
                if issue.is_some() {
                    return issue;
                }
            }
            b'`' if quote != Some(Quote::Single) => {
                return Some(("`", "sh", "starts sh-only command substitution"));
            }
            b';' if quote.is_none() => semicolon = true,
            b'^' if quote != Some(Quote::Double) => {
                return Some(("^", "cmd", "is cmd.exe escape syntax"));
            }
            b'%' => {
                let rest = &bytes[index + 1..];
                if let Some(end) = rest.iter().position(|byte| *byte == b'%') {
                    let name = &rest[..end];
                    if !name.is_empty()
                        && (name[0].is_ascii_alphabetic() || name[0] == b'_')
                        && name[1..]
                            .iter()
                            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    {
                        return Some(("%NAME%", "cmd", "is cmd.exe-only variable syntax"));
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }

    if semicolon && quote.is_none() {
        Some((";", "sh", "is a command separator in sh but not in cmd.exe"))
    } else {
        None
    }
}

fn group_common(s: &Step) -> Result<(), String> {
    if let Some(mp) = s.max_parallel {
        if mp == 0 {
            return Err(format!("step '{}': max_parallel must be >= 1", s.id));
        }
    }
    if s.max_parallel.is_some() && !s.is_group() && !s.is_foreach() {
        return Err(format!(
            "step '{}': max_parallel only makes sense with parallel: or foreach:",
            s.id
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Returns Err(message) so tests can assert on validation text. Fresh-run
    /// (strict) validation: legacy = false.
    fn parse(y: &str) -> Result<(), String> {
        let f: Flow = yaml::from_str(y).map_err(|e| e.to_string())?;
        validate(&f, false)
    }

    #[test]
    fn rejects_group_with_tool_settings() {
        let e = parse("name: t\nsteps:\n  - id: g\n    access: read\n    parallel:\n      - id: c\n        cmd: echo hi\n").unwrap_err();
        assert!(e.contains("carries only"), "{e}");
    }

    #[test]
    fn rejects_child_notes_and_compact() {
        let e = parse("name: t\nsteps:\n  - id: g\n    parallel:\n      - id: c\n        cmd: echo hi\n        notes: append\n").unwrap_err();
        assert!(e.contains("not supported inside parallel"), "{e}");
    }

    #[test]
    fn rejects_continue_from_foreach_and_self() {
        let e = parse("name: t\nsteps:\n  - id: a\n    foreach: {from: x}\n    cmd: echo hi\n  - id: b\n    tool: claude\n    access: read\n    continue_from: a\n    prompt: x\n").unwrap_err();
        assert!(e.contains("foreach step"), "{e}");
        let e = parse(
            "name: t\nsteps:\n  - id: b\n    tool: claude\n    access: read\n    continue_from: b\n    prompt: x\n",
        )
        .unwrap_err();
        assert!(e.contains("itself"), "{e}");
    }

    #[test]
    fn rejects_sibling_and_duplicate_resume_targets() {
        let y = "name: t\nsteps:\n  - id: seed\n    tool: claude\n    access: read\n    prompt: x\n  - id: g\n    parallel:\n      - id: c1\n        tool: claude\n        access: read\n        continue_from: seed\n        prompt: x\n      - id: c2\n        tool: claude\n        access: read\n        continue_from: seed\n        prompt: x\n";
        let e = parse(y).unwrap_err();
        assert!(e.contains("two concurrent resumes"), "{e}");
    }

    #[test]
    fn on_budget_needs_a_ceiling_a_target_and_the_goto_spelling() {
        let steps = "steps:\n  - id: a\n    cmd: echo hi\n  - id: wrap\n    cmd: echo bye\n";
        // Both halves together, under a ceiling, is the working shape.
        assert!(parse(&format!(
            "name: t\ndefaults:\n  max_cost_usd: 10.0\n  on_budget: goto:wrap\n  budget_reserve: {{cost_usd: 1.0}}\n{steps}"
        ))
        .is_ok());
        // The three terminals are landing targets like any other.
        for t in ["end", "fail", "stuck"] {
            let y = format!("name: t\ndefaults:\n  wall_clock_sec: 60\n  on_budget: goto:{t}\n  budget_reserve: {{wall_clock_sec: 10}}\n{steps}");
            assert!(parse(&y).is_ok(), "goto:{t} should be a valid landing");
        }
        // A reserve alone only decides how early a landing that does not exist
        // would fire, so it is refused rather than silently ignored.
        let e = parse(&format!(
            "name: t\ndefaults:\n  max_cost_usd: 10.0\n  budget_reserve: {{cost_usd: 1.0}}\n{steps}"
        ))
        .unwrap_err();
        assert!(e.contains("on_budget"), "{e}");
        // A landing with no ceiling above it can never fire.
        let e = parse(&format!(
            "name: t\ndefaults:\n  on_budget: goto:wrap\n{steps}"
        ))
        .unwrap_err();
        assert!(e.contains("max_cost_usd"), "{e}");
        // Bare step ids and unknown targets are both refused.
        let e = parse(&format!(
            "name: t\ndefaults:\n  wall_clock_sec: 60\n  on_budget: wrap\n{steps}"
        ))
        .unwrap_err();
        assert!(e.contains("goto:<id>"), "{e}");
        let e = parse(&format!(
            "name: t\ndefaults:\n  wall_clock_sec: 60\n  on_budget: goto:nowhere\n{steps}"
        ))
        .unwrap_err();
        assert!(e.contains("nowhere"), "{e}");
        // A negative reserve would lift the threshold ABOVE the ceiling and
        // quietly restore the hard failure the flow was written to avoid.
        let e = parse(&format!(
            "name: t\ndefaults:\n  max_cost_usd: 10.0\n  on_budget: goto:wrap\n  budget_reserve: {{cost_usd: -1.0}}\n{steps}"
        ))
        .unwrap_err();
        assert!(e.contains("budget_reserve.cost_usd"), "{e}");
    }

    #[test]
    fn budget_reserve_defaults_to_nothing_held_back_on_axes_with_no_ceiling() {
        let load = |y: &str| -> Flow {
            let f: Flow = yaml::from_str(y).expect("test flow parses");
            validate(&f, false).expect("test flow validates");
            f
        };
        // Only the cost axis has a ceiling, so only the cost axis needs a
        // reserve; the wall-clock accessor still reports the 0 default.
        let f = load("name: t\ndefaults:\n  max_cost_usd: 10.0\n  on_budget: goto:wrap\n  budget_reserve: {cost_usd: 1.0}\nsteps:\n  - id: a\n    cmd: echo hi\n  - id: wrap\n    cmd: echo bye\n");
        assert_eq!(f.defaults.budget_goto(), Some("wrap"));
        assert_eq!(f.defaults.budget_reserve_usd(), 1.0);
        assert_eq!(f.defaults.budget_reserve_sec(), 0);
        // No landing declared at all is the pre-F5 flow, unchanged.
        let f = load("name: t\nsteps:\n  - id: a\n    cmd: echo hi\n");
        assert_eq!(f.defaults.budget_goto(), None);
        assert_eq!(f.defaults.budget_reserve_usd(), 0.0);
        assert_eq!(f.defaults.budget_reserve_sec(), 0);
    }

    /// A landing whose threshold IS the ceiling fires and is then overtaken by
    /// the ceiling check on the same numbers, one loop iteration later, with the
    /// landing chain still unrun. Per axis: reserving on one buys the other
    /// nothing.
    #[test]
    fn refuses_a_landing_that_holds_nothing_back_on_a_declared_ceiling() {
        let steps = "steps:\n  - id: a\n    cmd: echo hi\n  - id: wrap\n    cmd: echo bye\n";
        for (defaults, axis) in [
            ("  max_cost_usd: 10.0\n  on_budget: goto:wrap\n", "max_cost_usd"),
            ("  wall_clock_sec: 60\n  on_budget: goto:wrap\n", "wall_clock_sec"),
            // Reserved on cost, silent on wall-clock: the wall axis alone is
            // enough to refuse, and naming it is the point.
            ("  max_cost_usd: 10.0\n  wall_clock_sec: 60\n  on_budget: goto:wrap\n  budget_reserve: {cost_usd: 2.0}\n", "wall_clock_sec"),
            ("  max_cost_usd: 10.0\n  wall_clock_sec: 60\n  on_budget: goto:wrap\n  budget_reserve: {wall_clock_sec: 30}\n", "max_cost_usd"),
            // Written out as zero is the same thing said out loud.
            ("  wall_clock_sec: 60\n  on_budget: goto:wrap\n  budget_reserve: {wall_clock_sec: 0}\n", "wall_clock_sec"),
        ] {
            let e = parse(&format!("name: t\ndefaults:\n{defaults}{steps}")).unwrap_err();
            assert!(e.contains(axis), "expected {axis} to be named, got: {e}");
            assert!(e.contains("budget_reserve"), "{e}");
        }
        // Both axes reserved is the working shape.
        assert!(parse(&format!(
            "name: t\ndefaults:\n  max_cost_usd: 10.0\n  wall_clock_sec: 60\n  on_budget: goto:wrap\n  budget_reserve: {{cost_usd: 2.0, wall_clock_sec: 30}}\n{steps}"
        ))
        .is_ok());
    }

    #[test]
    fn accepts_on_max_visits_end() {
        let y = "name: t\nsteps:\n  - id: a\n    cmd: echo hi\n    max_visits: 2\n    on_max_visits: goto:end\n";
        assert!(parse(y).is_ok());
    }

    #[test]
    fn fork_from_is_rejected_for_tools_without_a_headless_fork() {
        let e = parse("name: t\nsteps:\n  - id: a\n    tool: codex\n    access: read\n    prompt: x\n  - id: b\n    tool: codex\n    access: read\n    fork_from: a\n    prompt: y\n").unwrap_err();
        assert!(e.contains("cannot fork a session headlessly"), "{e}");
        // claude can fork
        assert!(parse("name: t\nsteps:\n  - id: a\n    tool: claude\n    access: read\n    prompt: x\n  - id: b\n    tool: claude\n    access: read\n    fork_from: a\n    prompt: y\n").is_ok());
    }

    #[test]
    fn siblings_may_fork_one_parent_but_may_not_resume_it() {
        let fork = "name: t\nsteps:\n  - id: seed\n    tool: claude\n    access: read\n    prompt: x\n  - id: g\n    parallel:\n      - id: c1\n        tool: claude\n        access: read\n        fork_from: seed\n        prompt: x\n      - id: c2\n        tool: claude\n        access: read\n        fork_from: seed\n        prompt: y\n";
        assert!(
            parse(fork).is_ok(),
            "forking one parent from two siblings is the point"
        );
        let resume = fork.replace("fork_from", "continue_from");
        assert!(parse(&resume)
            .unwrap_err()
            .contains("two concurrent resumes"));
    }

    #[test]
    fn fork_from_and_continue_from_are_mutually_exclusive() {
        let e = parse("name: t\nsteps:\n  - id: a\n    tool: claude\n    access: read\n    prompt: x\n  - id: b\n    tool: claude\n    access: read\n    continue_from: a\n    fork_from: a\n    prompt: y\n").unwrap_err();
        assert!(e.contains("mutually exclusive"), "{e}");
    }

    // "write" means something different per tool, so an AI step must declare
    // its tier; only cmd: steps may omit it.
    #[test]
    fn ai_steps_must_declare_an_access_level() {
        let e = parse("name: t\nsteps:\n  - id: a\n    tool: claude\n    prompt: x\n").unwrap_err();
        assert!(e.contains("access is required"), "{e}");
        assert!(e.contains("read, write, or full"), "{e}");
        // step-level, profile-level and defaults-level all satisfy it
        assert!(parse(
            "name: t\nsteps:\n  - id: a\n    tool: claude\n    access: read\n    prompt: x\n"
        )
        .is_ok());
        assert!(parse(
            "name: t\nprofiles:\n  p: {tool: claude, access: read}\nsteps:\n  - id: a\n    use: p\n    prompt: x\n"
        )
        .is_ok());
        assert!(parse(
            "name: t\ndefaults:\n  access: full\nsteps:\n  - id: a\n    tool: claude\n    prompt: x\n"
        )
        .is_ok());
        // parallel children are AI steps too
        let e = parse(
            "name: t\nsteps:\n  - id: g\n    parallel:\n      - id: c\n        tool: claude\n        prompt: x\n",
        )
        .unwrap_err();
        assert!(e.contains("access is required"), "{e}");
        // cmd: steps are exempt
        assert!(parse("name: t\nsteps:\n  - id: a\n    cmd: echo hi\n").is_ok());
    }

    #[test]
    fn cursor_write_is_a_validation_error() {
        let e = parse(
            "name: t\nsteps:\n  - id: a\n    tool: cursor\n    access: write\n    prompt: x\n",
        )
        .unwrap_err();
        assert!(e.contains("two permission tiers"), "{e}");
        // defaults-supplied write is just as wrong
        let e = parse(
            "name: t\ndefaults:\n  access: write\nsteps:\n  - id: a\n    tool: cursor\n    prompt: x\n",
        )
        .unwrap_err();
        assert!(e.contains("two permission tiers"), "{e}");
        for acc in ["read", "full"] {
            assert!(
                parse(&format!(
                    "name: t\nsteps:\n  - id: a\n    tool: cursor\n    access: {acc}\n    prompt: x\n"
                ))
                .is_ok(),
                "{acc}"
            );
        }
    }

    // Args that change the tool's permission set used to be a warning; they are
    // now a validation error unless the step opts in or declares full.
    #[test]
    fn permission_flags_in_args_are_rejected_unless_full_or_overridden() {
        let cases = [
            (
                "pi",
                "write",
                "[\"-t\", \"read,bash,edit,write,grep,find,ls\"]",
            ),
            ("pi", "read", "[\"--approve\"]"),
            ("claude", "read", "[\"--allowedTools\", \"Bash\"]"),
            (
                "claude",
                "write",
                "[\"--permission-mode\", \"bypassPermissions\"]",
            ),
            ("opencode", "read", "[\"--agent\", \"build\"]"),
            ("grok", "write", "[\"--allow\", \"Bash(ls)\"]"),
            ("agy", "read", "[\"--mode\", \"accept-edits\"]"),
            ("codex", "write", "[\"-s\", \"danger-full-access\"]"),
            (
                "codex",
                "read",
                "[\"-c\", \"sandbox_mode=\\\"workspace-write\\\"\"]",
            ),
            ("codex", "write", "[\"--force\"]"),
        ];
        for (tool, acc, args) in cases {
            let yaml = format!(
                "name: t\nsteps:\n  - id: a\n    tool: {tool}\n    access: {acc}\n    args: {args}\n    prompt: x\n"
            );
            let e = parse(&yaml).unwrap_err();
            assert!(
                e.contains("overrides the declared access level"),
                "{tool}: {e}"
            );
            assert!(e.contains("allow_access_override"), "{tool}: {e}");
            // the escape hatch: an explicit opt-in
            let opted = yaml.replace("    args:", "    allow_access_override: true\n    args:");
            assert!(parse(&opted).is_ok(), "{tool} with override");
            // full means full: the same args are fine
            let full = yaml.replace(&format!("access: {acc}"), "access: full");
            assert!(parse(&full).is_ok(), "{tool} at full");
        }
        // profile args are merged into the step and checked the same way
        let e = parse(
            "name: t\nprofiles:\n  p: {tool: pi, access: write, args: [\"-t\", \"read,bash\"]}\nsteps:\n  - id: a\n    use: p\n    prompt: x\n",
        )
        .unwrap_err();
        assert!(e.contains("overrides the declared access level"), "{e}");
        // templated args cannot be judged at load time; the runtime check owns them
        assert!(parse(
            "name: t\nvars: {f: \"--force\"}\nsteps:\n  - id: a\n    tool: claude\n    access: read\n    args: [\"{{vars.f}}\"]\n    prompt: x\n"
        )
        .is_ok());
    }

    // The name becomes part of the run dir path: only PATH-affecting characters
    // are forbidden. Unicode, spaces and dots are legitimate names, and the
    // check must run in validate() so `sfh validate` and `sfh run` agree.
    #[test]
    fn flow_names_forbid_path_characters_only() {
        for name in [
            "研究 2026.07",
            "my flow",
            "v1.2.3",
            "a-b_c",
            "report (final)",
        ] {
            assert!(validate_name(name).is_ok(), "{name}");
            let y = format!("name: '{name}'\nsteps:\n  - id: a\n    cmd: echo hi\n");
            assert!(parse(&y).is_ok(), "validate must accept {name}");
        }
        for name in [
            "../evil", "a/b", "a\\b", "..", ".", "x:y", "a<b", "a>b", "a*b", "a?b", "a|b", "a\"b",
            "trail ", "trail.", "", "   ",
        ] {
            assert!(validate_name(name).is_err(), "{name}");
        }
        // and the same rejection happens through validate(), not just run
        let e = parse("name: '../evil'\nsteps:\n  - id: a\n    cmd: echo hi\n").unwrap_err();
        assert!(e.contains("flow name"), "{e}");
    }

    #[test]
    fn rejects_unknown_tool_and_bad_retry_on() {
        assert!(
            parse("name: t\nsteps:\n  - id: a\n    tool: gemini\n    prompt: x\n")
                .unwrap_err()
                .contains("unknown tool")
        );
        assert!(
            parse("name: t\nsteps:\n  - id: a\n    cmd: echo hi\n    retry_on: sometimes\n")
                .unwrap_err()
                .contains("retry_on")
        );
    }

    #[test]
    fn rejects_unsafe_budget_and_tool_parallel_values() {
        for value in ["-0.01", ".nan", ".inf", "-.inf"] {
            let error = parse(&format!(
                "name: t\ndefaults:\n  max_cost_usd: {value}\nsteps:\n  - id: a\n    cmd: echo hi\n"
            ))
            .unwrap_err();
            assert!(error.contains("max_cost_usd"), "{value}: {error}");
            assert!(error.contains("finite number >= 0"), "{value}: {error}");
        }

        let error = parse(
            "name: t\ndefaults:\n  tool_max_parallel: {claude: 0}\nsteps:\n  - id: a\n    cmd: echo hi\n",
        )
        .unwrap_err();
        assert!(error.contains("tool_max_parallel.claude"), "{error}");
        assert!(error.contains("forever"), "{error}");
    }

    #[test]
    fn rejects_leaf_retry_settings_silently_attached_to_a_group() {
        for field in ["retry_on: any", "hang_after_sec: 0"] {
            let error = parse(&format!(
                "name: t\nsteps:\n  - id: g\n    {field}\n    parallel:\n      - id: c\n        cmd: echo hi\n"
            ))
            .unwrap_err();
            assert!(error.contains("carries only"), "{field}: {error}");
        }
    }

    #[test]
    fn rejects_platform_specific_string_shell_syntax() {
        for (command, marker) in [
            ("echo $?", "$?"),
            ("echo $(date)", "$("),
            ("echo ${NAME}", "${"),
            ("echo `date`", "`"),
            ("echo first; echo second", ";"),
            ("echo %NAME%", "%NAME%"),
            ("echo one^&two", "^"),
        ] {
            let yaml = format!("steps:\n  - id: check\n    cmd: {command:?}\n");
            let error = parse(&yaml).unwrap_err();
            assert!(error.contains(marker), "{command:?}: {error}");
            assert!(error.contains("array form"), "{command:?}: {error}");
        }
    }

    #[test]
    fn accepts_portable_operators_and_quoted_semicolons() {
        for command in [
            "echo one && echo two || echo three",
            "echo one | echo two > output.txt 2>&1",
            "echo \"a;b\" && echo 'c;d'",
            "echo \"a^b\"",
            "echo '$? `${NAME}'",
            "echo \"unterminated; ambiguous",
        ] {
            let yaml = format!("steps:\n  - id: check\n    cmd: {command:?}\n");
            assert!(parse(&yaml).is_ok(), "{command:?}");
        }
    }

    #[test]
    fn string_cmd_template_expansion_is_rejected_unless_opted_in() {
        let e = parse("steps:\n  - id: a\n    cmd: \"tar -cf backup.tar {{steps.x.output}}\"\n")
            .unwrap_err();
        assert!(e.contains("disabled by default"), "{e}");
        assert!(e.contains("cmd: ["), "{e}");
        assert!(e.contains("unsafe_shell_template"), "{e}");
        // vars and builtins count as expansion too
        assert!(parse("steps:\n  - id: a\n    cmd: \"echo v{{visit}}\"\n").is_err());
        // the explicit opt-in
        assert!(parse(
            "steps:\n  - id: a\n    unsafe_shell_template: true\n    cmd: \"echo {{vars.x}}\"\n"
        )
        .is_ok());
        // raw blocks are not expansions
        assert!(parse(
            "steps:\n  - id: a\n    cmd: \"echo {{raw}}{{steps.x.output}}{{endraw}}\"\n"
        )
        .is_ok());
        // no template at all: fine either way
        assert!(parse("steps:\n  - id: a\n    cmd: \"echo plain\"\n").is_ok());
        // the array form is the recommended path and always allowed
        assert!(parse(
            "steps:\n  - id: a\n    cmd: [\"tar\", \"-cf\", \"backup.tar\", \"{{steps.x.output}}\"]\n"
        )
        .is_ok());
        // not a group setting
        let e = parse(
            "steps:\n  - id: g\n    unsafe_shell_template: true\n    parallel:\n      - id: c\n        cmd: [\"echo\", \"hi\"]\n",
        )
        .unwrap_err();
        assert!(e.contains("carries only"), "{e}");
    }

    #[test]
    fn argv_form_may_explicitly_select_a_shell() {
        assert!(parse(
            "steps:\n  - id: check\n    cmd: [\"sh\", \"-c\", \"echo $?; echo ${NAME}\"]\n"
        )
        .is_ok());
        assert!(parse(
            "steps:\n  - id: check\n    cmd: [\"cmd\", \"/C\", \"echo %NAME% ^& more\"]\n"
        )
        .is_ok());
    }

    // rev_break #13: an argv form that wraps a shell re-parses its script text,
    // so a template there is rejected like a string-form cmd unless opted in.
    #[test]
    fn argv_wrapped_shell_template_is_rejected_unless_opted_in() {
        let e =
            parse("steps:\n  - id: a\n    cmd: [\"sh\", \"-c\", \"echo {{steps.x.output}}\"]\n")
                .unwrap_err();
        assert!(e.contains("wraps a shell"), "{e}");
        assert!(e.contains("unsafe_shell_template"), "{e}");
        // The opt-in allows it.
        assert!(parse(
            "steps:\n  - id: a\n    unsafe_shell_template: true\n    cmd: [\"sh\", \"-c\", \"echo {{vars.x}}\"]\n"
        )
        .is_ok());
        // A non-wrapping argv expands freely (the recommended path).
        assert!(parse(
            "steps:\n  - id: a\n    cmd: [\"tar\", \"-cf\", \"b.tar\", \"{{steps.x.output}}\"]\n"
        )
        .is_ok());
    }

    // rev_break #12 / rev_regression: bin / cwd / argv[0] are executed-privileged;
    // the validator rejects step-output templates there exactly where the runtime
    // does, so `sfh validate` fails up front instead of after upstream steps ran.
    #[test]
    fn exec_privileged_fields_reject_step_output_at_validation() {
        for yaml in [
            "steps:\n  - id: a\n    cmd: [\"echo\"]\n    bin: \"{{steps.x.output}}\"\n",
            "steps:\n  - id: a\n    cmd: [\"echo\"]\n    cwd: \"{{steps.x.output}}\"\n",
            "steps:\n  - id: a\n    cmd: [\"{{steps.x.output}}\"]\n",
        ] {
            let e = parse(yaml).unwrap_err();
            assert!(e.contains("executed by sfh"), "{yaml}: {e}");
            assert!(e.contains("allow_dynamic_exec_paths"), "{yaml}: {e}");
        }
        // The escape hatch, and a plain (non-run-derived) value, both pass.
        assert!(parse(
            "steps:\n  - id: a\n    allow_dynamic_exec_paths: true\n    cmd: [\"echo\"]\n    cwd: \"{{steps.x.output}}\"\n"
        )
        .is_ok());
        assert!(parse("steps:\n  - id: a\n    cmd: [\"echo\"]\n    cwd: \"/work\"\n").is_ok());
    }

    // rev_regression: load_lenient restores the pre-1.0 rules for an unchanged
    // legacy run - a missing access defaults to write and a string-form cmd may
    // carry a template - where the strict loader rejects both.
    #[test]
    fn lenient_loader_restores_legacy_defaults() {
        let missing_access = "steps:\n  - id: a\n    tool: claude\n    prompt: x\n";
        // Strict (fresh run) rejects the missing access.
        assert!(parse(missing_access).is_err());
        // Lenient (legacy resume) accepts it with a warning.
        let f: Flow = yaml::from_str(missing_access).unwrap();
        assert!(validate(&f, true).is_ok());

        let templated_cmd = "steps:\n  - id: a\n    cmd: \"echo {{steps.x.output}}\"\n";
        // Strict rejects the string-cmd template.
        assert!(parse(templated_cmd).is_err());
        // Lenient accepts it with a warning.
        let f: Flow = yaml::from_str(templated_cmd).unwrap();
        assert!(validate(&f, true).is_ok());
    }

    #[test]
    fn rejects_overlapping_last_line_contains_phrases() {
        let error = parse(
            "steps:\n  - id: choose\n    cmd: echo verdict\n    route:\n      - when_last_line_contains: ACHIEVED\n        goto: end\n      - when_last_line_contains: NOT-ACHIEVED\n        goto: fail\n",
        )
        .unwrap_err();
        assert!(error.contains("substring"), "{error}");
        assert!(error.contains("when_last_line_is"), "{error}");
    }

    #[test]
    fn resolved_tools_only_sees_profiles_the_flow_actually_uses() {
        let f: Flow = yaml::from_str(
            "profiles:\n  aaa-unused: {tool: codex, bin: /tmp/malware}\n  used: {tool: claude, bin: /opt/claude, model: m1, effort: high}\n  fb: {tool: grok, bin: /opt/grok}\nsteps:\n  - id: a\n    use: used\n    fallback: [fb]\n    prompt: x\n  - id: b\n    tool: codex\n    access: read\n    prompt: y\n  - id: c\n    cmd: [\"echo\", \"hi\"]\n  - id: d\n    cmd: [\"echo\", \"text\"]\n    compact: {when_over: 5, tool: opencode, bin: /opt/oc}\n",
        )
        .unwrap();
        let r = f.resolved_tools();
        // Nothing may surface the unused profile's bin: it is data, not an
        // instruction to run anything.
        assert!(
            !r.iter().any(|rt| rt.bin.as_deref() == Some("/tmp/malware")),
            "{r:?}"
        );
        assert!(r.contains(&ResolvedTool {
            tool: "claude".to_string(),
            bin: Some("/opt/claude".into()),
            model: Some("m1".into()),
            effort: Some("high".into()),
            access: vec!["write".into()],
        }));
        assert!(r.contains(&ResolvedTool {
            tool: "grok".to_string(),
            bin: Some("/opt/grok".into()),
            model: None,
            effort: None,
            access: vec!["write".into()],
        }));
        assert!(r.contains(&ResolvedTool {
            tool: "codex".to_string(),
            bin: None,
            model: None,
            effort: None,
            access: vec!["read".into()],
        }));
        // A compact summarizer is a real launch even on a cmd: step.
        assert!(r.contains(&ResolvedTool {
            tool: "opencode".to_string(),
            bin: Some("/opt/oc".into()),
            model: None,
            effort: None,
            // A compact summarizer only ever reads the text it was handed.
            access: vec!["read".into()],
        }));
        assert_eq!(r.len(), 4, "{r:?}");
    }

    /// Upgrading sfh must not, by itself, make every existing run dir
    /// unresumable.
    ///
    /// `--resume` compares an effective-config fingerprint that is a
    /// serialization of `Flow`. Every field added in v1.2 is therefore
    /// `skip_serializing_if`-guarded, so a flow that uses none of them
    /// serializes to exactly the bytes 1.1 produced. This test pins the
    /// property rather than the bytes: no v1.2 key may appear in the projection
    /// of a flow that does not use it.
    #[test]
    fn a_flow_using_no_v1_2_keys_serializes_as_it_did_before_v1_2() {
        let f: Flow = yaml::from_str(
            "name: legacy\nsteps:\n  - id: a\n    cmd: [\"echo\", \"hi\"]\n  - id: b\n    tool: codex\n    access: read\n    prompt: x\n",
        )
        .unwrap();
        let json = f.effective_config_json().unwrap();
        for key in [
            "\"workspace\"",
            "\"contexts\"",
            "\"effects\"",
            "\"context\"",
            "\"context_delivery\"",
            "\"replay\"",
            "\"max_context_chars\"",
            "\"exit_conflict\"",
            "\"outcomes\"",
        ] {
            assert!(
                !json.contains(key),
                "{key} leaked into the fingerprint of a flow that never used it: {json}"
            );
        }
        // And a flow that DOES use them is a different configuration, which is
        // the whole point of the fingerprint.
        let with: Flow = yaml::from_str(
            "name: legacy\nworkspace: {mode: auto}\nsteps:\n  - id: a\n    cmd: [\"echo\", \"hi\"]\n  - id: b\n    tool: codex\n    access: read\n    prompt: x\n",
        )
        .unwrap();
        assert_ne!(json, with.effective_config_json().unwrap());
    }

    #[test]
    fn exit_conflict_distinguishes_saying_nothing_from_saying_fail() {
        let f: Flow = yaml::from_str(
            "defaults: {exit_conflict: trust_protocol}\nsteps:\n  - id: a\n    tool: agy\n    prompt: x\n  - id: b\n    tool: agy\n    prompt: y\n    exit_conflict: fail\n  - id: c\n    tool: pi\n    prompt: z\n",
        )
        .unwrap();
        let by = |id: &str| {
            let s = f.steps.iter().find(|s| s.id == id).unwrap();
            s.exit_conflict(&f)
        };
        assert_eq!(by("a"), Some(ExitConflict::TrustProtocol));
        // A step may pull BACK to strict even where defaults loosened, and that
        // has to be distinguishable from "nothing was declared" - otherwise it
        // could not override agy's own default either.
        assert_eq!(by("b"), Some(ExitConflict::Fail));
        assert_eq!(by("c"), Some(ExitConflict::TrustProtocol));

        let silent: Flow =
            yaml::from_str("steps:\n  - id: a\n    tool: pi\n    prompt: x\n").unwrap();
        assert_eq!(silent.steps[0].exit_conflict(&silent), None);
    }

    #[test]
    fn an_outcomes_table_is_checked_before_anything_runs() {
        let ok: Result<Flow, _> = yaml::from_str(
            "steps:\n  - id: gate\n    cmd: [\"sh\", \"-c\", \"true\"]\n    outcomes:\n      2: {result: continue, label: acceptance_incomplete}\n      10: {result: retryable}\n    route:\n      - {when_label_is: acceptance_incomplete, goto: end}\n      - {when_outcome_is: retryable, goto: fail}\n      - {goto: end}\n",
        );
        assert!(validate(&ok.unwrap(), false).is_ok());

        // Each of these is a rule the author believes in and would never take,
        // or a table entry that cannot mean what it says.
        for (why, src) in [
            (
                "a label no outcome carries",
                "steps:\n  - id: gate\n    cmd: [\"sh\", \"-c\", \"true\"]\n    outcomes:\n      2: {result: continue, label: incomplete}\n    route:\n      - {when_label_is: typo, goto: end}\n      - {goto: end}\n",
            ),
            (
                "an outcome class no entry declares",
                "steps:\n  - id: gate\n    cmd: [\"sh\", \"-c\", \"true\"]\n    outcomes:\n      2: {result: continue}\n    route:\n      - {when_outcome_is: retryable, goto: end}\n      - {goto: end}\n",
            ),
            (
                "retrying a success",
                "steps:\n  - id: gate\n    cmd: [\"sh\", \"-c\", \"true\"]\n    outcomes:\n      0: {result: retryable}\n",
            ),
            (
                "an empty label",
                "steps:\n  - id: gate\n    cmd: [\"sh\", \"-c\", \"true\"]\n    outcomes:\n      2: {result: continue, label: \"  \"}\n",
            ),
            (
                "sfh's own no-process marker",
                "steps:\n  - id: gate\n    cmd: [\"sh\", \"-c\", \"true\"]\n    outcomes:\n      -1: {result: complete}\n",
            ),
        ] {
            let f: Flow = yaml::from_str(src).expect("fixture parses");
            assert!(validate(&f, false).is_err(), "{why} must be refused");
        }
    }

    #[test]
    fn a_fan_out_step_has_no_outcome_of_its_own_to_route_on() {
        // The trap this closes: the group records a COMPOSITE exit and no
        // label, so a `when_label_is` rule whose label really is declared
        // passes the "can it ever match" check and then never fires. Both
        // halves of the answer were wrong at once.
        let group = parse(
            "name: t\nsteps:\n  - id: fan\n    outcomes:\n      2: {result: continue, label: partial}\n    parallel:\n      - {id: a, cmd: [\"echo\", \"hi\"]}\n",
        )
        .unwrap_err();
        assert!(group.contains("carries only"), "{group}");

        // A foreach's table describes each ITEM's launch, which is useful, so
        // the table stays - only the rule that reads the group is refused.
        let rule = parse(
            "name: t\nvars: {i: \"a\"}\nsteps:\n  - id: each\n    foreach: {from: \"{{vars.i}}\", split: lines}\n    cmd: [\"echo\", \"{{item}}\"]\n    outcomes:\n      2: {result: continue, label: partial}\n    route:\n      - {when_label_is: partial, goto: end}\n      - {goto: end}\n",
        )
        .unwrap_err();
        assert!(rule.contains("group composite"), "{rule}");
        assert!(parse(
            "name: t\nvars: {i: \"a\"}\nsteps:\n  - id: each\n    foreach: {from: \"{{vars.i}}\", split: lines}\n    cmd: [\"echo\", \"{{item}}\"]\n    outcomes:\n      2: {result: continue, label: partial}\n",
        )
        .is_ok());
    }

    #[test]
    fn an_outcome_class_says_only_whether_to_carry_on_retry_or_stop() {
        // The vocabulary stays tiny and domain-free on purpose: everything
        // domain-shaped belongs in the label, which sfh never interprets.
        assert!(OutcomeResult::Complete.is_success());
        assert!(OutcomeResult::Continue.is_success());
        assert!(!OutcomeResult::Retryable.is_success());
        assert!(!OutcomeResult::Fail.is_success());
        assert_eq!(OutcomeResult::default(), OutcomeResult::Fail);
        for r in [
            OutcomeResult::Complete,
            OutcomeResult::Continue,
            OutcomeResult::Retryable,
            OutcomeResult::Fail,
        ] {
            let round: OutcomeResult = yaml::from_str(r.as_str()).expect("round-trips");
            assert_eq!(round, r);
        }
    }

    #[test]
    fn trusting_a_protocol_over_an_exit_code_is_listed_as_an_override() {
        let f: Flow = yaml::from_str(
            "steps:\n  - id: a\n    tool: pi\n    prompt: x\n    exit_conflict: trust_protocol\n  - id: b\n    tool: pi\n    prompt: y\n    exit_conflict: fail\n",
        )
        .unwrap();
        let o = f.unsafe_overrides();
        assert!(
            o.contains(&"steps.a.exit_conflict=trust_protocol".to_string()),
            "{o:?}"
        );
        // Declaring the strict default is not an override of anything.
        assert!(!o.iter().any(|s| s.starts_with("steps.b.")), "{o:?}");
    }

    #[test]
    fn every_program_a_cmd_step_launches_is_visible_to_preflight() {
        let f: Flow = yaml::from_str(
            "steps:\n  - id: verify\n    cmd: [\"bash\", \"-lc\", \"cargo test\"]\n  - id: fan\n    parallel:\n      - id: fmt\n        cmd: [\"bash\", \"-lc\", \"cargo fmt --check\"]\n      - id: node\n        cmd: [\"pnpm\", \"test\"]\n  - id: agent\n    tool: codex\n    prompt: x\n  - id: line\n    cmd: \"echo hi\"\n",
        )
        .unwrap();
        let c = f.resolved_commands();
        // Both steps that launch bash are attributed to it, including the one
        // nested in a parallel group - the shape that hid a bad `bin` before.
        assert_eq!(
            c.get("bash").map(|s| s.iter().cloned().collect::<Vec<_>>()),
            Some(vec!["fmt".to_string(), "verify".to_string()])
        );
        assert!(c.contains_key("pnpm"));
        // A preset step launches no command, and a `cmd:` STRING is run by the
        // platform shell rather than by anything the flow named.
        assert!(!c.contains_key("codex"));
        assert!(c.contains_key(if cfg!(windows) { "cmd" } else { "sh" }));
    }

    #[test]
    fn effective_fingerprint_ignores_only_unreferenced_profiles() {
        let mut flow: Flow = yaml::from_str(
            "api_version: 1\nprofiles:\n  used: {tool: claude, access: read}\n  fallback: {tool: grok, access: read}\n  compact: {tool: opencode, access: read}\n  unrelated: {tool: codex, access: read, model: old, env: {API_TOKEN: supersecret}}\nsteps:\n  - id: work\n    use: used\n    fallback: [fallback]\n    prompt: x\n    compact: {when_over: 100, use: compact}\n",
        )
        .unwrap();
        let original = flow.effective_config_json().unwrap();

        flow.profiles.get_mut("unrelated").unwrap().model = Some("new".into());
        assert_eq!(
            flow.effective_config_json().unwrap(),
            original,
            "an unrelated global profile must not make this flow unresumable"
        );

        flow.profiles.get_mut("fallback").unwrap().model = Some("new".into());
        assert_ne!(
            flow.effective_config_json().unwrap(),
            original,
            "fallback and compact profiles are execution-relevant too"
        );

        let shown = flow.effective_config_json_pretty(false).unwrap();
        assert!(
            shown.contains("\"unrelated\""),
            "config show should retain the complete merge result"
        );
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(!shown.contains("supersecret"), "{shown}");
        assert!(
            flow.effective_config_json_pretty(true)
                .unwrap()
                .contains("supersecret"),
            "--show-secrets must be the explicit way to inspect env values"
        );
    }

    #[test]
    fn identifies_unterminated_consecutive_branches() {
        let flow: Flow = yaml::from_str(
            "steps:\n  - id: choose\n    cmd: echo verdict\n    route:\n      - {when_last_line_is: MET, goto: met}\n      - {when_last_line_is: UNMET, goto: unmet}\n      - {when_last_line_is: UNCLEAR, goto: unclear}\n  - id: met\n    cmd: echo met\n  - id: unmet\n    cmd: echo unmet\n    route: [{goto: end}]\n  - id: unclear\n    cmd: echo unclear\n",
        )
        .unwrap();
        let warnings = runtime_warnings(&flow);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("step 'met'"), "{warnings:?}");
        assert!(warnings[0].contains("step 'unmet'"), "{warnings:?}");
    }

    #[test]
    fn validate_name_allows_unicode_spaces_and_dots() {
        assert!(validate_name("研究 2026.07").is_ok());
        assert!(validate_name("my flow v2.1").is_ok());
        assert!(validate_name("hello world").is_ok());
        assert!(validate_name("a.b.c").is_ok());
        assert!(validate_name("日本語テスト").is_ok());
    }

    #[test]
    fn validate_name_rejects_path_separators_and_traversal() {
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("a\0b").is_err());
        assert!(validate_name("a<b").is_err());
        assert!(validate_name("a>b").is_err());
        assert!(validate_name("a:b").is_err());
        assert!(validate_name("a\"b").is_err());
        assert!(validate_name("a|b").is_err());
        assert!(validate_name("a?b").is_err());
        assert!(validate_name("a*b").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("  ").is_err());
        assert!(validate_name("trailing ").is_err());
        assert!(validate_name("trailing.").is_err());
    }

    #[test]
    fn validate_calls_validate_name() {
        let e = parse("name: \"a/b\"\nsteps:\n  - id: a\n    cmd: echo hi\n").unwrap_err();
        assert!(e.contains("path separators"), "{e}");
        assert!(parse("name: \"研究 2026.07\"\nsteps:\n  - id: a\n    cmd: echo hi\n").is_ok());
    }

    #[test]
    fn api_version_and_all_terminal_ids_are_unambiguous() {
        assert!(parse(
            "api_version: 1\nname: t\nsteps:\n  - id: work\n    cmd: [\"echo\", \"ok\"]\n"
        )
        .is_ok());
        let error =
            parse("api_version: 2\nname: t\nsteps:\n  - id: work\n    cmd: [\"echo\", \"ok\"]\n")
                .unwrap_err();
        assert!(error.contains("api_version"), "{error}");

        for id in ["end", "END", "Fail", "STUCK"] {
            let error = parse(&format!(
                "api_version: 1\nsteps:\n  - id: {id}\n    cmd: [\"echo\", \"ok\"]\n"
            ))
            .unwrap_err();
            assert!(error.contains("is reserved"), "{id}: {error}");
        }
    }

    #[test]
    fn rejects_catch_all_routes_that_hide_later_rules() {
        let error = parse(
            "api_version: 1\nsteps:\n  - id: choose\n    cmd: [\"echo\", \"ok\"]\n    route:\n      - {goto: end}\n      - {when_last_line_is: ok, goto: fail}\n",
        )
        .unwrap_err();
        assert!(error.contains("catch-all"), "{error}");
        assert!(error.contains("last"), "{error}");
    }

    #[test]
    fn code_validation_matches_schema_minima_and_tool_vocabulary() {
        for (field, value) in [
            ("timeout_sec", "0"),
            ("max_visits", "0"),
            ("max_total_steps", "0"),
            ("max_parallel", "0"),
            ("max_prompt_chars", "0"),
            ("max_emit_chars", "0"),
            ("wall_clock_sec", "0"),
        ] {
            let error = parse(&format!(
                "api_version: 1\ndefaults:\n  {field}: {value}\nsteps:\n  - id: work\n    cmd: [\"echo\", \"ok\"]\n"
            ))
            .unwrap_err();
            assert!(error.contains(field), "{field}: {error}");
        }
        let error = parse(
            "api_version: 1\ndefaults:\n  tool: imaginary\nsteps:\n  - id: work\n    prompt: x\n    access: read\n",
        )
        .unwrap_err();
        assert!(error.contains("unknown tool"), "{error}");
        let error = parse(
            "api_version: 1\ndefaults:\n  tool_max_parallel: {imaginary: 1}\nsteps:\n  - id: work\n    cmd: [\"echo\", \"ok\"]\n",
        )
        .unwrap_err();
        assert!(error.contains("tool_max_parallel"), "{error}");
        assert!(error.contains("unknown preset tool"), "{error}");
    }

    #[test]
    fn fallback_profiles_have_stable_names_artifacts_and_access() {
        let case_collision = parse(
            "api_version: 1\nprofiles:\n  Backup: {tool: claude, access: read}\n  backup: {tool: claude, access: read}\nsteps:\n  - id: work\n    use: Backup\n    prompt: x\n",
        )
        .unwrap_err();
        assert!(
            case_collision.contains("duplicate profile name"),
            "{case_collision}"
        );

        let repeated = parse(
            "api_version: 1\nprofiles:\n  primary: {tool: claude, access: read}\n  backup: {tool: claude, access: read}\nsteps:\n  - id: work\n    use: primary\n    fallback: [backup, backup]\n    prompt: x\n",
        )
        .unwrap_err();
        assert!(repeated.contains("listed more than once"), "{repeated}");

        let missing_access = parse(
            "api_version: 1\nprofiles:\n  primary: {tool: claude, access: read}\n  backup: {tool: claude}\nsteps:\n  - id: work\n    use: primary\n    fallback: [backup]\n    prompt: x\n",
        )
        .unwrap_err();
        assert!(
            missing_access.contains("fallback profile 'backup'")
                && missing_access.contains("access"),
            "{missing_access}"
        );
    }

    #[test]
    fn session_sources_must_dominate_and_must_fail_closed() {
        let future = parse(
            "api_version: 1\nsteps:\n  - id: consumer\n    tool: claude\n    access: read\n    continue_from: source\n    prompt: continue\n  - id: source\n    tool: claude\n    access: read\n    prompt: start\n",
        )
        .unwrap_err();
        assert!(future.contains("not guaranteed to run before"), "{future}");

        let fail_open = parse(
            "api_version: 1\nsteps:\n  - id: source\n    tool: claude\n    access: read\n    on_error: continue\n    prompt: start\n  - id: consumer\n    tool: claude\n    access: read\n    continue_from: source\n    prompt: continue\n",
        )
        .unwrap_err();
        assert!(fail_open.contains("may fail and continue"), "{fail_open}");

        let group_fail_open = parse(
            "api_version: 1\nsteps:\n  - id: source_group\n    on_error: continue\n    parallel:\n      - id: source\n        tool: claude\n        access: read\n        prompt: start\n  - id: consumer\n    tool: claude\n    access: read\n    continue_from: source\n    prompt: continue\n",
        )
        .unwrap_err();
        assert!(
            group_fail_open.contains("parallel group 'source_group'")
                && group_fail_open.contains("without creating a session"),
            "{group_fail_open}"
        );

        let provider_changes = parse(
            "api_version: 1\nprofiles:\n  primary: {tool: claude, access: read}\n  backup: {tool: codex, access: read}\nsteps:\n  - id: source\n    use: primary\n    fallback: [backup]\n    prompt: start\n  - id: consumer\n    tool: claude\n    access: read\n    continue_from: source\n    prompt: continue\n",
        )
        .unwrap_err();
        assert!(
            provider_changes.contains("no stable session provider"),
            "{provider_changes}"
        );
    }

    #[test]
    fn step_templates_require_dominance_unless_explicitly_optional() {
        let future = parse(
            "api_version: 1\nsteps:\n  - id: consumer\n    tool: claude\n    access: read\n    prompt: '{{steps.source.output}}'\n  - id: source\n    cmd: [\"echo\", \"answer\"]\n",
        )
        .unwrap_err();
        assert!(future.contains("does not dominate"), "{future}");

        let branched = "api_version: 1\nsteps:\n  - id: choose\n    cmd: [\"echo\", \"pick\"]\n    route:\n      - {when_last_line_is: source, goto: source}\n      - {goto: join}\n  - id: source\n    cmd: [\"echo\", \"answer\"]\n    route: [{goto: join}]\n  - id: join\n    tool: claude\n    access: read\n    prompt: '{{steps.source.output | optional}}'\n";
        assert!(parse(branched).is_ok());
        let required = branched.replace(" | optional", "");
        let error = parse(&required).unwrap_err();
        assert!(error.contains("does not dominate"), "{error}");
    }

    #[test]
    fn template_dominance_checks_only_effective_merged_settings() {
        let overridden = "api_version: 1
defaults:
  cwd: '{{steps.branch_only.output}}'
  env:
    ANSWER: '{{steps.branch_only.output}}'
profiles:
  primary:
    tool: claude
    access: read
    cwd: '{{steps.branch_only.output}}'
    env:
      ANSWER: '{{steps.branch_only.output}}'
steps:
  - id: choose
    cmd: [\"echo\", \"pick\"]
    cwd: .
    env: {ANSWER: fixed}
    route:
      - {when_last_line_is: branch, goto: branch_only}
      - {goto: consumer}
  - id: branch_only
    cmd: [\"echo\", \"answer\"]
    cwd: .
    env: {ANSWER: fixed}
    route: [{goto: consumer}]
  - id: consumer
    use: primary
    cwd: .
    env:
      ANSWER: fixed
    prompt: work
";
        let parsed = parse(overridden);
        assert!(
            parsed.is_ok(),
            "overridden profile/default templates are not runtime dependencies: {:?}",
            parsed.err()
        );

        let active = overridden.replacen(
            "  - id: consumer\n    use: primary\n    cwd: .\n    env:\n      ANSWER: fixed\n",
            "  - id: consumer\n    use: primary\n    cwd: .\n",
            1,
        );
        let error = parse(&active).unwrap_err();
        assert!(error.contains("does not dominate"), "{error}");

        let fallback = overridden.replace(
            "  primary:\n    tool: claude\n",
            "  fallback:\n    tool: claude\n    access: read\n    model: '{{steps.branch_only.output}}'\n  primary:\n    tool: claude\n",
        )
        .replace("    use: primary\n", "    use: primary\n    fallback: [fallback]\n");
        let error = parse(&fallback).unwrap_err();
        assert!(error.contains("does not dominate"), "{error}");
    }

    #[test]
    fn strict_mode_exposes_implicit_and_unreachable_control_flow() {
        let flow: Flow = yaml::from_str(
            "steps:\n  - id: first\n    cmd: [\"echo\", \"x\"]\n    route:\n      - {when_last_line_is: x, goto: end}\n  - id: unreachable\n    cmd: [\"echo\", \"never\"]\n",
        )
        .unwrap();
        validate(&flow, false).unwrap();
        let warnings = strict_warnings(&flow).join("\n");
        assert!(warnings.contains("api_version is omitted"), "{warnings}");
        assert!(warnings.contains("no catch-all"), "{warnings}");
    }
}
