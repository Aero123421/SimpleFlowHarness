use serde::Deserialize;
use serde_yaml_ng as yaml;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flow {
    pub name: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, yaml::Value>,
    #[serde(default)]
    pub defaults: Defaults,
    /// Named bundles of tool settings referenced by steps via `use:`.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
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

#[derive(Deserialize, Default)]
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
}

/// How much of each ceiling `on_budget` keeps back for the landing chain. The
/// landing threshold is `ceiling - reserve` on each axis INDEPENDENTLY: cost
/// and wall-clock never borrow from one another.
#[derive(Deserialize, Default, Clone, Copy)]
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

#[derive(Deserialize, Default, Clone)]
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

#[derive(Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    /// Extra attempts after the first (0 = no retry).
    pub max: u32,
    /// First backoff delay; doubles each attempt (default 5).
    pub backoff_sec: Option<u64>,
}

#[derive(Deserialize)]
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
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub env_remove: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Foreach {
    /// Template rendered, then split into items.
    pub from: String,
    /// lines (default) | json | separator:<sep>
    pub split: Option<String>,
}

#[derive(Deserialize)]
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

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Cmd {
    Shell(String),
    Argv(Vec<String>),
}

/// One `route:` rule. Every `when_*` present in a rule must hold (AND); a rule
/// with none is the catch-all. Adding one here means adding it to four other
/// places: schema/flow.schema.json, the catch-all test in `evaluate_route`
/// (a rule with a condition must not log as `via: catch_all`), any exclusivity
/// check that enumerates predicates, and - if its text is templated -
/// `engine::precheck`'s route-condition list. The precheck one is the easiest
/// to miss and the most expensive: without it a template typo survives validate
/// and dry-run and only kills the run after the guarded step has been billed
/// (that is exactly what happened to `when_stderr_matches`).
#[derive(Deserialize)]
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
    /// Count the members of THIS step's fan-out that reported a given verdict.
    /// Only on a `parallel:`/`foreach:` step, and only alone in its rule (see
    /// validate). See `WhenMembers`.
    pub when_members: Option<WhenMembers>,
    /// Step id, or one of the three terminals: "end" (finish, success),
    /// "fail" (finish, failure) or "stuck" (finish, needs a human - exit 4).
    pub goto: String,
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
#[derive(Deserialize, Clone)]
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
/// Only `stuck` is ALSO refused as a step id (see validate). `end` and `fail`
/// have shadowed a same-named step since v0.1 - a flow that has one is already
/// written around it, and turning that into a hard error now would break
/// working flows for no gain. `stuck` is new, so it is reserved before anyone
/// can write a flow that depends on the ambiguity.
pub const TERMINALS: [&str; 3] = ["end", "fail", "stuck"];

pub const TOOLS: [&str; 7] = ["codex", "claude", "opencode", "grok", "agy", "pi", "cursor"];

/// One concrete way a flow can launch a preset tool, as collected by
/// `Flow::resolved_tools`. Ordered so a BTreeSet dedupes and sorts it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedTool {
    pub tool: String,
    pub bin: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

pub fn load(path: &Path) -> Result<Flow, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut flow: Flow = yaml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    merge_global_profiles(&mut flow);
    validate(&flow, false)?;
    Ok(flow)
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
    merge_global_profiles(&mut flow);
    validate(&flow, true)?;
    flow.legacy_resume = true;
    Ok(flow)
}

/// Merge machine-level profiles from ~/.sfh/profiles.yaml (a bare name->profile map).
/// Flow-level profiles win on name conflicts. This keeps flow files portable while
/// machine-specific things (bin: paths, provider/model choices) live outside the repo.
fn merge_global_profiles(flow: &mut Flow) {
    let Some(p) = global_profiles_path() else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(&p) else {
        return;
    };
    match yaml::from_str::<BTreeMap<String, Profile>>(&text) {
        Ok(globals) => {
            for (k, v) in globals {
                flow.profiles.entry(k).or_insert(v);
            }
        }
        Err(e) => eprintln!("sfh: warning: ignoring {}: {e}", p.display()),
    }
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
    if flow.steps.is_empty() {
        return Err("flow has no steps".into());
    }
    if let Some(n) = &flow.name {
        validate_name(n)?;
    }
    let mut seen = HashSet::new();
    // Compared ignoring case for the same reason ids are: `Stuck` and `stuck`
    // are one name on Windows and macOS, so a rule that only caught the exact
    // spelling would let the ambiguity back in on two of the three OSes.
    let reserved_id = |id: &str| -> Result<(), String> {
        if id.eq_ignore_ascii_case("stuck") {
            return Err(format!(
                "step id '{id}' is reserved: 'stuck' is a goto target that ends the run for a human to look at (exit 4), so it cannot also name a step (compared ignoring case). Rename the step"
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
    for (name, p) in &flow.profiles {
        if !valid_id(name) {
            return Err(format!("profile name '{name}' must use only [A-Za-z0-9_-]"));
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
    }
    if let Some(cost) = flow.defaults.max_cost_usd {
        if !(cost.is_finite() && cost >= 0.0) {
            return Err(format!(
                "defaults.max_cost_usd must be a finite number >= 0 (got {cost})"
            ));
        }
    }
    for (tool, limit) in &flow.defaults.tool_max_parallel {
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
        for (i, r) in s.route.iter().enumerate() {
            check_goto(&format!("step '{}' route[{i}]", s.id), &r.goto)?;
            check_when_members(s, i, r)?;
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
    for warning in branch_fallthrough_warnings(flow) {
        eprintln!("sfh: warning: {warning}");
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

fn branch_fallthrough_warnings(flow: &Flow) -> Vec<String> {
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
        }));
        assert!(r.contains(&ResolvedTool {
            tool: "grok".to_string(),
            bin: Some("/opt/grok".into()),
            model: None,
            effort: None,
        }));
        assert!(r.contains(&ResolvedTool {
            tool: "codex".to_string(),
            bin: None,
            model: None,
            effort: None,
        }));
        // A compact summarizer is a real launch even on a cmd: step.
        assert!(r.contains(&ResolvedTool {
            tool: "opencode".to_string(),
            bin: Some("/opt/oc".into()),
            model: None,
            effort: None,
        }));
        assert_eq!(r.len(), 4, "{r:?}");
    }

    #[test]
    fn identifies_unterminated_consecutive_branches() {
        let flow: Flow = yaml::from_str(
            "steps:\n  - id: choose\n    cmd: echo verdict\n    route:\n      - {when_last_line_is: MET, goto: met}\n      - {when_last_line_is: UNMET, goto: unmet}\n      - {when_last_line_is: UNCLEAR, goto: unclear}\n  - id: met\n    cmd: echo met\n  - id: unmet\n    cmd: echo unmet\n    route: [{goto: end}]\n  - id: unclear\n    cmd: echo unclear\n",
        )
        .unwrap();
        let warnings = branch_fallthrough_warnings(&flow);
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
}
